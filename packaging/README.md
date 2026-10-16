# Distribution channel sources

This directory contains inert source metadata for npm, Homebrew, and WinGet.
Nothing here downloads a runtime, registers a host, starts a service, or
publishes a package.

`scripts/package_channels.py` stages channel artifacts only from an already
assembled canonical payload whose accepted candidate manifest binds the exact
runtime and native/remote/Codex evidence files by name, byte length, and digest.
It validates the evidence contracts, source revision, platform, native class,
3/3 Codex business attempts, post-admission reconciliation, cleanup, and empty
policy violations; an empty or merely present summary is rejected. The npm meta
package contains only a small launcher and selects a same-version optional
platform package; platform packages carry the canonical payload and have no
install scripts. Before emitting Homebrew or WinGet metadata, the generator
streams the exact-digest tar.gz or ZIP without extracting it, bounds compressed
and decompressed bytes and member count, rejects unsafe paths, links, special
files, duplicates, and conflicting hierarchies, requires the exact external
manifest inside the one canonical `nexus-cua-v<version>-<platform>/` archive
root, then applies the same runtime/profile/evidence validation to the archived
payload.

The channel names were checked for availability on 2026-09-02 before M4
implementation, but availability is not ownership or publish authority. Actual
publication remains outside the current no-publish scope.
