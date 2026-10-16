import argparse
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

import package_channels
import release_candidate


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
CHANNELS_PATH = REPOSITORY_ROOT / "packaging/channels.json"
META_SOURCE = REPOSITORY_ROOT / "packaging/npm/nexus-cua"


class PackageChannelTests(unittest.TestCase):
    def setUp(self) -> None:
        self.channels = package_channels.load_channels(CHANNELS_PATH)

    def test_committed_channel_metadata_matches_workspace_version(self) -> None:
        version = release_candidate.workspace_version(REPOSITORY_ROOT / "Cargo.toml")
        meta = package_channels.load_json(META_SOURCE / "package.json")
        self.assertEqual(meta["version"], version)
        self.assertEqual(
            set(meta["optionalDependencies"].values()),
            {version},
        )
        package_channels.validate_install_profiles(REPOSITORY_ROOT, version)
        launcher = (META_SOURCE / "bin/nexuscua.js").read_text(encoding="utf-8")
        for target in self.channels["npm"]["platforms"].values():
            self.assertIn(target["package"], launcher)

    def create_payload(
        self,
        root: Path,
        *,
        target: str = "darwin-arm64",
        state: str = "engineering_candidate",
        candidate_kind: str = "engineering",
    ) -> tuple[Path, str]:
        payload = root / "payload"
        target_config = self.channels["npm"]["platforms"][target]
        runtime = payload / target_config["runtime"]
        runtime.parent.mkdir(parents=True)
        if target == "darwin-arm64":
            runtime.write_bytes(b"#!/bin/sh\nprintf 'nexuscua 1.2.3\\n'\n")
            runtime.chmod(0o755)
        else:
            runtime.write_bytes(b"signed runtime fixture")
        runtime_sha256 = package_channels.sha256(runtime)
        files = {
            "README.md": "readme\n",
            "PRD.zh-CN.md": "prd\n",
            "LICENSE": "MIT\n",
            "THIRD_PARTY_NOTICES.md": "notices\n",
            "sbom.spdx.json": "{}\n",
            "provenance.intoto.jsonl": "{}\n",
            "skills/common/instructions.md": "common\n",
            "skills/cli/SKILL.md": "cli\n",
            "skills/mcp/SKILL.md": "mcp\n",
            "schemas/nexus.cua.v1/request.json": "{}\n",
        }
        for relative, contents in files.items():
            path = payload / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents, encoding="utf-8")
        source_revision = "a" * 40
        evidence_class, soak_profile = package_channels.NATIVE_EVIDENCE_CLASS[
            candidate_kind
        ]
        native_evidence = {
            "evidence_contract": "nexus.cua.native-evidence.v2",
            "requested_evidence_class": evidence_class,
            "evidence_class": evidence_class,
            "class_requirement_satisfied": True,
            "required_soak_profile": soak_profile,
            "source_revision": source_revision,
            "platform": target_config["manifest_platform"],
            "architecture": target_config["acceptance_architecture"],
            "runtime_sha256": runtime_sha256,
            "tested_payload_digests": {"runtime_sha256": runtime_sha256},
            "runner": {"status": "active"},
            "runner_manifest_sha256": "b" * 64,
            "required_report_digests": {
                "native.json": {"sha256": "c" * 64, "size_bytes": 123}
            },
            "issues": [],
        }
        remote_evidence = {
            "schema_version": "nexus.cua.remote-transport.v1",
            "source_revision": source_revision,
            "controller_binary_sha256": "d" * 64,
            "worker_binary_sha256": "d" * 64,
            "certificate_identity_sha256": "e" * 64,
            "transport": "tcp+mutual_tls_1_2_or_1_3",
            "gateway_protocol": "nexus.cua.gateway.v1",
            "process_count": 2,
            "resource_bounds": {
                "max_connections": 9,
                "max_frame_bytes": 65536,
                "max_inflight_requests": 4,
                "max_artifact_tickets": 4,
                "max_artifact_bytes": 4096,
                "max_request_timeout_ms": 5000,
                "mutation_ledger_entries": 16,
            },
            "scenarios": [
                {
                    "scenario": scenario,
                    "disposition": "accepted",
                    "accepted": True,
                    "evidence": "bounded conformance",
                }
                for scenario in sorted(package_channels.REMOTE_SCENARIOS)
            ],
            "accepted": True,
            "issues": [],
        }
        sequence = list(package_channels.CODEX_REQUIRED_SEQUENCE)
        cleanup = {
            "fixture_absent": True,
            "registration_removed": True,
            "artifacts_removed": True,
        }

        def business_attempt(index: int) -> dict:
            return {
                "attempt_index": index,
                "status": "accepted",
                "actual_tool_sequence": sequence,
                "tool_events": [
                    {"server": "nexuscua", "tool": tool} for tool in sequence
                ],
                "request_ids": {
                    "launch": f"fixture:run{index}:launch",
                    "click": f"fixture:run{index}:click",
                    "toggle": f"fixture:run{index}:toggle",
                    "terminate": f"fixture:run{index}:terminate",
                },
                "state_assertions": {
                    "initial_counter_zero": True,
                    "initial_checked_false": True,
                    "counter_one_after_click": True,
                    "checked_true_after_toggle": True,
                    "final_counter_one": True,
                    "final_checked_true": True,
                    "final_applications_empty": True,
                    "final_windows_empty": True,
                },
                "timings_ms": {"total": 1},
                "policy_violations": [],
                "cleanup": cleanup,
            }

        codex_evidence = {
            "schema_version": "nexus.cua.codex-acceptance.v1",
            "product_version": "1.2.3",
            "protocol_version": "nexus.cua.v1",
            "source_revision": source_revision,
            "platform": target_config["manifest_platform"],
            "architecture": target_config["acceptance_architecture"],
            "runtime_sha256": runtime_sha256,
            "runner_manifest_sha256": "f" * 64,
            "os_build": "test-os-build",
            "codex_version": "codex-cli test",
            "model_provider": "test-provider",
            "model_id": "test-model",
            "reasoning_configuration": "test",
            "prompt_sha256": "1" * 64,
            "tool_schema_sha256": "2" * 64,
            "skill_sha256": "3" * 64,
            "mcp_profile": "full",
            "provider_identity_sha256": "4" * 64,
            "authority_scope_sha256": "5" * 64,
            "allowed_tool_sequence": list(dict.fromkeys(sequence)),
            "business_attempts": [business_attempt(index) for index in range(1, 4)],
            "fault_scenario": {
                "status": "accepted",
                "tool_sequence": ["perform_action", "reconcile_mutation"],
                "original_request_id": "fixture:fault:action",
                "reconciled_request_id": "fixture:fault:action",
                "dispatch_count": 1,
                "effect_count": 1,
                "disposition": "succeeded",
                "policy_violations": [],
                "cleanup": cleanup,
            },
            "infrastructure_attempts": [],
            "cleanup": cleanup,
            "accepted": True,
            "policy_violations": [],
            "issues": [],
        }
        evidence_values = {
            "native-evidence-summary.json": native_evidence,
            "remote-transport-summary.json": remote_evidence,
            "codex-acceptance-summary.json": codex_evidence,
        }
        for relative, value in evidence_values.items():
            (payload / relative).write_text(json.dumps(value), encoding="utf-8")
        for name, expectations in package_channels.PROFILE_EXPECTATIONS.items():
            profile = {
                "contract": "nexus.cua.install-profile.v1",
                "profile": name,
                "product_version": "1.2.3",
                "protocol_version": "nexus.cua.v1",
                "bare_install_activates": False,
                "automatic_service_start": False,
                "automatic_permissions": False,
                **expectations,
            }
            path = payload / f"profiles/{name}.json"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(profile), encoding="utf-8")
        manifest = {
            "contract": "nexus.cua.release-candidate.v1",
            "version": "1.2.3",
            "candidate_kind": candidate_kind,
            "release_state": state,
            "protocol_version": "nexus.cua.v1",
            "source_revision": source_revision,
            "platform": target_config["manifest_platform"],
            "architecture": target_config["manifest_architecture"],
            "tested_runtime_sha256": runtime_sha256,
            "files": {
                "runtime": {
                    "path": target_config["runtime"],
                    "sha256": runtime_sha256,
                    "size_bytes": runtime.stat().st_size,
                },
                **{
                    key: {
                        "path": path.name,
                        "sha256": package_channels.sha256(path),
                        "size_bytes": path.stat().st_size,
                    }
                    for key, path in {
                        "native_evidence": payload
                        / "native-evidence-summary.json",
                        "remote_transport": payload
                        / "remote-transport-summary.json",
                        "codex_acceptance": payload
                        / "codex-acceptance-summary.json",
                    }.items()
                },
            },
        }
        (payload / "manifest.json").write_text(
            json.dumps(manifest), encoding="utf-8"
        )
        return payload, runtime_sha256

    def create_tar_gzip(
        self, payload: Path, archive: Path, archive_root: str
    ) -> None:
        with tarfile.open(archive, mode="w:gz") as bundle:
            bundle.add(payload, arcname=archive_root)

    def create_zip(self, payload: Path, archive: Path, archive_root: str) -> None:
        with zipfile.ZipFile(archive, mode="w", compression=zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(payload.rglob("*")):
                if path.is_file():
                    relative = path.relative_to(payload).as_posix()
                    bundle.write(path, f"{archive_root}/{relative}")

    def test_npm_packages_pack_without_scripts_or_downloads(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload, runtime_sha256 = self.create_payload(root)
            meta_output = root / "meta"
            platform_output = root / "platform"
            package_channels.stage_npm_meta(
                argparse.Namespace(
                    version="1.2.3",
                    meta_source=META_SOURCE,
                    output=meta_output,
                ),
                self.channels,
            )
            package_channels.stage_npm_platform(
                argparse.Namespace(
                    target="darwin-arm64",
                    version="1.2.3",
                    candidate_kind="engineering",
                    payload_root=payload,
                    runtime_sha256=runtime_sha256,
                    output=platform_output,
                ),
                self.channels,
            )

            meta = package_channels.load_json(meta_output / "package.json")
            platform_package = package_channels.load_json(
                platform_output / "package.json"
            )
            self.assertNotIn("scripts", meta)
            self.assertNotIn("scripts", platform_package)
            self.assertEqual(
                set(meta["optionalDependencies"]),
                {
                    "@nexus-research-lab/nexus-cua-darwin-arm64",
                    "@nexus-research-lab/nexus-cua-win32-x64",
                },
            )
            self.assertTrue(
                all(version == "1.2.3" for version in meta["optionalDependencies"].values())
            )
            self.assertEqual(platform_package["os"], ["darwin"])
            self.assertEqual(platform_package["cpu"], ["arm64"])
            self.assertEqual(
                (platform_output / "runtime.sha256").read_text(encoding="utf-8"),
                f"{runtime_sha256}\n",
            )

            npm = shutil.which("npm")
            self.assertIsNotNone(npm, "npm is required by the distribution source gate")
            pack_output = root / "packed"
            pack_output.mkdir()
            npm_environment = os.environ.copy()
            npm_environment["npm_config_cache"] = str(root / "npm-cache")
            for package_root in (meta_output, platform_output):
                subprocess.run(
                    [
                        npm,
                        "pack",
                        "--ignore-scripts",
                        "--pack-destination",
                        str(pack_output),
                    ],
                    cwd=package_root,
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=npm_environment,
                )
            self.assertEqual(len(list(pack_output.glob("*.tgz"))), 2)
            if sys.platform == "darwin" and platform.machine() == "arm64":
                prefix = root / "npm-prefix"
                platform_archive = next(
                    pack_output.glob(
                        "nexus-research-lab-nexus-cua-darwin-arm64-*.tgz"
                    )
                )
                meta_archive = next(pack_output.glob("nexus-cua-*.tgz"))
                subprocess.run(
                    [
                        npm,
                        "install",
                        "--global",
                        "--prefix",
                        str(prefix),
                        "--offline",
                        "--ignore-scripts",
                        "--no-audit",
                        "--no-fund",
                        str(platform_archive),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=npm_environment,
                )
                subprocess.run(
                    [
                        npm,
                        "install",
                        "--global",
                        "--prefix",
                        str(prefix),
                        "--omit=optional",
                        "--offline",
                        "--ignore-scripts",
                        "--no-audit",
                        "--no-fund",
                        str(meta_archive),
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=30,
                    env=npm_environment,
                )
                installed = subprocess.run(
                    [str(prefix / "bin/nexuscua"), "--version"],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
                self.assertEqual(installed.stdout, "nexuscua 1.2.3\n")

    def test_platform_package_rejects_digest_and_candidate_state(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload, runtime_sha256 = self.create_payload(
                root, state="unaccepted_legacy"
            )
            args = argparse.Namespace(
                target="darwin-arm64",
                version="1.2.3",
                candidate_kind="engineering",
                payload_root=payload,
                runtime_sha256=runtime_sha256,
                output=root / "platform",
            )
            with self.assertRaisesRegex(ValueError, "release_state"):
                package_channels.stage_npm_platform(args, self.channels)

            manifest = package_channels.load_json(payload / "manifest.json")
            manifest["release_state"] = "engineering_candidate"
            (payload / "manifest.json").write_text(
                json.dumps(manifest), encoding="utf-8"
            )
            args.runtime_sha256 = "0" * 64
            with self.assertRaisesRegex(ValueError, "runtime digest mismatch"):
                package_channels.stage_npm_platform(args, self.channels)

    def test_platform_package_rejects_semantically_unaccepted_codex_evidence(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload, runtime_sha256 = self.create_payload(root)
            evidence_path = payload / "codex-acceptance-summary.json"
            evidence = package_channels.load_json(evidence_path)
            evidence["accepted"] = False
            evidence_path.write_text(json.dumps(evidence), encoding="utf-8")
            manifest_path = payload / "manifest.json"
            manifest = package_channels.load_json(manifest_path)
            manifest["files"]["codex_acceptance"] = {
                "path": evidence_path.name,
                "sha256": package_channels.sha256(evidence_path),
                "size_bytes": evidence_path.stat().st_size,
            }
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "Codex evidence accepted"):
                package_channels.stage_npm_platform(
                    argparse.Namespace(
                        target="darwin-arm64",
                        version="1.2.3",
                        candidate_kind="engineering",
                        payload_root=payload,
                        runtime_sha256=runtime_sha256,
                        output=root / "platform",
                    ),
                    self.channels,
                )

    def test_platform_package_rejects_runtime_record_outside_canonical_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload, runtime_sha256 = self.create_payload(root)
            manifest_path = payload / "manifest.json"
            manifest = package_channels.load_json(manifest_path)
            manifest["files"]["runtime"]["path"] = "nexuscua"
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "manifest path"):
                package_channels.stage_npm_platform(
                    argparse.Namespace(
                        target="darwin-arm64",
                        version="1.2.3",
                        candidate_kind="engineering",
                        payload_root=payload,
                        runtime_sha256=runtime_sha256,
                        output=root / "platform",
                    ),
                    self.channels,
                )

    def test_homebrew_and_winget_bind_exact_archive(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mac_payload, _ = self.create_payload(root / "mac")
            mac_manifest = mac_payload / "manifest.json"
            mac_archive = root / "candidate.tar.gz"
            self.create_tar_gzip(
                mac_payload, mac_archive, "nexus-cua-v1.2.3-macos-universal"
            )
            mac_archive_sha256 = package_channels.sha256(mac_archive)
            mac_common = {
                "version": "1.2.3",
                "candidate_kind": "engineering",
                "candidate_manifest": mac_manifest,
                "archive": mac_archive,
                "archive_sha256": mac_archive_sha256,
            }

            formula = root / "nexus-cua.rb"
            package_channels.generate_homebrew(
                argparse.Namespace(
                    **mac_common,
                    url="file:///tmp/nexus-cua-candidate.tar.gz",
                    output=formula,
                ),
                self.channels,
            )
            formula_text = formula.read_text(encoding="utf-8")
            self.assertIn(f'sha256 "{mac_archive_sha256}"', formula_text)
            ruby = shutil.which("ruby")
            if ruby is not None:
                subprocess.run(
                    [ruby, "-c", str(formula)],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=30,
                )
            if os.environ.get("NEXUS_CUA_VALIDATE_HOMEBREW") == "1":
                brew = shutil.which("brew")
                self.assertIsNotNone(brew, "Homebrew validation was explicitly required")
                result = subprocess.run(
                    [brew, "style", str(formula)],
                    capture_output=True,
                    text=True,
                    timeout=60,
                )
                self.assertEqual(
                    result.returncode,
                    0,
                    f"{result.stdout}\n{result.stderr}",
                )

            windows_payload, _ = self.create_payload(
                root / "windows", target="win32-x64"
            )
            windows_manifest = windows_payload / "manifest.json"
            windows_archive = root / "candidate.zip"
            self.create_zip(
                windows_payload, windows_archive, "nexus-cua-v1.2.3-windows-x86_64"
            )
            windows_archive_sha256 = package_channels.sha256(windows_archive)
            winget = root / "winget"
            package_channels.generate_winget(
                argparse.Namespace(
                    version="1.2.3",
                    candidate_kind="engineering",
                    candidate_manifest=windows_manifest,
                    archive=windows_archive,
                    archive_sha256=windows_archive_sha256,
                    url="https://example.invalid/nexus-cua-candidate.zip",
                    output=winget,
                ),
                self.channels,
            )
            generated = list(winget.glob("*.yaml"))
            self.assertEqual(len(generated), 3)
            installer = next(path for path in generated if ".installer." in path.name)
            installer_text = installer.read_text(encoding="utf-8")
            self.assertIn("NestedInstallerType: portable", installer_text)
            self.assertIn(
                "RelativeFilePath: "
                "nexus-cua-v1.2.3-windows-x86_64\\bin\\nexuscua.exe",
                installer_text,
            )
            self.assertIn(
                f"InstallerSha256: {windows_archive_sha256.upper()}", installer_text
            )
            winget_command = shutil.which("winget")
            if winget_command is not None:
                subprocess.run(
                    [winget_command, "validate", "--manifest", str(winget)],
                    check=True,
                    capture_output=True,
                    text=True,
                    timeout=60,
                )

    def test_channel_archive_rejects_manifest_mismatch_and_unsafe_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mac_payload, _ = self.create_payload(root / "mac")
            mac_manifest = mac_payload / "manifest.json"
            mac_archive = root / "candidate.tar.gz"
            self.create_tar_gzip(
                mac_payload, mac_archive, "nexus-cua-v1.2.3-macos-universal"
            )
            manifest_value = package_channels.load_json(mac_manifest)
            mac_manifest.write_text(
                json.dumps(manifest_value, indent=2), encoding="utf-8"
            )
            with self.assertRaisesRegex(ValueError, "exact external candidate manifest"):
                package_channels.generate_homebrew(
                    argparse.Namespace(
                        version="1.2.3",
                        candidate_kind="engineering",
                        candidate_manifest=mac_manifest,
                        archive=mac_archive,
                        archive_sha256=package_channels.sha256(mac_archive),
                        url="file:///tmp/nexus-cua-candidate.tar.gz",
                        output=root / "nexus-cua.rb",
                    ),
                    self.channels,
                )

            windows_payload, _ = self.create_payload(
                root / "windows", target="win32-x64"
            )
            windows_archive = root / "unsafe.zip"
            self.create_zip(
                windows_payload, windows_archive, "nexus-cua-v1.2.3-windows-x86_64"
            )
            with zipfile.ZipFile(windows_archive, mode="a") as bundle:
                bundle.writestr("../escape", b"unsafe")
            with self.assertRaisesRegex(ValueError, "unsafe or non-canonical path"):
                package_channels.generate_winget(
                    argparse.Namespace(
                        version="1.2.3",
                        candidate_kind="engineering",
                        candidate_manifest=windows_payload / "manifest.json",
                        archive=windows_archive,
                        archive_sha256=package_channels.sha256(windows_archive),
                        url="https://example.invalid/nexus-cua-candidate.zip",
                        output=root / "winget-unsafe",
                    ),
                    self.channels,
                )


if __name__ == "__main__":
    unittest.main()
