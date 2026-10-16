#!/usr/bin/env python3
"""Stage fail-closed, no-download distribution channel artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import sys
import tarfile
import zipfile
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import urlparse


MAX_PAYLOAD_FILES = 4096
MAX_PAYLOAD_BYTES = 512 * 1024 * 1024
MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
MAX_ARCHIVE_JSON_BYTES = 8 * 1024 * 1024
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SEMVER_RE = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:[-+][0-9A-Za-z.-]+)?$"
)
SAFE_URL_RE = re.compile(r"^[0-9A-Za-z:/?&._~%+=-]+$")
SAFE_ARCHIVE_COMPONENT_RE = re.compile(r"^[0-9A-Za-z._+-]+$")
ACCEPTED_STATES = {
    "engineering": "engineering_candidate",
    "release": "release_candidate",
}
SOURCE_REVISION_RE = re.compile(r"^[0-9a-f]{40}$")
NATIVE_EVIDENCE_CLASS = {
    "engineering": ("engineering_accepted", "engineering"),
    "release": ("release_accepted", "release"),
}
REMOTE_SCENARIOS = {
    "mutual_tls_rejects_untrusted_client",
    "authentication_precedes_worker_resolution",
    "closed_gateway_round_trip",
    "lease_epoch_mismatch_fails_closed",
    "path_free_artifact_ticket",
    "post_admission_disconnect_reconciles",
    "worker_epoch_rotation_blocks_replay",
    "oversized_frame_rejected",
}
CODEX_REQUIRED_SEQUENCE = (
    "get_status",
    "list_launchable_apps",
    "launch_application",
    "list_windows",
    "observe_window",
    "click_normalized",
    "observe_window",
    "perform_action",
    "observe_window",
    "terminate_application",
    "list_apps",
    "list_windows",
)
MCP_TOOL_ALLOWLIST = {
    "get_status",
    "list_apps",
    "list_launchable_apps",
    "launch_application",
    "list_windows",
    "list_displays",
    "observe_display",
    "observe_window",
    "read_clipboard",
    "write_clipboard",
    "zoom",
    "click_normalized",
    "click_view_normalized",
    "type_text",
    "press_keys",
    "scroll",
    "drag_normalized",
    "invoke_element",
    "set_element_value",
    "set_range_value",
    "reconcile_mutation",
    "terminate_application",
    "perform_action",
    "verify_state",
}
REQUIRED_PAYLOAD_FILES = (
    "README.md",
    "PRD.zh-CN.md",
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
    "sbom.spdx.json",
    "manifest.json",
    "provenance.intoto.jsonl",
    "native-evidence-summary.json",
    "remote-transport-summary.json",
    "codex-acceptance-summary.json",
    "skills/common/instructions.md",
    "skills/cli/SKILL.md",
    "skills/mcp/SKILL.md",
    "profiles/unbound.json",
    "profiles/nexus.json",
    "profiles/mcp.json",
)
PROFILE_EXPECTATIONS = {
    "unbound": {
        "adapter_binding": "none",
        "skill_binding": "none",
        "authority_provider": "none",
        "host_integration": "none",
        "default_mcp_transport": None,
        "allowed_mcp_transports": [],
    },
    "mcp": {
        "adapter_binding": "mcp",
        "skill_binding": "mcp",
        "authority_provider": "registered_local_provider",
        "host_integration": "explicit_inspect_plan_apply",
        "default_mcp_transport": "stdio",
        "allowed_mcp_transports": ["stdio", "http"],
    },
    "nexus": {
        "adapter_binding": "cli",
        "skill_binding": "cli",
        "authority_provider": "nexus_scoped_broker",
        "host_integration": "downstream_nexus_installer",
        "default_mcp_transport": None,
        "allowed_mcp_transports": [],
    },
}
PROFILE_KEYS = {
    "contract",
    "profile",
    "product_version",
    "protocol_version",
    "adapter_binding",
    "skill_binding",
    "authority_provider",
    "host_integration",
    "default_mcp_transport",
    "allowed_mcp_transports",
    "bare_install_activates",
    "automatic_service_start",
    "automatic_permissions",
}


@dataclass(frozen=True)
class ArchiveFile:
    size_bytes: int
    sha256: str
    content: bytes | None
    mode: int | None


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def reject_duplicate_json_keys(pairs: list[tuple[str, object]]) -> dict:
    value = {}
    for key, child in pairs:
        if key in value:
            raise ValueError(f"JSON object contains duplicate key {key!r}")
        value[key] = child
    return value


def load_json(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as stream:
        value = json.load(stream, object_pairs_hook=reject_duplicate_json_keys)
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def decode_json_object(value: bytes, label: str) -> dict:
    if len(value) > MAX_ARCHIVE_JSON_BYTES:
        raise ValueError(f"{label} exceeds the bounded JSON size")
    parsed = json.loads(value, object_pairs_hook=reject_duplicate_json_keys)
    if not isinstance(parsed, dict):
        raise ValueError(f"{label} must contain a JSON object")
    return parsed


def load_channels(path: Path) -> dict:
    channels = load_json(path)
    if channels.get("contract") != "nexus.cua.distribution-channels.v1":
        raise ValueError("distribution channel contract is unsupported")
    return channels


def check_version(version: str) -> None:
    if not SEMVER_RE.fullmatch(version):
        raise ValueError(f"invalid SemVer package version: {version}")


def check_digest(value: object, label: str) -> None:
    if not isinstance(value, str) or not SHA256_RE.fullmatch(value):
        raise ValueError(f"{label} must be a lowercase SHA-256 digest")


def check_source_revision(value: object) -> None:
    if not isinstance(value, str) or not SOURCE_REVISION_RE.fullmatch(value):
        raise ValueError("source revision must be one lowercase 40-character Git SHA")


def check_record(
    record: object,
    *,
    expected_name: str,
    expected_sha256: str,
    expected_size: int,
    label: str,
) -> None:
    if not isinstance(record, dict) or set(record) != {"path", "sha256", "size_bytes"}:
        raise ValueError(f"{label} manifest record is not closed")
    check_digest(record.get("sha256"), f"{label} digest")
    if record.get("path") != expected_name:
        raise ValueError(f"{label} manifest path does not match {expected_name}")
    if record.get("size_bytes") != expected_size:
        raise ValueError(f"{label} manifest size does not match the payload")
    if record.get("sha256") != expected_sha256:
        raise ValueError(f"{label} manifest digest does not match the payload")


def check_file_record(
    record: object, path: Path, label: str, *, expected_name: str | None = None
) -> None:
    check_record(
        record,
        expected_name=expected_name or path.name,
        expected_sha256=sha256(path),
        expected_size=path.stat().st_size,
        label=label,
    )


def is_subsequence(required: tuple[str, ...], actual: list[str]) -> bool:
    position = 0
    for value in actual:
        if position < len(required) and value == required[position]:
            position += 1
    return position == len(required)


def check_archive(archive: Path, expected_sha256: str) -> None:
    check_digest(expected_sha256, "archive digest")
    if not archive.is_file() or archive.is_symlink():
        raise ValueError(f"archive must be a regular non-symlink file: {archive}")
    if archive.stat().st_size > MAX_ARCHIVE_BYTES:
        raise ValueError(f"archive exceeds {MAX_ARCHIVE_BYTES} compressed bytes")
    actual = sha256(archive)
    if actual != expected_sha256:
        raise ValueError(
            f"archive digest mismatch: expected={expected_sha256}, actual={actual}"
        )


def check_url(url: str, *, allow_file: bool) -> None:
    if not SAFE_URL_RE.fullmatch(url):
        raise ValueError("channel URL contains unsafe or non-canonical characters")
    parsed = urlparse(url)
    allowed = {"https"}
    if allow_file:
        allowed.add("file")
    if parsed.scheme not in allowed or (parsed.scheme == "https" and not parsed.netloc):
        raise ValueError(f"channel URL must use one of {sorted(allowed)}")


def validate_candidate_manifest(
    manifest: dict,
    *,
    version: str,
    candidate_kind: str,
    platform: str | None = None,
    architecture: str | None = None,
    runtime_sha256: str | None = None,
) -> None:
    if candidate_kind not in ACCEPTED_STATES:
        raise ValueError(f"unsupported candidate kind: {candidate_kind}")
    check_source_revision(manifest.get("source_revision"))
    expected = {
        "contract": "nexus.cua.release-candidate.v1",
        "version": version,
        "candidate_kind": candidate_kind,
        "release_state": ACCEPTED_STATES[candidate_kind],
        "protocol_version": "nexus.cua.v1",
    }
    if platform is not None:
        expected["platform"] = platform
    if architecture is not None:
        expected["architecture"] = architecture
    if runtime_sha256 is not None:
        expected["tested_runtime_sha256"] = runtime_sha256
    failures = [
        f"{field}={manifest.get(field)!r}, expected {expected_value!r}"
        for field, expected_value in expected.items()
        if manifest.get(field) != expected_value
    ]
    if failures:
        raise ValueError("candidate manifest rejected: " + "; ".join(failures))
    files = manifest.get("files")
    if not isinstance(files, dict):
        raise ValueError("candidate manifest files must be an object")
    for name in ("runtime", "native_evidence", "remote_transport", "codex_acceptance"):
        record = files.get(name)
        if not isinstance(record, dict):
            raise ValueError(f"candidate manifest is missing {name} evidence")
        check_digest(record.get("sha256"), f"candidate {name}")
        if (
            not isinstance(record.get("path"), str)
            or not record["path"]
            or not isinstance(record.get("size_bytes"), int)
            or record["size_bytes"] < 0
        ):
            raise ValueError(f"candidate {name} file record is invalid")


def check_candidate_manifest(
    path: Path,
    *,
    version: str,
    candidate_kind: str,
    platform: str | None = None,
    architecture: str | None = None,
    runtime_sha256: str | None = None,
) -> dict:
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"candidate manifest must be a regular non-symlink file: {path}")
    manifest = load_json(path)
    validate_candidate_manifest(
        manifest,
        version=version,
        candidate_kind=candidate_kind,
        platform=platform,
        architecture=architecture,
        runtime_sha256=runtime_sha256,
    )
    return manifest


def inventory_payload(root: Path, runtime_relative: Path) -> list[Path]:
    if not root.is_dir() or root.is_symlink():
        raise ValueError(f"payload root must be a non-symlink directory: {root}")
    required = [Path(value) for value in REQUIRED_PAYLOAD_FILES]
    required.extend((runtime_relative, Path("schemas/nexus.cua.v1")))
    for relative in required:
        candidate = root / relative
        if not candidate.exists() or candidate.is_symlink():
            raise ValueError(f"canonical payload is missing {relative.as_posix()}")

    files: list[Path] = []
    total_bytes = 0
    for path in sorted(root.rglob("*")):
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError(f"canonical payload contains a symlink: {path}")
        if stat.S_ISDIR(metadata.st_mode):
            continue
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError(f"canonical payload contains a non-regular file: {path}")
        files.append(path)
        if len(files) > MAX_PAYLOAD_FILES:
            raise ValueError(f"canonical payload exceeds {MAX_PAYLOAD_FILES} files")
        total_bytes += metadata.st_size
        if total_bytes > MAX_PAYLOAD_BYTES:
            raise ValueError(f"canonical payload exceeds {MAX_PAYLOAD_BYTES} bytes")
    if not any(
        path.is_file() and not path.is_symlink()
        for path in (root / "schemas/nexus.cua.v1").rglob("*.json")
    ):
        raise ValueError("canonical payload schema directory has no JSON schema")
    return files


def validate_install_profile(value: dict, name: str, version: str) -> None:
    if set(value) != PROFILE_KEYS:
        raise ValueError(f"{name} install profile has unknown or missing fields")
    expected = {
        "contract": "nexus.cua.install-profile.v1",
        "profile": name,
        "product_version": version,
        "protocol_version": "nexus.cua.v1",
        "bare_install_activates": False,
        "automatic_service_start": False,
        "automatic_permissions": False,
        **PROFILE_EXPECTATIONS[name],
    }
    if value != expected:
        raise ValueError(f"{name} install profile does not match its closed contract")


def validate_install_profiles(root: Path, version: str) -> None:
    for name in PROFILE_EXPECTATIONS:
        validate_install_profile(load_json(root / f"profiles/{name}.json"), name, version)


def reject_sensitive_summary_values(value: object, label: str) -> None:
    forbidden_keys = {
        "authorization",
        "token",
        "grant",
        "screenshot",
        "accessibility_tree",
        "clipboard",
        "typed_text",
        "input_text",
        "native_handle",
        "local_path",
    }
    if isinstance(value, dict):
        for key, child in value.items():
            if str(key).lower() in forbidden_keys:
                raise ValueError(f"{label} contains forbidden sensitive field {key!r}")
            reject_sensitive_summary_values(child, label)
    elif isinstance(value, list):
        for child in value:
            reject_sensitive_summary_values(child, label)


def validate_native_evidence(
    value: dict,
    *,
    candidate_kind: str,
    source_revision: str,
    platform_name: str,
    architecture: str,
    runtime_sha256: str,
) -> None:
    evidence_class, soak_profile = NATIVE_EVIDENCE_CLASS[candidate_kind]
    expected = {
        "evidence_contract": "nexus.cua.native-evidence.v2",
        "requested_evidence_class": evidence_class,
        "class_requirement_satisfied": True,
        "required_soak_profile": soak_profile,
        "source_revision": source_revision,
        "platform": platform_name,
        "architecture": architecture,
        "runtime_sha256": runtime_sha256,
        "issues": [],
    }
    for field, expected_value in expected.items():
        if value.get(field) != expected_value:
            raise ValueError(f"native evidence {field} does not match the candidate")
    ranks = {"diagnostic": 0, "engineering_accepted": 1, "release_accepted": 2}
    actual_class = value.get("evidence_class")
    if actual_class not in ranks or ranks[actual_class] < ranks[evidence_class]:
        raise ValueError("native evidence class was downgraded")
    payload_digests = value.get("tested_payload_digests")
    if not isinstance(payload_digests, dict) or payload_digests.get(
        "runtime_sha256"
    ) != runtime_sha256:
        raise ValueError("native evidence does not bind the tested runtime")
    runner = value.get("runner")
    if not isinstance(runner, dict) or runner.get("status") != "active":
        raise ValueError("native evidence runner is not active")
    check_digest(value.get("runner_manifest_sha256"), "native runner manifest")
    reports = value.get("required_report_digests")
    if not isinstance(reports, dict) or not reports:
        raise ValueError("native evidence has no required report digests")
    for name, record in reports.items():
        if not isinstance(name, str) or not name or not isinstance(record, dict):
            raise ValueError("native evidence report record is invalid")
        check_digest(record.get("sha256"), f"native report {name}")
        if not isinstance(record.get("size_bytes"), int) or record["size_bytes"] < 0:
            raise ValueError(f"native report {name} size is invalid")


def validate_remote_evidence(value: dict, source_revision: str) -> None:
    expected = {
        "schema_version": "nexus.cua.remote-transport.v1",
        "source_revision": source_revision,
        "transport": "tcp+mutual_tls_1_2_or_1_3",
        "gateway_protocol": "nexus.cua.gateway.v1",
        "process_count": 2,
        "accepted": True,
        "issues": [],
    }
    for field, expected_value in expected.items():
        if value.get(field) != expected_value:
            raise ValueError(f"remote evidence {field} does not match its contract")
    for field in (
        "controller_binary_sha256",
        "worker_binary_sha256",
        "certificate_identity_sha256",
    ):
        check_digest(value.get(field), f"remote evidence {field}")
    if value["controller_binary_sha256"] != value["worker_binary_sha256"]:
        raise ValueError("remote evidence used different controller and worker binaries")
    bounds = value.get("resource_bounds")
    if (
        not isinstance(bounds, dict)
        or not bounds
        or any(not isinstance(item, int) or item <= 0 for item in bounds.values())
    ):
        raise ValueError("remote evidence resource bounds are missing or invalid")
    scenarios = value.get("scenarios")
    if not isinstance(scenarios, list) or {
        scenario.get("scenario") for scenario in scenarios if isinstance(scenario, dict)
    } != REMOTE_SCENARIOS:
        raise ValueError("remote evidence does not contain the exact scenario matrix")
    if len(scenarios) != len(REMOTE_SCENARIOS) or any(
        not isinstance(scenario, dict)
        or scenario.get("accepted") is not True
        or not isinstance(scenario.get("disposition"), str)
        or not scenario["disposition"]
        or not isinstance(scenario.get("evidence"), str)
        or not scenario["evidence"]
        for scenario in scenarios
    ):
        raise ValueError("remote evidence contains a failed or malformed scenario")
    reject_sensitive_summary_values(value, "remote evidence")


def validate_request_ids(value: object, label: str) -> None:
    required = {"launch", "click", "toggle", "terminate"}
    if not isinstance(value, dict) or set(value) != required:
        raise ValueError(f"{label} request IDs do not cover the exact mutation set")
    request_ids = list(value.values())
    if any(
        not isinstance(item, str)
        or not 1 <= len(item) <= 128
        or not re.fullmatch(r"[0-9A-Za-z_.:-]+", item)
        for item in request_ids
    ) or len(set(request_ids)) != len(request_ids):
        raise ValueError(f"{label} request IDs are invalid or reused")


def validate_cleanup(value: object, label: str) -> None:
    required = {"fixture_absent", "registration_removed", "artifacts_removed"}
    if (
        not isinstance(value, dict)
        or not required.issubset(value)
        or any(item is not True for item in value.values())
    ):
        raise ValueError(f"{label} cleanup is incomplete")


def validate_codex_attempt(value: object, attempt_index: int, allowed: set[str]) -> None:
    if not isinstance(value, dict):
        raise ValueError("Codex business attempt must be an object")
    if value.get("attempt_index") != attempt_index or value.get("status") != "accepted":
        raise ValueError("Codex business attempts must be consecutive 3/3 passes")
    sequence = value.get("actual_tool_sequence")
    if (
        not isinstance(sequence, list)
        or any(not isinstance(tool, str) or tool not in allowed for tool in sequence)
        or not is_subsequence(CODEX_REQUIRED_SEQUENCE, sequence)
    ):
        raise ValueError("Codex business attempt tool sequence is incomplete or out of scope")
    events = value.get("tool_events")
    if (
        not isinstance(events, list)
        or [event.get("tool") for event in events if isinstance(event, dict)] != sequence
        or any(
            not isinstance(event, dict)
            or event.get("server") != "nexuscua"
            or event.get("tool", "").startswith("nexus_cua_")
            for event in events
        )
    ):
        raise ValueError("Codex business attempt does not prove exact MCP tool identities")
    validate_request_ids(value.get("request_ids"), "Codex business attempt")
    expected_state = {
        "initial_counter_zero": True,
        "initial_checked_false": True,
        "counter_one_after_click": True,
        "checked_true_after_toggle": True,
        "final_counter_one": True,
        "final_checked_true": True,
        "final_applications_empty": True,
        "final_windows_empty": True,
    }
    if value.get("state_assertions") != expected_state:
        raise ValueError("Codex business attempt state assertions are incomplete")
    timings = value.get("timings_ms")
    if (
        not isinstance(timings, dict)
        or not timings
        or any(not isinstance(item, int | float) or item < 0 for item in timings.values())
    ):
        raise ValueError("Codex business attempt timings are invalid")
    if value.get("policy_violations") != []:
        raise ValueError("Codex business attempt contains a policy violation")
    validate_cleanup(value.get("cleanup"), "Codex business attempt")


def validate_codex_evidence(
    value: dict,
    *,
    version: str,
    source_revision: str,
    platform_name: str,
    architecture: str,
    runtime_sha256: str,
) -> None:
    expected = {
        "schema_version": "nexus.cua.codex-acceptance.v1",
        "product_version": version,
        "protocol_version": "nexus.cua.v1",
        "source_revision": source_revision,
        "platform": platform_name,
        "architecture": architecture,
        "runtime_sha256": runtime_sha256,
        "mcp_profile": "full",
        "accepted": True,
        "policy_violations": [],
        "issues": [],
    }
    for field, expected_value in expected.items():
        if value.get(field) != expected_value:
            raise ValueError(f"Codex evidence {field} does not match the candidate")
    for field in (
        "runner_manifest_sha256",
        "prompt_sha256",
        "tool_schema_sha256",
        "skill_sha256",
        "provider_identity_sha256",
        "authority_scope_sha256",
    ):
        check_digest(value.get(field), f"Codex evidence {field}")
    for field in (
        "os_build",
        "codex_version",
        "model_provider",
        "model_id",
        "reasoning_configuration",
    ):
        if not isinstance(value.get(field), str) or not value[field]:
            raise ValueError(f"Codex evidence {field} is missing")
    allowed_sequence = value.get("allowed_tool_sequence")
    if (
        not isinstance(allowed_sequence, list)
        or not allowed_sequence
        or any(
            not isinstance(tool, str)
            or not tool
            or tool.startswith("nexus_cua_")
            or tool not in MCP_TOOL_ALLOWLIST
            for tool in allowed_sequence
        )
        or len(set(allowed_sequence)) != len(allowed_sequence)
    ):
        raise ValueError("Codex evidence tool allowlist is invalid")
    allowed = set(allowed_sequence)
    attempts = value.get("business_attempts")
    if not isinstance(attempts, list) or len(attempts) != 3:
        raise ValueError("Codex evidence must contain exactly three business attempts")
    for index, attempt in enumerate(attempts, start=1):
        validate_codex_attempt(attempt, index, allowed)
    fault = value.get("fault_scenario")
    if (
        not isinstance(fault, dict)
        or fault.get("status") != "accepted"
        or fault.get("tool_sequence") != ["perform_action", "reconcile_mutation"]
        or fault.get("original_request_id") != fault.get("reconciled_request_id")
        or fault.get("dispatch_count") != 1
        or fault.get("effect_count") != 1
        or fault.get("disposition") != "succeeded"
        or fault.get("policy_violations") != []
    ):
        raise ValueError("Codex post-admission fault scenario is incomplete")
    validate_cleanup(fault.get("cleanup"), "Codex fault scenario")
    infrastructure = value.get("infrastructure_attempts")
    if not isinstance(infrastructure, list) or len(infrastructure) > 6 or any(
        not isinstance(attempt, dict)
        or attempt.get("status") != "infrastructure_error"
        or attempt.get("tool_events") != []
        for attempt in infrastructure
    ):
        raise ValueError("Codex infrastructure retries are invalid or unrecorded")
    validate_cleanup(value.get("cleanup"), "Codex evidence")
    reject_sensitive_summary_values(value, "Codex evidence")


def validate_payload_evidence(
    root: Path,
    manifest: dict,
    *,
    version: str,
    candidate_kind: str,
    platform_name: str,
    acceptance_architecture: str,
    runtime_sha256: str,
) -> None:
    source_revision = manifest["source_revision"]
    paths = {
        "native_evidence": root / "native-evidence-summary.json",
        "remote_transport": root / "remote-transport-summary.json",
        "codex_acceptance": root / "codex-acceptance-summary.json",
    }
    files = manifest["files"]
    for name, path in paths.items():
        check_file_record(files.get(name), path, name)
    validate_native_evidence(
        load_json(paths["native_evidence"]),
        candidate_kind=candidate_kind,
        source_revision=source_revision,
        platform_name=platform_name,
        architecture=acceptance_architecture,
        runtime_sha256=runtime_sha256,
    )
    validate_remote_evidence(load_json(paths["remote_transport"]), source_revision)
    validate_codex_evidence(
        load_json(paths["codex_acceptance"]),
        version=version,
        source_revision=source_revision,
        platform_name=platform_name,
        architecture=acceptance_architecture,
        runtime_sha256=runtime_sha256,
    )


def canonical_archive_path(raw: str, *, is_directory: bool) -> str | None:
    if not raw or "\0" in raw or "\\" in raw or raw.startswith("/"):
        raise ValueError(f"archive contains unsafe path {raw!r}")
    value = raw
    while value.startswith("./"):
        value = value[2:]
    if is_directory:
        value = value.rstrip("/")
    if value in {"", "."}:
        if is_directory:
            return None
        raise ValueError("archive contains an invalid root file")
    parts = value.split("/")
    if any(
        part in {"", ".", ".."} or not SAFE_ARCHIVE_COMPONENT_RE.fullmatch(part)
        for part in parts
    ):
        raise ValueError(f"archive contains unsafe or non-canonical path {raw!r}")
    return "/".join(parts)


def require_archive_root(name: str, archive_root: str, *, is_directory: bool) -> None:
    if name == archive_root:
        if not is_directory:
            raise ValueError("archive root must be a directory")
        return
    if not name.startswith(f"{archive_root}/"):
        raise ValueError(
            f"archive member {name!r} is outside the canonical root {archive_root!r}"
        )


def reject_conflicting_archive_hierarchy(
    files: dict[str, ArchiveFile],
    seen: set[str],
    name: str,
    *,
    regular_file: bool,
) -> None:
    components = name.split("/")
    parents = ["/".join(components[:index]) for index in range(1, len(components))]
    if any(parent in files for parent in parents) or (
        regular_file and any(existing.startswith(f"{name}/") for existing in seen)
    ):
        raise ValueError(f"archive contains conflicting path hierarchy at {name!r}")


def read_archive_file(
    stream: object,
    *,
    declared_size: int,
    capture: bool,
    label: str,
    mode: int | None,
) -> ArchiveFile:
    if declared_size < 0 or declared_size > MAX_PAYLOAD_BYTES:
        raise ValueError(f"archive member {label!r} exceeds the payload byte bound")
    if capture and declared_size > MAX_ARCHIVE_JSON_BYTES:
        raise ValueError(f"archive JSON member {label!r} exceeds the bounded JSON size")
    digest = hashlib.sha256()
    content = bytearray() if capture else None
    actual_size = 0
    while True:
        chunk = stream.read(min(1024 * 1024, declared_size - actual_size + 1))
        if not chunk:
            break
        actual_size += len(chunk)
        if actual_size > declared_size:
            raise ValueError(f"archive member {label!r} exceeds its declared size")
        digest.update(chunk)
        if content is not None:
            content.extend(chunk)
    if actual_size != declared_size:
        raise ValueError(f"archive member {label!r} is truncated")
    return ArchiveFile(
        size_bytes=actual_size,
        sha256=digest.hexdigest(),
        content=bytes(content) if content is not None else None,
        mode=mode,
    )


def record_archive_file(
    files: dict[str, ArchiveFile],
    seen: set[str],
    *,
    name: str,
    stream: object,
    declared_size: int,
    capture_names: set[str],
    mode: int | None,
    total_bytes: int,
) -> int:
    if name in seen:
        raise ValueError(f"archive contains duplicate path {name!r}")
    reject_conflicting_archive_hierarchy(files, seen, name, regular_file=True)
    seen.add(name)
    if len(files) >= MAX_PAYLOAD_FILES:
        raise ValueError(f"archive exceeds {MAX_PAYLOAD_FILES} files")
    if total_bytes + declared_size > MAX_PAYLOAD_BYTES:
        raise ValueError(f"archive exceeds {MAX_PAYLOAD_BYTES} decompressed bytes")
    entry = read_archive_file(
        stream,
        declared_size=declared_size,
        capture=name in capture_names,
        label=name,
        mode=mode,
    )
    files[name] = entry
    return total_bytes + entry.size_bytes


def inspect_tar_gzip_archive(
    path: Path, capture_names: set[str], archive_root: str
) -> dict[str, ArchiveFile]:
    files: dict[str, ArchiveFile] = {}
    seen: set[str] = set()
    total_bytes = 0
    member_count = 0
    try:
        with tarfile.open(path, mode="r:gz") as archive:
            for member in archive:
                member_count += 1
                if member_count > MAX_PAYLOAD_FILES:
                    raise ValueError(f"archive exceeds {MAX_PAYLOAD_FILES} members")
                name = canonical_archive_path(member.name, is_directory=member.isdir())
                if name is None:
                    continue
                require_archive_root(name, archive_root, is_directory=member.isdir())
                if name in seen:
                    raise ValueError(f"archive contains duplicate path {name!r}")
                if member.isdir():
                    reject_conflicting_archive_hierarchy(
                        files, seen, name, regular_file=False
                    )
                    seen.add(name)
                    continue
                if not member.isfile():
                    raise ValueError(f"archive member {name!r} is not a regular file")
                stream = archive.extractfile(member)
                if stream is None:
                    raise ValueError(f"archive member {name!r} cannot be read")
                with stream:
                    total_bytes = record_archive_file(
                        files,
                        seen,
                        name=name,
                        stream=stream,
                        declared_size=member.size,
                        capture_names=capture_names,
                        mode=member.mode,
                        total_bytes=total_bytes,
                    )
    except (tarfile.TarError, EOFError) as error:
        raise ValueError("Homebrew archive is not a valid bounded tar.gz") from error
    return files


def inspect_zip_archive(
    path: Path, capture_names: set[str], archive_root: str
) -> dict[str, ArchiveFile]:
    files: dict[str, ArchiveFile] = {}
    seen: set[str] = set()
    total_bytes = 0
    try:
        with zipfile.ZipFile(path, mode="r") as archive:
            members = archive.infolist()
            if len(members) > MAX_PAYLOAD_FILES:
                raise ValueError(f"archive exceeds {MAX_PAYLOAD_FILES} members")
            for member in members:
                if member.flag_bits & 0x1:
                    raise ValueError("WinGet archive must not contain encrypted entries")
                if member.compress_type not in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED}:
                    raise ValueError("WinGet archive uses an unsupported compression method")
                is_directory = member.is_dir()
                name = canonical_archive_path(member.filename, is_directory=is_directory)
                if name is None:
                    continue
                require_archive_root(name, archive_root, is_directory=is_directory)
                if name in seen:
                    raise ValueError(f"archive contains duplicate path {name!r}")
                mode = (member.external_attr >> 16) & 0xFFFF
                file_type = stat.S_IFMT(mode)
                if is_directory:
                    if file_type not in {0, stat.S_IFDIR}:
                        raise ValueError(f"archive directory {name!r} has an unsafe file type")
                    reject_conflicting_archive_hierarchy(
                        files, seen, name, regular_file=False
                    )
                    seen.add(name)
                    continue
                if file_type not in {0, stat.S_IFREG}:
                    raise ValueError(f"archive member {name!r} is not a regular file")
                with archive.open(member, mode="r") as stream:
                    total_bytes = record_archive_file(
                        files,
                        seen,
                        name=name,
                        stream=stream,
                        declared_size=member.file_size,
                        capture_names=capture_names,
                        mode=mode or None,
                        total_bytes=total_bytes,
                    )
    except (zipfile.BadZipFile, EOFError) as error:
        raise ValueError("WinGet archive is not a valid bounded ZIP") from error
    return files


def require_archive_payload(
    files: dict[str, ArchiveFile], runtime_relative: str
) -> None:
    required = {*REQUIRED_PAYLOAD_FILES, runtime_relative}
    missing = sorted(required.difference(files))
    if missing:
        raise ValueError(f"archive is missing canonical payload file {missing[0]!r}")
    if not any(
        name.startswith("schemas/nexus.cua.v1/") and name.endswith(".json")
        for name in files
    ):
        raise ValueError("archive schema directory has no JSON schema")


def archive_json(files: dict[str, ArchiveFile], name: str) -> dict:
    content = files[name].content
    if content is None:
        raise ValueError(f"archive validator did not capture required JSON {name!r}")
    return decode_json_object(content, f"archive {name}")


def validate_channel_archive(
    archive: Path,
    candidate_manifest: Path,
    *,
    archive_format: str,
    target: dict,
    version: str,
    candidate_kind: str,
) -> None:
    if not candidate_manifest.is_file() or candidate_manifest.is_symlink():
        raise ValueError("candidate manifest must be a regular non-symlink file")
    if candidate_manifest.stat().st_size > MAX_ARCHIVE_JSON_BYTES:
        raise ValueError("external candidate manifest exceeds the bounded JSON size")
    external_manifest_bytes = candidate_manifest.read_bytes()
    external_manifest = decode_json_object(
        external_manifest_bytes, "external candidate manifest"
    )
    runtime_sha256 = external_manifest.get("tested_runtime_sha256")
    check_digest(runtime_sha256, "candidate runtime")
    validate_candidate_manifest(
        external_manifest,
        version=version,
        candidate_kind=candidate_kind,
        platform=target["manifest_platform"],
        architecture=target["manifest_architecture"],
        runtime_sha256=runtime_sha256,
    )
    archive_root = f"nexus-cua-v{version}-{target['archive_platform']}"
    relative_captures = {
        "manifest.json",
        "native-evidence-summary.json",
        "remote-transport-summary.json",
        "codex-acceptance-summary.json",
        *(f"profiles/{name}.json" for name in PROFILE_EXPECTATIONS),
    }
    captured = {f"{archive_root}/{name}" for name in relative_captures}
    if archive_format == "tar.gz":
        archived_files = inspect_tar_gzip_archive(archive, captured, archive_root)
    elif archive_format == "zip":
        archived_files = inspect_zip_archive(archive, captured, archive_root)
    else:
        raise ValueError(f"unsupported channel archive format: {archive_format}")
    prefix = f"{archive_root}/"
    files = {
        name.removeprefix(prefix): entry for name, entry in archived_files.items()
    }
    runtime_relative = target["runtime"]
    require_archive_payload(files, runtime_relative)

    embedded_manifest_bytes = files["manifest.json"].content
    if embedded_manifest_bytes != external_manifest_bytes:
        raise ValueError("archive does not contain the exact external candidate manifest")
    embedded_manifest = archive_json(files, "manifest.json")
    validate_candidate_manifest(
        embedded_manifest,
        version=version,
        candidate_kind=candidate_kind,
        platform=target["manifest_platform"],
        architecture=target["manifest_architecture"],
        runtime_sha256=runtime_sha256,
    )
    runtime = files[runtime_relative]
    check_record(
        embedded_manifest["files"].get("runtime"),
        expected_name=runtime_relative,
        expected_sha256=runtime.sha256,
        expected_size=runtime.size_bytes,
        label="runtime",
    )
    if archive_format == "tar.gz" and (
        runtime.mode is None or runtime.mode & 0o111 == 0
    ):
        raise ValueError("Homebrew runtime is not executable in the archive")

    evidence_names = {
        "native_evidence": "native-evidence-summary.json",
        "remote_transport": "remote-transport-summary.json",
        "codex_acceptance": "codex-acceptance-summary.json",
    }
    for key, name in evidence_names.items():
        evidence = files[name]
        check_record(
            embedded_manifest["files"].get(key),
            expected_name=name,
            expected_sha256=evidence.sha256,
            expected_size=evidence.size_bytes,
            label=key,
        )
    source_revision = embedded_manifest["source_revision"]
    validate_native_evidence(
        archive_json(files, evidence_names["native_evidence"]),
        candidate_kind=candidate_kind,
        source_revision=source_revision,
        platform_name=target["manifest_platform"],
        architecture=target["acceptance_architecture"],
        runtime_sha256=runtime_sha256,
    )
    validate_remote_evidence(
        archive_json(files, evidence_names["remote_transport"]), source_revision
    )
    validate_codex_evidence(
        archive_json(files, evidence_names["codex_acceptance"]),
        version=version,
        source_revision=source_revision,
        platform_name=target["manifest_platform"],
        architecture=target["acceptance_architecture"],
        runtime_sha256=runtime_sha256,
    )
    for name in PROFILE_EXPECTATIONS:
        validate_install_profile(
            archive_json(files, f"profiles/{name}.json"), name, version
        )


def create_directory(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.mkdir(mode=0o755, exist_ok=False)


def write_json(path: Path, value: dict) -> None:
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")


def write_text(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        stream.write(value)


def stage_npm_meta(args: argparse.Namespace, channels: dict) -> None:
    check_version(args.version)
    source = args.meta_source
    package = load_json(source / "package.json")
    if package.get("name") != channels["npm"]["meta_package"]:
        raise ValueError("npm meta package name differs from channel contract")
    launcher = source / "bin/nexuscua.js"
    if not launcher.is_file() or launcher.is_symlink():
        raise ValueError("npm meta launcher is missing or is a symlink")
    package["version"] = args.version
    package["optionalDependencies"] = {
        entry["package"]: args.version
        for entry in channels["npm"]["platforms"].values()
    }
    if "scripts" in package:
        raise ValueError("npm meta package must not contain lifecycle scripts")
    create_directory(args.output)
    try:
        (args.output / "bin").mkdir()
        shutil.copyfile(launcher, args.output / "bin/nexuscua.js")
        os.chmod(args.output / "bin/nexuscua.js", 0o755)
        write_json(args.output / "package.json", package)
    except Exception:
        shutil.rmtree(args.output)
        raise


def stage_npm_platform(args: argparse.Namespace, channels: dict) -> None:
    check_version(args.version)
    check_digest(args.runtime_sha256, "runtime digest")
    try:
        target = channels["npm"]["platforms"][args.target]
    except KeyError as error:
        raise ValueError(f"unsupported npm platform target: {args.target}") from error
    runtime_relative = Path(target["runtime"])
    files = inventory_payload(args.payload_root, runtime_relative)
    validate_install_profiles(args.payload_root, args.version)
    runtime = args.payload_root / runtime_relative
    actual_runtime_sha256 = sha256(runtime)
    if actual_runtime_sha256 != args.runtime_sha256:
        raise ValueError(
            "runtime digest mismatch: "
            f"expected={args.runtime_sha256}, actual={actual_runtime_sha256}"
        )
    manifest = check_candidate_manifest(
        args.payload_root / "manifest.json",
        version=args.version,
        candidate_kind=args.candidate_kind,
        platform=target["manifest_platform"],
        architecture=target["manifest_architecture"],
        runtime_sha256=args.runtime_sha256,
    )
    check_file_record(
        manifest["files"].get("runtime"),
        runtime,
        "runtime",
        expected_name=runtime_relative.as_posix(),
    )
    validate_payload_evidence(
        args.payload_root,
        manifest,
        version=args.version,
        candidate_kind=args.candidate_kind,
        platform_name=target["manifest_platform"],
        acceptance_architecture=target["acceptance_architecture"],
        runtime_sha256=args.runtime_sha256,
    )
    package = {
        "name": target["package"],
        "version": args.version,
        "description": f"Signed Nexus Computer Use Runtime payload for {args.target}",
        "license": "MIT",
        "repository": {
            "type": "git",
            "url": "git+https://github.com/nexus-research-lab/nexus-cua.git",
        },
        "os": [target["os"]],
        "cpu": [target["cpu"]],
        "files": ["payload", "runtime.sha256"],
        "exports": {"./binary": f"./payload/{runtime_relative.as_posix()}"},
        "publishConfig": {"access": "public"},
    }
    create_directory(args.output)
    try:
        payload_output = args.output / "payload"
        payload_output.mkdir()
        for source in files:
            relative = source.relative_to(args.payload_root)
            destination = payload_output / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
        copied_runtime = payload_output / runtime_relative
        if sha256(copied_runtime) != args.runtime_sha256:
            raise ValueError("runtime changed while the npm package was being staged")
        validate_install_profiles(payload_output, args.version)
        copied_manifest = check_candidate_manifest(
            payload_output / "manifest.json",
            version=args.version,
            candidate_kind=args.candidate_kind,
            platform=target["manifest_platform"],
            architecture=target["manifest_architecture"],
            runtime_sha256=args.runtime_sha256,
        )
        check_file_record(
            copied_manifest["files"].get("runtime"),
            copied_runtime,
            "runtime",
            expected_name=runtime_relative.as_posix(),
        )
        validate_payload_evidence(
            payload_output,
            copied_manifest,
            version=args.version,
            candidate_kind=args.candidate_kind,
            platform_name=target["manifest_platform"],
            acceptance_architecture=target["acceptance_architecture"],
            runtime_sha256=args.runtime_sha256,
        )
        if target["os"] != "win32":
            os.chmod(copied_runtime, 0o755)
        write_text(args.output / "runtime.sha256", f"{args.runtime_sha256}\n")
        write_json(args.output / "package.json", package)
    except Exception:
        shutil.rmtree(args.output)
        raise


def generate_homebrew(args: argparse.Namespace, channels: dict) -> None:
    check_version(args.version)
    check_archive(args.archive, args.archive_sha256)
    check_url(args.url, allow_file=True)
    validate_channel_archive(
        args.archive,
        args.candidate_manifest,
        archive_format="tar.gz",
        target=channels["npm"]["platforms"]["darwin-arm64"],
        version=args.version,
        candidate_kind=args.candidate_kind,
    )
    formula_class = channels["homebrew"]["class"]
    formula = f'''class {formula_class} < Formula
  desc "Model-neutral, cross-platform computer-use runtime"
  homepage "https://github.com/nexus-research-lab/nexus-cua"
  url "{args.url}"
  version "{args.version}"
  sha256 "{args.archive_sha256}"
  license "MIT"

  depends_on macos: :sonoma

  def install
    bin.install "bin/nexuscua"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/nexuscua --version")
  end
end
'''
    write_text(args.output, formula)


def generate_winget(args: argparse.Namespace, channels: dict) -> None:
    check_version(args.version)
    check_archive(args.archive, args.archive_sha256)
    check_url(args.url, allow_file=False)
    validate_channel_archive(
        args.archive,
        args.candidate_manifest,
        archive_format="zip",
        target=channels["npm"]["platforms"]["win32-x64"],
        version=args.version,
        candidate_kind=args.candidate_kind,
    )
    identifier = channels["winget"]["package_identifier"]
    manifest_version = channels["winget"]["manifest_version"]
    create_directory(args.output)
    try:
        prefix = f"{identifier}-{args.version}"
        write_text(
            args.output / f"{prefix}.yaml",
            f'''PackageIdentifier: {identifier}
PackageVersion: {args.version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: {manifest_version}
''',
        )
        write_text(
            args.output / f"{prefix}.installer.yaml",
            f'''PackageIdentifier: {identifier}
PackageVersion: {args.version}
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: nexus-cua-v{args.version}-windows-x86_64\\bin\\nexuscua.exe
    PortableCommandAlias: nexuscua
Scope: user
UpgradeBehavior: install
Installers:
  - Architecture: x64
    InstallerUrl: {args.url}
    InstallerSha256: {args.archive_sha256.upper()}
ManifestType: installer
ManifestVersion: {manifest_version}
''',
        )
        write_text(
            args.output / f"{prefix}.locale.en-US.yaml",
            f'''PackageIdentifier: {identifier}
PackageVersion: {args.version}
PackageLocale: en-US
Publisher: Nexus Research Lab
PublisherUrl: https://github.com/nexus-research-lab
PackageName: Nexus Computer Use Runtime
PackageUrl: https://github.com/nexus-research-lab/nexus-cua
License: MIT
LicenseUrl: https://github.com/nexus-research-lab/nexus-cua/blob/main/LICENSE
ShortDescription: Model-neutral, cross-platform computer-use runtime
Tags:
  - automation
  - computer-use
  - mcp
ManifestType: defaultLocale
ManifestVersion: {manifest_version}
''',
        )
    except Exception:
        shutil.rmtree(args.output)
        raise


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    root.add_argument(
        "--channels",
        type=Path,
        default=Path("packaging/channels.json"),
    )
    commands = root.add_subparsers(dest="command", required=True)

    meta = commands.add_parser("npm-meta")
    meta.add_argument("--version", required=True)
    meta.add_argument(
        "--meta-source",
        type=Path,
        default=Path("packaging/npm/nexus-cua"),
    )
    meta.add_argument("--output", type=Path, required=True)

    platform = commands.add_parser("npm-platform")
    platform.add_argument("--target", required=True)
    platform.add_argument("--version", required=True)
    platform.add_argument("--candidate-kind", choices=sorted(ACCEPTED_STATES), required=True)
    platform.add_argument("--payload-root", type=Path, required=True)
    platform.add_argument("--runtime-sha256", required=True)
    platform.add_argument("--output", type=Path, required=True)

    homebrew = commands.add_parser("homebrew")
    add_archive_arguments(homebrew)
    homebrew.add_argument("--output", type=Path, required=True)

    winget = commands.add_parser("winget")
    add_archive_arguments(winget)
    winget.add_argument("--output", type=Path, required=True)
    return root


def add_archive_arguments(command: argparse.ArgumentParser) -> None:
    command.add_argument("--version", required=True)
    command.add_argument("--candidate-kind", choices=sorted(ACCEPTED_STATES), required=True)
    command.add_argument("--candidate-manifest", type=Path, required=True)
    command.add_argument("--archive", type=Path, required=True)
    command.add_argument("--archive-sha256", required=True)
    command.add_argument("--url", required=True)


def main() -> int:
    args = parser().parse_args()
    try:
        channels = load_channels(args.channels)
        if args.command == "npm-meta":
            stage_npm_meta(args, channels)
        elif args.command == "npm-platform":
            stage_npm_platform(args, channels)
        elif args.command == "homebrew":
            generate_homebrew(args, channels)
        else:
            generate_winget(args, channels)
    except (KeyError, OSError, ValueError, json.JSONDecodeError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
