#!/usr/bin/env python3
"""Validate repository-local Markdown paths and heading anchors."""

from __future__ import annotations

import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parents[1]
INLINE_LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
REFERENCE_LINK = re.compile(r"^\s*\[[^\]]+\]:\s*(\S+)", re.MULTILINE)
HEADING = re.compile(r"^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$")
TITLE = re.compile(r"\s+[\"'][^\"']*[\"']$")
HTML_TAG = re.compile(r"<[^>]+>")
EXTERNAL_SCHEMES = ("http:", "https:", "mailto:", "tel:", "data:")


def markdown_files() -> list[Path]:
    result = subprocess.run(
        [
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.md",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return sorted(
        {
            Path(line.strip())
            for line in result.stdout.splitlines()
            if line.strip() and (ROOT / line.strip()).is_file()
        },
        key=lambda path: path.as_posix(),
    )


def heading_anchors(path: Path) -> set[str]:
    counts: defaultdict[str, int] = defaultdict(int)
    anchors: set[str] = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        match = HEADING.match(line)
        if match is None:
            continue
        heading = HTML_TAG.sub("", match.group(1)).lower()
        base = "".join(
            character
            for character in heading
            if character.isalnum() or character in " _-"
        ).strip()
        base = re.sub(r"[\s_]+", "-", base)
        base = re.sub(r"-+", "-", base)
        count = counts[base]
        counts[base] += 1
        anchors.add(base if count == 0 else f"{base}-{count}")
    return anchors


def validate() -> tuple[list[Path], list[str]]:
    files = markdown_files()
    failures: list[str] = []
    anchor_cache: dict[Path, set[str]] = {}
    for relative in files:
        source = ROOT / relative
        text = source.read_text(encoding="utf-8")
        targets = INLINE_LINK.findall(text) + REFERENCE_LINK.findall(text)
        for raw in targets:
            target = TITLE.sub("", raw.strip()).removeprefix("<").removesuffix(">")
            if not target or target.startswith(EXTERNAL_SCHEMES):
                continue
            path_part, separator, anchor = target.partition("#")
            decoded_path = unquote(path_part.partition("?")[0])
            destination = (
                source if not decoded_path else (source.parent / decoded_path).resolve()
            )
            if not destination.exists():
                failures.append(f"{relative.as_posix()}: missing {target}")
                continue
            if not separator or not anchor or destination.suffix.lower() != ".md":
                continue
            destination = destination.resolve()
            anchors = anchor_cache.get(destination)
            if anchors is None:
                anchors = heading_anchors(destination)
                anchor_cache[destination] = anchors
            decoded_anchor = unquote(anchor).lower()
            if decoded_anchor not in anchors:
                try:
                    shown_destination = destination.relative_to(ROOT).as_posix()
                except ValueError:
                    shown_destination = str(destination)
                failures.append(
                    f"{relative.as_posix()}: missing anchor #{anchor} in {shown_destination}"
                )
    return files, failures


def main() -> int:
    try:
        files, failures = validate()
    except (OSError, UnicodeError, subprocess.CalledProcessError) as error:
        print(f"Markdown validation failed: {error}", file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"Markdown links valid: {len(files)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
