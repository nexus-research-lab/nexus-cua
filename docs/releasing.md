# Releasing

Releases are published by [`publish.yml`](../.github/workflows/publish.yml) when a
`v<semver>` tag is pushed. Release binaries are built on GitHub-hosted runners.

## Steps

1. Set the workspace version in `Cargo.toml` and the version-bearing crate,
   SDK, package, profile and skill files. Run `make check`.
2. Move the `Unreleased` entries in `CHANGELOG.md` into a
   `## [<version>] - <date>` section. The workflow publishes that section as the
   release notes and fails without it.
3. Commit to `main`, then push the tag:

   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```

The workflow rejects a tag that does not match the workspace version or is not
on `main`. It runs the macOS and Windows quality gates, builds the runtimes,
signs them, packages them with `scripts/package_release.py`, uploads everything
to a draft release and then publishes it. A version with a prerelease suffix is
published as a GitHub prerelease. It also creates `sdk/go/v<version>` at the same
commit for the Go module.

To rerun a failed publish for an existing tag, run the workflow manually with
that tag. An already published release is never overwritten, because hosts pin
archive digests.

## Supplemental Intel archive

Dispatch `publish.yml` from `main` with the existing release `tag` and
`intel_only=true` to build the tag's exact source on `macos-15-intel`. This mode
produces the `release-macos-x86_64` workflow artifact, including its checksum,
manifest, SBOM and notices. Review the artifact before adding its new filenames
to the existing release and merging its checksums into `SHA256SUMS`. Existing
archive bytes and their digests remain immutable. Record the supplemental
package's validation scope in the release notes.

## Signing

| Platform | Configured credentials | Without credentials |
| --- | --- | --- |
| macOS arm64 / x86_64 | Developer ID signing and notarization (`MACOS_DEVELOPER_ID_*`, `APPLE_NOTARY_*`) | ad-hoc signature |
| Windows x64 | Azure Artifact Signing Authenticode (`AZURE_*` secrets and variables) | unsigned |

Each manifest records the signing kind it received. Set the repository variable
`NEXUS_CUA_REQUIRE_SIGNING=true` to make missing credentials fail the release
instead of falling back.

## Assets

For each platform the release contains `nexuscua-<version>-<platform>-<arch>`
as `.tar.gz` (macOS) or `.zip` (Windows), its `.sha256`, and standalone copies of
`manifest.json`, `sbom.spdx.json` and `THIRD_PARTY_NOTICES.md`. A combined
`SHA256SUMS` covers every asset. Archives contain the runtime, the
`nexus.cua.release.v1` manifest, an SPDX SBOM of the shipped Cargo dependency
closure, and a `licenses/` directory copied from local crate sources. Packaging
downloads nothing.

## Host pinning

Hosts that install the runtime should pin both the version and the archive
SHA-256 from `SHA256SUMS`. A release's own `.sha256` proves only transport
integrity. Nexus records these values in
`internal/service/computeruse/pin.go`.
