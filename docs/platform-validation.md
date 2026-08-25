# Platform Validation

This document records concrete validation evidence for Nexus Computer Use
Runtime. It is an evidence log, not a declared support matrix. A successful
developer-machine smoke test does not replace deterministic fixtures, release
benchmarks, soak tests, clean-machine package tests, or signed artifacts.

## Validation layers

1. `cargo fmt`, strict workspace Clippy, and workspace tests validate portable
   protocol, runtime, transport, and selected native code at build time.
2. `doctor` validates the selected driver contract and current operating-system
   permission state.
3. A native read-only smoke opens a bounded session over the production local
   transport, enumerates real applications and windows, observes an exact
   window, and verifies transient-artifact cleanup.
4. Maintained GUI fixtures and the release matrix remain the authority for a
   supported platform claim.

## Developer validation snapshot: 2026-08-25

| Host | Environment | Evidence |
| --- | --- | --- |
| macOS | macOS 26.6.2, Apple M4 Max ARM64 | `make check` passed, including all 40 workspace tests, schema drift, and Markdown links; `doctor` selected macOS and reported screen capture, accessibility, and input permissions granted |
| Windows | Windows 11 `10.0.26100.3476` ARM64 in Parallels Desktop; Rust 1.88.0; MSVC 14.44.35207; Windows SDK 26100 | formatting, strict Clippy for all workspace targets, all 39 workspace tests, `doctor`, trusted-host discovery/session smoke, and the native read-only observation smoke below passed |

The Windows run used the current working tree on the VM's local disk and Cargo's
offline locked mode after seeding the registry cache. That isolates compilation
and test results from shared-folder locking and network availability.

### Windows native read-only smoke

The service ran as the interactive console user over its protected Windows
named pipe. The smoke verified:

- a read-only, application-allowlisted session with a finite TTL;
- trusted-host discovery of Windows Terminal and Explorer with random
  30-second refs, normalized executable paths, verified Authenticode state,
  and publisher names (`Microsoft Corporation` and `Microsoft Windows`);
- successful read-only session creation from the freshly discovered Windows
  Terminal process generation, plus stable `stale_discovery` recovery after a
  ref expired;
- real Windows Terminal and Explorer discovery with opaque application refs;
- exact top-level window discovery with opaque window refs and logical screen
  bounds;
- normal recovery from one stale foreground-window ref by listing windows
  again;
- a successful Windows.Graphics.Capture observation of a stable Windows
  Terminal window: 2350 x 1225 PNG, 20,090 bytes;
- an interactive UI Automation tree with 14 normalized elements, complete tree
  status, hierarchy, bounds, and `invoke`/`select` actions;
- authenticated CLI-to-service request handling over the native named pipe; and
- deletion of the screenshot artifact immediately after the session closed.

The successful observation completed in about 645 ms in an unoptimized debug
build inside the VM. This is a diagnostic sample, not a benchmark baseline.

## Remaining native release evidence

The current snapshot does not establish release support. Phase 1 still needs:

- real semantic mutations and SendInput paths against deterministic fixtures;
- mixed-DPI and multiple-display coordinate validation;
- minimized, occluded, restarted, elevated, and hung-application behavior;
- permission denial and revocation flows;
- absolute latency and memory baselines plus the eight-hour resource soak; and
- signed clean-machine package and upgrade testing on the declared macOS and
  Windows matrix.

See the [delivery roadmap](roadmap.md) for ordering and
[runtime contract](specs/runtime-contract-v1.md) for the normative gates.
