#!/usr/bin/env python3
"""Reject GitHub workflow actions that are not pinned to exact commits."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
USES_LINE = re.compile(r"^\s*(?:-\s*)?uses:\s*[\"']?([^\s\"'#]+)")
PINNED_ACTION = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")


def validate(root: Path = ROOT) -> tuple[int, list[str]]:
    workflow_root = root / ".github/workflows"
    workflows = sorted((*workflow_root.glob("*.yml"), *workflow_root.glob("*.yaml")))
    checked = 0
    failures: list[str] = []
    for path in workflows:
        relative = path.relative_to(root).as_posix()
        for line_number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), start=1
        ):
            match = USES_LINE.match(line)
            if match is None:
                continue
            action = match.group(1)
            if action.startswith("./"):
                continue
            checked += 1
            if not PINNED_ACTION.fullmatch(action):
                failures.append(
                    f"{relative}:{line_number}: external action is not pinned to "
                    f"a lowercase 40-character commit SHA: {action}"
                )
    if not workflows:
        failures.append(".github/workflows contains no workflow files")
    return checked, failures


def main() -> int:
    try:
        checked, failures = validate()
    except (OSError, UnicodeError) as error:
        print(f"Workflow action validation failed: {error}", file=sys.stderr)
        return 1
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"Workflow action pins valid: {checked} external uses")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
