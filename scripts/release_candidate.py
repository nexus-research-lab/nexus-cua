#!/usr/bin/env python3
"""Fail-closed helpers for producing Nexus CUA release candidates."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path


CONTRACT = "nexus.cua.release-candidate.v1"
EVIDENCE_CONTRACT = "nexus.cua.native-evidence.v2"
EVIDENCE_CLASS_RANK = {
    "diagnostic": 0,
    "engineering_accepted": 1,
    "release_accepted": 2,
}
REQUIRED_SOAK_PROFILE = {
    "engineering_accepted": "engineering",
    "release_accepted": "release",
}
CANDIDATE_KINDS = ("engineering", "release")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SEMVER_RE = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:[-+][0-9A-Za-z.-]+)?$")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_json(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as stream:
        value = json.load(stream)
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def workspace_version(cargo_manifest: Path) -> str:
    in_workspace_package = False
    for raw_line in cargo_manifest.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line.startswith("["):
            in_workspace_package = line == "[workspace.package]"
        elif in_workspace_package and line.startswith("version"):
            _, raw_value = line.split("=", 1)
            return raw_value.strip().strip('"')
    raise ValueError(f"workspace package version is missing from {cargo_manifest}")


def check_version(version: str, cargo_manifest: Path) -> None:
    if not SEMVER_RE.fullmatch(version):
        raise ValueError(f"candidate version is not SemVer: {version}")
    expected = workspace_version(cargo_manifest)
    if version != expected:
        raise ValueError(
            f"candidate version {version} does not match workspace version {expected}"
        )


def validate_evidence(
    evidence_path: Path,
    runtime_path: Path,
    source_revision: str,
    platform: str,
    architecture: str,
    required_class: str,
    runner_manifest_path: Path,
) -> dict:
    evidence = load_json(evidence_path)
    failures: list[str] = []

    if required_class not in REQUIRED_SOAK_PROFILE:
        raise ValueError(f"unsupported accepted evidence class: {required_class}")

    expected = {
        "evidence_contract": EVIDENCE_CONTRACT,
        "requested_evidence_class": required_class,
        "class_requirement_satisfied": True,
        "required_soak_profile": REQUIRED_SOAK_PROFILE[required_class],
        "source_revision": source_revision,
        "platform": platform,
        "architecture": architecture,
    }
    for field, expected_value in expected.items():
        if evidence.get(field) != expected_value:
            failures.append(
                f"{field} must be {expected_value!r}, got {evidence.get(field)!r}"
            )

    evidence_class = evidence.get("evidence_class")
    if (
        not isinstance(evidence_class, str)
        or evidence_class not in EVIDENCE_CLASS_RANK
        or EVIDENCE_CLASS_RANK[evidence_class] < EVIDENCE_CLASS_RANK[required_class]
    ):
        failures.append(
            f"evidence_class must satisfy {required_class!r}, got {evidence_class!r}"
        )

    issues = evidence.get("issues")
    if issues != []:
        failures.append(f"issues must be empty, got {issues!r}")

    runner = evidence.get("runner")
    expected_runner = load_json(runner_manifest_path)
    if (
        not isinstance(runner, dict)
        or runner.get("status") != "active"
        or runner != expected_runner
    ):
        failures.append("runner must be an active pinned runner manifest")

    tested_payload_digests = evidence.get("tested_payload_digests")
    if not isinstance(tested_payload_digests, dict):
        failures.append("tested_payload_digests must be an object")
        tested_payload_digests = {}
    recorded_hash = tested_payload_digests.get("runtime_sha256")
    actual_hash = sha256(runtime_path)
    if not isinstance(recorded_hash, str) or not SHA256_RE.fullmatch(recorded_hash):
        failures.append("runtime_sha256 must be a lowercase SHA-256 digest")
    elif recorded_hash != actual_hash:
        failures.append(
            f"tested runtime digest mismatch: evidence={recorded_hash}, file={actual_hash}"
        )
    if evidence.get("runtime_sha256") != recorded_hash:
        failures.append("runtime_sha256 must match tested_payload_digests.runtime_sha256")

    runner_manifest_sha256 = evidence.get("runner_manifest_sha256")
    if not isinstance(runner_manifest_sha256, str) or not SHA256_RE.fullmatch(
        runner_manifest_sha256
    ):
        failures.append("runner_manifest_sha256 must be a lowercase SHA-256 digest")
    elif runner_manifest_sha256 != sha256(runner_manifest_path):
        failures.append("runner_manifest_sha256 does not match the pinned manifest")

    report_digests = evidence.get("required_report_digests")
    if not isinstance(report_digests, dict) or not report_digests:
        failures.append("required_report_digests must be a non-empty object")
    else:
        for name, record in report_digests.items():
            if (
                not isinstance(name, str)
                or not isinstance(record, dict)
                or not isinstance(record.get("sha256"), str)
                or not SHA256_RE.fullmatch(record["sha256"])
                or not isinstance(record.get("size_bytes"), int)
                or record["size_bytes"] < 0
            ):
                failures.append(f"invalid required report digest record: {name!r}")

    if failures:
        raise ValueError("native evidence rejected:\n- " + "\n- ".join(failures))
    return evidence


def file_record(path: Path) -> dict:
    if not path.is_file():
        raise ValueError(f"release candidate input is not a file: {path}")
    return {
        "path": path.name,
        "sha256": sha256(path),
        "size_bytes": path.stat().st_size,
    }


def build_manifest(args: argparse.Namespace) -> dict:
    check_version(args.version, args.cargo_manifest)
    if args.candidate_kind not in CANDIDATE_KINDS:
        raise ValueError(f"unsupported candidate kind: {args.candidate_kind}")
    native_evidence = load_json(args.evidence)
    tested_payload_digests = native_evidence.get("tested_payload_digests")
    tested_runtime_sha256 = (
        tested_payload_digests.get("runtime_sha256")
        if isinstance(tested_payload_digests, dict)
        else None
    )
    if not isinstance(tested_runtime_sha256, str) or not SHA256_RE.fullmatch(
        tested_runtime_sha256
    ):
        raise ValueError("native evidence has no valid tested runtime digest")
    manifest = {
        "contract": CONTRACT,
        "release_state": "unaccepted_legacy",
        "candidate_kind": args.candidate_kind,
        "version": args.version,
        "protocol_version": args.protocol_version,
        "source_revision": args.source_revision,
        "platform": args.platform,
        "architecture": args.architecture,
        "tested_runtime_sha256": tested_runtime_sha256,
        "files": {
            "runtime": file_record(args.runtime),
            "sbom": file_record(args.sbom),
            "native_evidence": file_record(args.evidence),
            "signature_evidence": file_record(args.signature_evidence),
            "smoke_evidence": file_record(args.smoke_evidence),
        },
    }
    args.output.write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return manifest


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="command", required=True)

    version = commands.add_parser("check-version")
    version.add_argument("--version", required=True)
    version.add_argument("--cargo-manifest", type=Path, default=Path("Cargo.toml"))

    evidence = commands.add_parser("validate-evidence")
    evidence.add_argument("--evidence", type=Path, required=True)
    evidence.add_argument("--runtime", type=Path, required=True)
    evidence.add_argument("--source-revision", required=True)
    evidence.add_argument("--platform", required=True)
    evidence.add_argument("--architecture", required=True)
    evidence.add_argument(
        "--required-class",
        choices=sorted(REQUIRED_SOAK_PROFILE),
        required=True,
    )
    evidence.add_argument("--runner-manifest", type=Path, required=True)

    manifest = commands.add_parser("manifest")
    manifest.add_argument("--version", required=True)
    manifest.add_argument("--candidate-kind", choices=CANDIDATE_KINDS, required=True)
    manifest.add_argument("--protocol-version", required=True)
    manifest.add_argument("--source-revision", required=True)
    manifest.add_argument("--platform", required=True)
    manifest.add_argument("--architecture", required=True)
    manifest.add_argument("--runtime", type=Path, required=True)
    manifest.add_argument("--sbom", type=Path, required=True)
    manifest.add_argument("--evidence", type=Path, required=True)
    manifest.add_argument("--signature-evidence", type=Path, required=True)
    manifest.add_argument("--smoke-evidence", type=Path, required=True)
    manifest.add_argument("--output", type=Path, required=True)
    manifest.add_argument("--cargo-manifest", type=Path, default=Path("Cargo.toml"))
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "check-version":
            check_version(args.version, args.cargo_manifest)
        elif args.command == "validate-evidence":
            validate_evidence(
                args.evidence,
                args.runtime,
                args.source_revision,
                args.platform,
                args.architecture,
                args.required_class,
                args.runner_manifest,
            )
        else:
            build_manifest(args)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
