import argparse
import json
import tempfile
import unittest
from pathlib import Path

import release_candidate


class ReleaseCandidateTests(unittest.TestCase):
    def test_validate_evidence_binds_runtime_and_revision(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = root / "nexus-cua"
            runtime.write_bytes(b"tested runtime")
            runner_manifest = root / "runner.json"
            runner_manifest.write_text(
                json.dumps({"status": "active"}), encoding="utf-8"
            )
            evidence = root / "summary.json"
            evidence.write_text(
                json.dumps(
                    {
                        "evidence_contract": release_candidate.EVIDENCE_CONTRACT,
                        "requested_evidence_class": "release_accepted",
                        "evidence_class": "release_accepted",
                        "class_requirement_satisfied": True,
                        "required_soak_profile": "release",
                        "source_revision": "abc123",
                        "runtime_sha256": release_candidate.sha256(runtime),
                        "tested_payload_digests": {
                            "runtime_sha256": release_candidate.sha256(runtime)
                        },
                        "platform": "macos",
                        "architecture": "aarch64",
                        "runner": {"status": "active"},
                        "runner_manifest_sha256": release_candidate.sha256(
                            runner_manifest
                        ),
                        "required_report_digests": {
                            "validation.json": {
                                "sha256": "b" * 64,
                                "size_bytes": 123,
                            }
                        },
                        "issues": [],
                    }
                ),
                encoding="utf-8",
            )

            release_candidate.validate_evidence(
                evidence,
                runtime,
                "abc123",
                "macos",
                "aarch64",
                "release_accepted",
                runner_manifest,
            )

            runtime.write_bytes(b"rebuilt elsewhere")
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                release_candidate.validate_evidence(
                    evidence,
                    runtime,
                    "abc123",
                    "macos",
                    "aarch64",
                    "release_accepted",
                    runner_manifest,
                )

    def test_validate_evidence_rejects_class_downgrade(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = root / "nexus-cua"
            runtime.write_bytes(b"tested runtime")
            runner_manifest = root / "runner.json"
            runner_manifest.write_text(
                json.dumps({"status": "active"}), encoding="utf-8"
            )
            evidence = root / "summary.json"
            evidence.write_text(
                json.dumps(
                    {
                        "evidence_contract": release_candidate.EVIDENCE_CONTRACT,
                        "requested_evidence_class": "engineering_accepted",
                        "evidence_class": "engineering_accepted",
                        "class_requirement_satisfied": True,
                        "required_soak_profile": "engineering",
                        "source_revision": "abc123",
                        "runtime_sha256": release_candidate.sha256(runtime),
                        "tested_payload_digests": {
                            "runtime_sha256": release_candidate.sha256(runtime)
                        },
                        "platform": "macos",
                        "architecture": "aarch64",
                        "runner": {"status": "active"},
                        "runner_manifest_sha256": release_candidate.sha256(
                            runner_manifest
                        ),
                        "required_report_digests": {
                            "validation.json": {
                                "sha256": "b" * 64,
                                "size_bytes": 123,
                            }
                        },
                        "issues": [],
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(ValueError, "requested_evidence_class"):
                release_candidate.validate_evidence(
                    evidence,
                    runtime,
                    "abc123",
                    "macos",
                    "aarch64",
                    "release_accepted",
                    runner_manifest,
                )

    def test_check_version_matches_workspace_package(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text(
                '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "1.2.3"\n',
                encoding="utf-8",
            )
            release_candidate.check_version("1.2.3", manifest)
            with self.assertRaisesRegex(ValueError, "does not match"):
                release_candidate.check_version("1.2.4", manifest)

    def test_manifest_records_signed_payload_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cargo_manifest = root / "Cargo.toml"
            cargo_manifest.write_text(
                '[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8"
            )
            runtime = root / "nexus-cua"
            runtime.write_bytes(b"signed runtime")
            evidence = root / "summary.json"
            evidence.write_text(
                json.dumps(
                    {
                        "tested_payload_digests": {
                            "runtime_sha256": release_candidate.sha256(runtime)
                        }
                    }
                ),
                encoding="utf-8",
            )
            paths = {}
            for name in ("sbom", "signature", "smoke"):
                path = root / f"{name}.txt"
                path.write_text(name, encoding="utf-8")
                paths[name] = path
            output = root / "manifest.json"
            result = release_candidate.build_manifest(
                argparse.Namespace(
                    version="1.2.3",
                    candidate_kind="engineering",
                    protocol_version="nexus.cua.v1",
                    source_revision="abc123",
                    platform="macos",
                    architecture="universal2",
                    runtime=runtime,
                    sbom=paths["sbom"],
                    evidence=evidence,
                    signature_evidence=paths["signature"],
                    smoke_evidence=paths["smoke"],
                    output=output,
                    cargo_manifest=cargo_manifest,
                )
            )
            self.assertEqual(result["release_state"], "unaccepted_legacy")
            self.assertEqual(result["candidate_kind"], "engineering")
            self.assertEqual(result["files"]["smoke_evidence"]["path"], "smoke.txt")


if __name__ == "__main__":
    unittest.main()
