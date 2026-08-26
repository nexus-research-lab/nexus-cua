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
4. Deterministic native fixtures exercise the production transport and real
   platform drivers. Maintained release runners remain the authority for a
   supported platform claim.

## Developer validation snapshot: 2026-08-26

| Host | Environment | Evidence |
| --- | --- | --- |
| macOS | macOS 26.6.2, Apple M4 Max ARM64 | portable/native compilation and contract tests pass; `doctor` selects macOS, but the temporary linker-signed executable is not an accepted stable permission identity |
| Windows | Windows 11 `10.0.26100.3476` ARM64 in Parallels Desktop; Rust 1.88.0; .NET SDK 8.0.424; Windows SDK 26100 | native WPF fixture build plus real WGC, UIA, SendInput, and controlled hung-provider validation passed; the ARM64 VM remains engineering-only and is not the required Windows x64 release runner |

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

### Official client engineering validation

The Go and Python clients consume every committed request, success result, and
stable error fixture. Local macOS tests also completed real Unix-socket
`get_capabilities`, permission, and trusted-discovery requests against the
sidecar. Python 3.10, 3.11, 3.12, and 3.13 passed the source test suite; the
declared wheel and source distribution built without runtime dependencies.

The Windows 11 ARM64 engineering VM then ran Python 3.13.15 compatibility tests,
including import of the real overlapped named-pipe implementation. A live
Python request completed against the VM sidecar over its protected
`\\.\pipe\...` endpoint. A Go 1.25.5 Windows ARM64 test executable was
cross-compiled from the same working tree and executed inside the VM; all eight
tests passed, including the real named-pipe capability, permission, and
trusted-discovery path. The Go executable was used because repeated winget
downloads of the Go MSI failed at the VM network boundary; no Go test result
was inferred from that failed installation.

These runs establish client transport engineering on macOS and Windows ARM64.
They do not replace the M2 maintained Windows x64 fixture/evidence gate or the
M3 full native action acceptance that depends on it.

### Deterministic Windows fixture

The self-contained WPF fixture and standalone harness completed the production
named-pipe path as the interactive console user. The strict run proved:

- short-lived trusted-host discovery and process/window generation binding;
- exact-window WGC capture and full/interactive UIA snapshots with 77 normalized
  elements and secure-field redaction;
- semantic invoke, set-value, toggle, select, and expand;
- real `SendInput` focus, click, move, text, keys, scroll, and drag;
- stale-observation rejection, geometry change, destroyed-window rejection,
  and generation-2 replacement;
- same-request timeout reconciliation, deadline-driven session expiry, close
  cleanup, and transient artifact deletion;
- solid-magenta occlusion without desktop-pixel leakage; and
- minimized discovery plus fail-closed screenshot behavior without implicit
  restore.

One configured VM session exposed a real multiple-display transition and a
different screenshot scale, so multiple-display and mixed-DPI mapping passed in
that engineering run. The final M2 regression ran with only the primary VM
display exposed, so its machine-readable summary correctly left all three
topology scenarios incomplete instead of borrowing the earlier result. A
negative-coordinate target and elevated/protected target remain outstanding.
Windows truthfully
reports its three global permission categories as `not_applicable`; UIPI and
secure-desktop behavior belongs to the protected-target probe. A forced sidecar
termination followed by restart also passed: the old session returned
`session_unavailable`, and the lease-aware artifact store removed the orphaned
screenshot generation.

The controlled WPF provider faults also passed on this ARM64 VM. A UI-thread
hang before action preflight returned
`target_unresponsive/not_dispatched`, and a concurrent observation returned
`target_unresponsive/not_applicable`. A custom synchronous UIA invoke provider
then proved the later boundary: the short caller deadline returned
`deadline_exceeded/indeterminate`, and a longer wait using the same request ID
reconciled to `target_unresponsive/indeterminate`. These are real engineering
results, not Windows x64 release evidence.

The final source regression also passed 46 workspace tests, four standalone
harness tests, and the WPF ARM64 build with zero warnings. Its full fixture path
reported 81 normalized elements and completed capture, semantic mutation,
foreground mutation, lifecycle, occlusion, minimization, and generation
replacement after the WGC cache gained mutation-generation invalidation. A
diagnostic evidence aggregation then classified the run `incomplete` and named
only the absent topology, restart-in-that-run, protected target, maintained
runner, benchmark, and soak reports. The raw final VM records remain under
`C:\nexus-cua-vm-state\m2-*-20260826`; they are not repository-pinned release
evidence.

### macOS fixture boundary

The AppKit fixture builds and exposes the same machine-readable control
contract. Real ScreenCaptureKit, AX semantic mutations, pointer movement,
click, scroll, drag, artifact cleanup, and graceful sidecar restart paths work.
The 4K fixture profile produced a real 3840 x 2160 exact-window capture on the
current multi-display host, including negative logical coordinates.

The local foreground keyboard defect was narrowed to two implementation and
fixture issues. AppKit activation is asynchronous, and global HID keyboard
routing could lose or misdirect content after a concurrent focus change. The
driver now waits for the selected application to become active and posts key
and Unicode events to that already-authorized process; pointer events retain
their real global HID semantics. The fixture now installs the standard
Edit/Select All responder-chain command needed to validate `Command+A`.

An ad-hoc local run then replaced `Fixture Text` through the foreground
`Command+A` plus Unicode path and continued through scroll, drag, geometry, and
generation-2 window replacement. That is useful engineering evidence, but not
an accepted run: repeated rapid local launches still exposed transient
ScreenCaptureKit visibility/capture and AX-identity failures under the temporary
linker identity. The harness now applies bounded retries only to explicitly
retryable read operations and waits for the old window descriptor to disappear;
it never retries a mutation.

A later ad-hoc rebuild also received a sanitized/cyclic AX application tree
instead of the fixture window controls despite positive process-level
preflights. That reinforces the same conclusion: this developer executable is
not suitable permission evidence. The hardware workflow now requires a real
non-ad-hoc signing identity with the fixed test identifier and refuses to run
the release matrix without it.

### Diagnostic performance evidence

The repeatable harness now records warm sample counts and p50/p95 values for
IPC, application/window discovery, capture, semantics, combined observation,
semantic action, and foreground action. It also records RSS, CPU time, Windows
handles, macOS open files, actual screenshot dimensions, and monotonic growth.

Current debug runs are diagnostic only. On the ARM64 Windows VM, two-sample
capture and combined-observation p95 values were about 722 ms and 705 ms. A
later Windows fixture probe produced an exact 3840 x 2160 frame and a
133,689,344-byte peak resident-set delta, narrowly below the 128-MiB budget. A
later macOS ScreenCaptureKit diagnostic produced the required 3840 x 2160 frame
with an approximately 90 MiB peak physical footprint and passed the incremental
memory gate. During a 300-operation, five-minute diagnostic, however, physical
footprint rose from approximately 54.0 MiB to 61.4 MiB even though native leak
object counts stayed constant. That monotonic trend still requires resolution
or accepted long-soak evidence. None of these results is a release baseline.

## Remaining native release evidence

The current snapshot does not establish release support. Phase 1 still needs:

- an accepted complete macOS fixture path under a stable permission-owning app
  identity;
- maintained-runner mixed-DPI and multiple-display coordinate validation;
- macOS permission denial/revocation, Windows elevated/protected truthfulness,
  and the corresponding machine-readable evidence summaries;
- passing absolute latency/memory baselines, five-minute idle CPU, and the
  one-hour/eight-hour resource soaks on named maintained runners; and
- signed clean-machine package and upgrade testing on the declared macOS and
  Windows matrix.

See the [delivery roadmap](roadmap.md) for ordering and
[runtime contract](specs/runtime-contract-v1.md) for the normative gates.
