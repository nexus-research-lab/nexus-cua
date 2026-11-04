#!/usr/bin/env python3
"""Package one built nexuscua runtime as a publishable release archive.

The archive layout, manifest contract and asset names are consumed by host
installers (for example Nexus), which bind each archive to a digest reviewed in
their own source. Packaging never downloads anything: license texts come from
the locally vendored Cargo registry sources, and crates that ship no license
file are recorded with their declared expression and repository instead.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import time
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = "nexus.cua.release.v1"
PROTOCOL_VERSION = "nexus.cua.v1"
PACKAGE = "nexus-cua"
BINARY = "nexuscua"
BUILD_COMMAND = "cargo build --release --locked --package nexus-cua --bin nexuscua"
PLATFORMS = {"macos", "windows"}
ARCHITECTURES = {"arm64", "x86_64"}
SIGNING_KINDS = {
    "developer-id-notarized": True,
    "authenticode": True,
    "ad-hoc": False,
    "unsigned": False,
}
LICENSE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")
SEMVER = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?$")
REVISION = re.compile(r"^[0-9a-f]{40}$")
TREE_LINE = re.compile(r"^(?P<name>[A-Za-z0-9_-]+) v(?P<version>\S+)(?P<rest>(?: \([^)]*\))*)$")
TREE_GROUP = re.compile(r"\(([^)]*)\)")
# Bounded so a broken dependency graph cannot produce an unbounded archive.
MAX_DEPENDENCIES = 1024
MAX_LICENSE_BYTES = 1 << 20


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def workspace_version(cargo_manifest: Path = ROOT / "Cargo.toml") -> str:
    match = re.search(r'(?m)^version = "([^"]+)"$', cargo_manifest.read_text(encoding="utf-8"))
    if match is None:
        raise ValueError("Cargo.toml has no workspace version")
    return match.group(1)


def version_from_tag(tag: str) -> str:
    if not tag.startswith("v") or not SEMVER.fullmatch(tag[1:]):
        raise ValueError(f"release tag must be v<semver>, got {tag!r}")
    return tag[1:]


def archive_base(version: str, platform: str, architecture: str) -> str:
    return f"{BINARY}-{version}-{platform}-{architecture}"


def parse_cargo_tree(output: str) -> list[tuple[str, str]]:
    """Return unique registry (name, version) pairs, excluding path crates."""
    seen: dict[tuple[str, str], None] = {}
    for raw in output.splitlines():
        line = raw.strip()
        if not line:
            continue
        match = TREE_LINE.match(line)
        if match is None:
            raise ValueError(f"unexpected cargo tree line: {line!r}")
        groups = [group for group in TREE_GROUP.findall(match.group("rest")) if group != "*"]
        if "proc-macro" in groups:
            continue  # runs at compile time; no code is linked into the runtime
        if any("://" not in group for group in groups):
            continue  # workspace member or other local path crate
        seen[(match.group("name"), match.group("version"))] = None
    if len(seen) > MAX_DEPENDENCIES:
        raise ValueError(f"dependency closure exceeds {MAX_DEPENDENCIES} packages")
    return list(seen)


def license_files(package_dir: Path, license_file: str | None) -> list[Path]:
    files = []
    if package_dir.is_dir():
        for entry in sorted(package_dir.iterdir()):
            if entry.is_file() and entry.name.upper().startswith(LICENSE_PREFIXES):
                files.append(entry)
    if license_file:
        declared = (package_dir / license_file).resolve()
        if declared.is_file() and declared.is_relative_to(package_dir.resolve()) and declared not in files:
            files.append(declared)
    return files


def collect_licenses(dependencies: list[tuple[str, str]], metadata: dict, destination: Path) -> list[dict]:
    """Copy each dependency's shipped license texts and return SBOM records."""
    packages = {(item["name"], item["version"]): item for item in metadata["packages"]}
    records = []
    for name, version in sorted(dependencies):
        package = packages.get((name, version))
        if package is None:
            raise ValueError(f"cargo metadata is missing {name} {version}")
        package_dir = Path(package["manifest_path"]).parent
        target = destination / f"{name}-{version}"
        target.mkdir(parents=True, exist_ok=False)
        copied = 0
        for path in license_files(package_dir, package.get("license_file")):
            if path.stat().st_size > MAX_LICENSE_BYTES:
                raise ValueError(f"license file is too large: {path}")
            shutil.copyfile(path, target / path.name)
            copied += 1
        if copied == 0:
            (target / "SOURCE.txt").write_text(
                "This crate ships no license file in its published package.\n"
                f"Declared license: {package.get('license') or 'NOASSERTION'}\n"
                f"Repository: {package.get('repository') or 'NOASSERTION'}\n",
                encoding="utf-8",
            )
        records.append(
            {
                "name": name,
                "version": version,
                "license": package.get("license") or "NOASSERTION",
                "source": package.get("source") or "",
            }
        )
    return records


def lock_checksums(lock_path: Path = ROOT / "Cargo.lock") -> dict[tuple[str, str], str]:
    checksums: dict[tuple[str, str], str] = {}
    for block in lock_path.read_text(encoding="utf-8").split("[[package]]"):
        name = re.search(r'(?m)^name = "([^"]+)"$', block)
        version = re.search(r'(?m)^version = "([^"]+)"$', block)
        checksum = re.search(r'(?m)^checksum = "([0-9a-f]{64})"$', block)
        if name and version and checksum:
            checksums[(name.group(1), version.group(1))] = checksum.group(1)
    return checksums


def spdx_id(name: str, version: str) -> str:
    return "SPDXRef-" + re.sub(r"[^A-Za-z0-9.-]", "-", f"{name}-{version}")


def build_sbom(version: str, revision: str, records: list[dict], checksums: dict, created_at: str) -> dict:
    root_id = spdx_id(PACKAGE, version)
    packages = [
        {
            "name": PACKAGE,
            "SPDXID": root_id,
            "versionInfo": version,
            "downloadLocation": f"git+https://github.com/nexus-research-lab/nexus-cua@{revision}",
            "filesAnalyzed": False,
            "licenseConcluded": "MIT",
            "licenseDeclared": "MIT",
            "copyrightText": "NOASSERTION",
        }
    ]
    relationships = [
        {"spdxElementId": "SPDXRef-DOCUMENT", "relationshipType": "DESCRIBES", "relatedSpdxElement": root_id}
    ]
    for record in records:
        name, dependency_version = record["name"], record["version"]
        package = {
            "name": name,
            "SPDXID": spdx_id(name, dependency_version),
            "versionInfo": dependency_version,
            "downloadLocation": (
                f"https://crates.io/api/v1/crates/{name}/{dependency_version}/download"
                if record["source"].startswith("registry+")
                else "NOASSERTION"
            ),
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": record["license"].replace("/", " OR "),
            "copyrightText": "NOASSERTION",
        }
        checksum = checksums.get((name, dependency_version))
        if checksum:
            package["checksums"] = [{"algorithm": "SHA256", "checksumValue": checksum}]
        packages.append(package)
        relationships.append(
            {"spdxElementId": root_id, "relationshipType": "DEPENDS_ON", "relatedSpdxElement": package["SPDXID"]}
        )
    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": f"{PACKAGE}-{version}",
        "documentNamespace": f"https://github.com/nexus-research-lab/nexus-cua/sbom/{version}/{revision}",
        "creationInfo": {
            "created": created_at,
            "creators": ["Organization: Nexus Research Lab", "Tool: scripts/package_release.py"],
            "comment": "Cargo normal-dependency closure of the nexuscua binary for this target.",
        },
        "packages": packages,
        "relationships": relationships,
    }


def build_manifest(
    *,
    version: str,
    revision: str,
    platform: str,
    architecture: str,
    target: str,
    toolchain: str,
    signing_kind: str,
    runtime_name: str,
    runtime_sha256: str,
    sbom_sha256: str,
    notices_sha256: str,
    created_at: str,
) -> dict:
    if signing_kind not in SIGNING_KINDS:
        raise ValueError(f"unsupported signing kind: {signing_kind}")
    return {
        "contract": CONTRACT,
        "version": version,
        "protocol_version": PROTOCOL_VERSION,
        "source_revision": revision,
        "platform": platform,
        "architecture": architecture,
        "release_state": "prerelease" if "-" in version else "published",
        "signing": {"kind": signing_kind, "production_signed": SIGNING_KINDS[signing_kind]},
        "build": {"command": BUILD_COMMAND, "target": target, "toolchain": toolchain},
        "files": {
            "runtime": {"path": runtime_name, "sha256": runtime_sha256},
            "sbom": {"path": "sbom.spdx.json", "sha256": sbom_sha256},
            "notices": {"path": "THIRD_PARTY_NOTICES.md", "sha256": notices_sha256},
        },
        "created_at": created_at,
    }


def write_archive(stage: Path, output: Path) -> None:
    """Write a deterministic archive whose single top-level directory is `stage`."""
    entries = sorted(path for path in stage.rglob("*"))
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "0")) or 315532800  # 1980-01-01, zip minimum
    if output.name.endswith(".zip"):
        with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for path in entries:
                if path.is_dir():
                    continue
                info = zipfile.ZipInfo(
                    str(Path(stage.name) / path.relative_to(stage)).replace("\\", "/"),
                    date_time=time.gmtime(epoch)[:6],
                )
                info.external_attr = (0o755 if path.name == f"{BINARY}.exe" else 0o644) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, path.read_bytes())
        return

    def normalize(info: tarfile.TarInfo) -> tarfile.TarInfo:
        info.uid = info.gid = 0
        info.uname = info.gname = ""
        info.mtime = epoch
        if info.isdir() or Path(info.name).name == BINARY:
            info.mode = 0o755
        else:
            info.mode = 0o644
        return info

    with tarfile.open(output, "w:gz", format=tarfile.PAX_FORMAT) as archive:
        archive.add(stage, arcname=stage.name, recursive=False, filter=normalize)
        for path in entries:
            archive.add(path, arcname=str(Path(stage.name) / path.relative_to(stage)), recursive=False, filter=normalize)


def write_checksum(path: Path) -> Path:
    checksum = path.with_name(path.name + ".sha256")
    checksum.write_text(f"{sha256(path)}  {path.name}\n", encoding="ascii")
    return checksum


def run(command: list[str]) -> str:
    return subprocess.run(command, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def package(args: argparse.Namespace) -> int:
    if args.platform not in PLATFORMS or args.architecture not in ARCHITECTURES:
        raise ValueError("unsupported platform or architecture")
    if not REVISION.fullmatch(args.source_revision):
        raise ValueError("source revision must be a full lowercase commit SHA")
    if workspace_version() != args.version:
        raise ValueError(f"Cargo workspace version {workspace_version()} != {args.version}")
    runtime = Path(args.runtime)
    runtime_name = BINARY + (".exe" if args.platform == "windows" else "")
    if not runtime.is_file() or runtime.name != runtime_name:
        raise ValueError(f"runtime must be an existing {runtime_name}")

    output = Path(args.output_dir)
    base = archive_base(args.version, args.platform, args.architecture)
    stage = output / "stage" / base
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    created_at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

    tree = run(["cargo", "tree", "--locked", "-p", PACKAGE, "--target", args.target,
                "-e", "normal", "--prefix", "none", "--format", "{p}"])
    metadata = json.loads(run(["cargo", "metadata", "--locked", "--format-version", "1",
                               "--filter-platform", args.target]))
    records = collect_licenses(parse_cargo_tree(tree), metadata, stage / "licenses")

    shutil.copyfile(runtime, stage / runtime_name)
    if args.platform != "windows":
        (stage / runtime_name).chmod(0o755)
    for name in ("LICENSE", "THIRD_PARTY_NOTICES.md", "CHANGELOG.md"):
        shutil.copyfile(ROOT / name, stage / name)
    (stage / "sbom.spdx.json").write_text(
        json.dumps(build_sbom(args.version, args.source_revision, records, lock_checksums(), created_at), indent=2) + "\n",
        encoding="utf-8",
    )
    manifest = build_manifest(
        version=args.version,
        revision=args.source_revision,
        platform=args.platform,
        architecture=args.architecture,
        target=args.target,
        toolchain=args.toolchain,
        signing_kind=args.signing_kind,
        runtime_name=runtime_name,
        runtime_sha256=sha256(stage / runtime_name),
        sbom_sha256=sha256(stage / "sbom.spdx.json"),
        notices_sha256=sha256(stage / "THIRD_PARTY_NOTICES.md"),
        created_at=created_at,
    )
    (stage / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    (stage / "README.txt").write_text(
        f"Nexus Computer Use Runtime {args.version}\n"
        f"{args.platform}/{args.architecture}, signing: {args.signing_kind}\n"
        f"Source revision {args.source_revision}\n"
        "See manifest.json for digests and THIRD_PARTY_NOTICES.md and licenses/ for licenses.\n",
        encoding="utf-8",
    )

    archive = output / (base + (".zip" if args.platform == "windows" else ".tar.gz"))
    write_archive(stage, archive)
    write_checksum(archive)
    # Standalone copies let reviewers inspect provenance without unpacking.
    for source, suffix in (("manifest.json", "manifest.json"), ("sbom.spdx.json", "sbom.spdx.json"),
                           ("THIRD_PARTY_NOTICES.md", "THIRD_PARTY_NOTICES.md")):
        shutil.copyfile(stage / source, output / f"{base}.{suffix}")
    shutil.rmtree(output / "stage")
    print(archive)
    return 0


def checksums(args: argparse.Namespace) -> int:
    directory = Path(args.directory)
    names = sorted(path.name for path in directory.iterdir() if path.is_file() and path.name != "SHA256SUMS")
    if not names:
        raise ValueError("no release assets to checksum")
    (directory / "SHA256SUMS").write_text(
        "".join(f"{sha256(directory / name)}  {name}\n" for name in names), encoding="ascii"
    )
    return 0


def release_notes(changelog: str, version: str) -> str:
    """Return the CHANGELOG section for `version`, without its heading."""
    match = re.search(rf"(?ms)^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## \[|\Z)", changelog)
    if match is None or not match.group(1).strip():
        raise ValueError(f"CHANGELOG.md has no non-empty section for {version}")
    return match.group(1).strip() + "\n"


def notes(args: argparse.Namespace) -> int:
    body = release_notes((ROOT / "CHANGELOG.md").read_text(encoding="utf-8"), args.version)
    signing = []
    for manifest_path in sorted(Path(args.assets).glob("*.manifest.json")):
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        signing.append(f"- {manifest['platform']}/{manifest['architecture']}: `{manifest['signing']['kind']}`")
    if signing:
        body += "\n### Signing\n\n" + "\n".join(signing) + "\n"
    body += "\nVerify downloads against `SHA256SUMS`.\n"
    Path(args.output).write_text(body, encoding="utf-8")
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    version = commands.add_parser("version", help="print and verify the version for a release tag")
    version.add_argument("--tag", required=True)

    pack = commands.add_parser("package", help="package one built runtime")
    pack.add_argument("--version", required=True)
    pack.add_argument("--platform", required=True, choices=sorted(PLATFORMS))
    pack.add_argument("--architecture", required=True, choices=sorted(ARCHITECTURES))
    pack.add_argument("--target", required=True)
    pack.add_argument("--toolchain", required=True)
    pack.add_argument("--runtime", required=True)
    pack.add_argument("--source-revision", required=True)
    pack.add_argument("--signing-kind", required=True, choices=sorted(SIGNING_KINDS))
    pack.add_argument("--output-dir", required=True)

    sums = commands.add_parser("checksums", help="write SHA256SUMS for a release asset directory")
    sums.add_argument("--directory", required=True)

    note = commands.add_parser("notes", help="render GitHub release notes from CHANGELOG.md")
    note.add_argument("--version", required=True)
    note.add_argument("--assets", required=True)
    note.add_argument("--output", required=True)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "version":
            value = version_from_tag(args.tag)
            if workspace_version() != value:
                raise ValueError(f"tag {args.tag} does not match Cargo workspace version {workspace_version()}")
            print(value)
            return 0
        return {"package": package, "checksums": checksums, "notes": notes}[args.command](args)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"package_release: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
