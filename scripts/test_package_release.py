"""Tests for the release packaging helper."""

from __future__ import annotations

import argparse
import json
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

import package_release  # noqa: E402


class PackageReleaseTests(unittest.TestCase):
    def test_tag_must_be_v_semver(self) -> None:
        self.assertEqual(package_release.version_from_tag("v0.2.0"), "0.2.0")
        self.assertEqual(package_release.version_from_tag("v0.3.0-rc.1"), "0.3.0-rc.1")
        for tag in ("0.2.0", "v0.2", "sdk/go/v0.2.0", "v0.2.0+build"):
            with self.assertRaises(ValueError):
                package_release.version_from_tag(tag)

    def test_cargo_tree_keeps_shipped_registry_crates_only(self) -> None:
        output = "\n".join(
            [
                "nexus-cua v0.2.0 (/work/crates/cli)",
                "serde v1.0.229",
                "serde_derive v1.0.229 (proc-macro)",
                "serde v1.0.229 (*)",
                "nexus-cua-runtime v0.2.0 (/work/crates/runtime) (*)",
                "forked v0.1.0 (https://github.com/example/forked#0123abc)",
                "",
            ]
        )
        self.assertEqual(
            package_release.parse_cargo_tree(output),
            [("serde", "1.0.229"), ("forked", "0.1.0")],
        )
        with self.assertRaises(ValueError):
            package_release.parse_cargo_tree("not a cargo tree line")

    def test_package_disables_cargo_tree_colors(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            runtime = root / "nexuscua"
            runtime.write_bytes(b"test runtime")
            args = argparse.Namespace(
                version=package_release.workspace_version(), platform="macos",
                architecture="arm64", target="aarch64-apple-darwin",
                toolchain="rustc 1.88.0", runtime=str(runtime),
                source_revision="a" * 40, signing_kind="ad-hoc",
                output_dir=str(root / "dist"),
            )
            with patch.object(package_release, "run", side_effect=["", '{"packages": []}']) as run:
                self.assertEqual(package_release.package(args), 0)
            command = run.call_args_list[0].args[0]
            self.assertEqual(command[:2], ["cargo", "tree"])
            self.assertIn("--color", command)
            self.assertEqual(command[command.index("--color") + 1], "never")

    def test_licenses_are_copied_or_recorded_without_downloads(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            licensed = root / "registry" / "licensed-1.0.0"
            licensed.mkdir(parents=True)
            (licensed / "Cargo.toml").write_text("")
            (licensed / "LICENSE-MIT").write_text("MIT text")
            (licensed / "src.rs").write_text("")
            bare = root / "registry" / "bare-2.0.0"
            bare.mkdir(parents=True)
            (bare / "Cargo.toml").write_text("")
            metadata = {
                "packages": [
                    {"name": "licensed", "version": "1.0.0", "manifest_path": str(licensed / "Cargo.toml"),
                     "license": "MIT", "source": "registry+https://github.com/rust-lang/crates.io-index"},
                    {"name": "bare", "version": "2.0.0", "manifest_path": str(bare / "Cargo.toml"),
                     "license": "Apache-2.0", "repository": "https://example.test/bare", "source": "registry+x"},
                ]
            }
            records = package_release.collect_licenses(
                [("licensed", "1.0.0"), ("bare", "2.0.0")], metadata, root / "out"
            )
            self.assertEqual([record["name"] for record in records], ["bare", "licensed"])
            self.assertEqual((root / "out/licensed-1.0.0/LICENSE-MIT").read_text(), "MIT text")
            self.assertFalse((root / "out/licensed-1.0.0/src.rs").exists())
            source = (root / "out/bare-2.0.0/SOURCE.txt").read_text()
            self.assertIn("Apache-2.0", source)
            self.assertIn("https://example.test/bare", source)
            with self.assertRaises(ValueError):
                package_release.collect_licenses([("missing", "0.0.1")], metadata, root / "other")

    def test_manifest_matches_host_installer_fields(self) -> None:
        manifest = package_release.build_manifest(
            version="0.2.0", revision="a" * 40, platform="windows", architecture="x86_64",
            target="x86_64-pc-windows-msvc", toolchain="rustc 1.88.0", signing_kind="unsigned",
            runtime_name="nexuscua.exe", runtime_sha256="b" * 64, sbom_sha256="c" * 64,
            notices_sha256="d" * 64, created_at="2026-11-04T00:00:00Z",
        )
        self.assertEqual(manifest["contract"], "nexus.cua.release.v1")
        self.assertEqual(manifest["release_state"], "published")
        self.assertEqual(manifest["protocol_version"], "nexus.cua.v1")
        self.assertEqual(manifest["files"]["runtime"], {"path": "nexuscua.exe", "sha256": "b" * 64})
        self.assertFalse(manifest["signing"]["production_signed"])
        prerelease = package_release.build_manifest(**{**_manifest_args(), "version": "0.3.0-rc.1"})
        self.assertEqual(prerelease["release_state"], "prerelease")
        with self.assertRaises(ValueError):
            package_release.build_manifest(**{**_manifest_args(), "signing_kind": "self-signed"})

    def test_archives_have_one_root_and_executable_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            stage = root / "nexuscua-0.2.0-macos-arm64"
            (stage / "licenses/a-1").mkdir(parents=True)
            (stage / "nexuscua").write_bytes(b"binary")
            (stage / "manifest.json").write_text("{}")
            (stage / "licenses/a-1/LICENSE").write_text("text")
            tar_path = root / "out.tar.gz"
            package_release.write_archive(stage, tar_path)
            with tarfile.open(tar_path) as archive:
                members = {member.name: member for member in archive.getmembers()}
            self.assertTrue(all(name.startswith(stage.name) for name in members))
            self.assertEqual(members[f"{stage.name}/nexuscua"].mode, 0o755)
            self.assertEqual(members[f"{stage.name}/manifest.json"].mode, 0o644)

            (stage / "nexuscua").rename(stage / "nexuscua.exe")
            zip_path = root / "out.zip"
            package_release.write_archive(stage, zip_path)
            with zipfile.ZipFile(zip_path) as archive:
                names = archive.namelist()
                self.assertIn(f"{stage.name}/nexuscua.exe", names)
                self.assertIn(f"{stage.name}/licenses/a-1/LICENSE", names)
                self.assertTrue(all("\\" not in name for name in names))

            checksum = package_release.write_checksum(zip_path).read_text()
            self.assertEqual(checksum, f"{package_release.sha256(zip_path)}  out.zip\n")

    def test_release_notes_come_from_the_changelog_section(self) -> None:
        changelog = "# Changelog\n\n## [Unreleased]\n\n- next\n\n## [0.2.0] - 2026-11-04\n\n- shipped\n\n## [0.1.0]\n\n- old\n"
        self.assertEqual(package_release.release_notes(changelog, "0.2.0"), "- shipped\n")
        with self.assertRaises(ValueError):
            package_release.release_notes(changelog, "0.9.9")

    def test_sbom_describes_root_and_dependencies(self) -> None:
        sbom = package_release.build_sbom(
            "0.2.0", "a" * 40,
            [{"name": "serde", "version": "1.0.0", "license": "MIT/Apache-2.0", "source": "registry+x"}],
            {("serde", "1.0.0"): "e" * 64}, "2026-11-04T00:00:00Z",
        )
        json.dumps(sbom)
        self.assertEqual(sbom["spdxVersion"], "SPDX-2.3")
        self.assertEqual(sbom["packages"][1]["licenseDeclared"], "MIT OR Apache-2.0")
        self.assertEqual(sbom["packages"][1]["checksums"][0]["checksumValue"], "e" * 64)
        self.assertEqual(len(sbom["relationships"]), 2)


def _manifest_args() -> dict:
    return dict(
        version="0.2.0", revision="a" * 40, platform="macos", architecture="arm64",
        target="aarch64-apple-darwin", toolchain="rustc 1.88.0", signing_kind="ad-hoc",
        runtime_name="nexuscua", runtime_sha256="b" * 64, sbom_sha256="c" * 64,
        notices_sha256="d" * 64, created_at="2026-11-04T00:00:00Z",
    )


if __name__ == "__main__":
    unittest.main()
