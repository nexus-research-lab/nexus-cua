# Platform Validation

## Codex MCP dogfood

On 2026-08-27, Codex CLI `0.149.0` loaded the project-scoped local
`nexus_cua` stdio MCP server and controlled only the exact authorized
`dev.nexus.cua.native-fixture` process. The successful visual path used
`gpt-5.6-luna` with accessibility disabled and executed:

```text
nexus_cua_get_status
  -> nexus_cua_list_windows
  -> nexus_cua_observe_window
  -> nexus_cua_click_normalized
  -> nexus_cua_observe_window
```

`nexus_cua` and the `nexus_cua_*` leaves in this section are immutable names
captured by historical development evidence, not supported compatibility names.
The managed alias is `nexuscua`, with unprefixed leaves and qualified names such
as `mcp__nexuscua__observe_window`. Historical event names are not rewritten
after capture; the old tools were deleted outright in the M3 source migration
and are not registered by the current adapter.

The first image established `counter=0`; one click-only bounded action was
delivered through the foreground route; the fresh second image established
`counter=1`. Codex used MCP tools directly and ran with a read-only shell
sandbox; no shell, filesystem, Browser, private sidecar token, or direct runtime
call substituted for the MCP action.

The provider-neutral zoom path was then dogfooded independently with the same
fixture and shell restrictions:

```text
nexus_cua_observe_window
  -> nexus_cua_zoom
  -> nexus_cua_click_view_normalized
  -> nexus_cua_observe_window
```

Codex cropped the latest protected observation around the button, clicked once
in opaque view-local top-left `0..1000` coordinates, and verified that the full
window counter increased by exactly one. This validates both bounded crop
delivery and server-side projection back into the original protected image.

The same project MCP integration also passed the system-routed window lifecycle
path:

```text
list_windows -> observe(no image) -> set_window_bounds -> list_windows
             -> observe(no image) -> set_window_minimized(true) -> list_windows
             -> observe(no image) -> set_window_minimized(false) -> list_windows
```

Codex verified the requested logical x/y/width/height within two points, then
verified minimized `false -> true -> false`. The actions used their independent
authority kinds and `system` delivery; foreground pointer/keyboard permission
was not used. Earlier attempts truthfully exposed AppKit layout and visible
screen clamping plus asynchronous AX minimized-state propagation. The fixture
now permits real horizontal resizing, and the macOS AX actor performs bounded
read-back confirmation for at most 200 milliseconds before reporting success.

The observation-bound application lifecycle path also passed through a fresh
Codex CLI process using only MCP tools and with foreground input disabled:

```text
get_status -> list_windows -> observe(no image, AX disabled)
           -> terminate_application -> list_apps -> list_windows
```

The dedicated mutation returned `delivery_mode=system`, `dispatched=true`, and
`observation_invalidated=true`; both final listings were empty. The model never
received or supplied a PID, path, command line, or force flag. An earlier run
truthfully returned `indeterminate` even though read-only reconciliation then
showed that the application had exited. That exposed a non-main-thread
`NSRunningApplication.isTerminated` refresh race. The capture actor now confirms
exit with libproc against the exact PID/start-time generation, and the clean
Codex rerun passed without retrying the mutation.

The stricter cold-start lifecycle path then passed in a new ephemeral Codex
process while the fixture was absent from the running-application list. The
fixture bundle was installed under the current user's standard Applications
directory, and Codex used only the advertised MCP tools:

```text
get_status(session.active=false)
  -> list_apps/list_windows (no configured application running)
  -> list_launchable_apps (one exact stable identity)
  -> launch_application(dispatched=true)
  -> list_apps/list_windows
  -> observe_window(no image, AX disabled)
  -> terminate_application(dispatched=true)
  -> list_apps/list_windows ([]/[])
```

The host authority admitted only `dev.nexus.cua.native-fixture`. The model never
received a launch path, arguments, PID, native registration handle, sidecar
credential, or shell fallback. Launch returned a fresh running-generation
discovery reference; the MCP adapter rebuilt its finite session around that
generation, and termination removed it cleanly. There were no unavailable
tools, indeterminate mutations, retries, or unproven final states. This is the
project's required form of Codex integration evidence: self-tests or direct
shell calls alone do not satisfy it.

After action effect/evidence fields were added, a fresh Codex CLI `0.149.0`
process using `gpt-5.6-sol` repeated the cold lifecycle through the release MCP
binary. Its JSON event stream recorded every operation as `mcp_tool_call` with
`server = "nexus_cua"`; no shell, filesystem, browser, network, or other MCP
tool event occurred. The exact lifecycle result was:

```text
status(session.active=false)
  -> cold list apps/windows (session_unavailable, launch recovery)
  -> one launchable dev.nexus.cua.native-fixture
  -> launch once (foreground, dispatched=true)
  -> one running app and one visible Generation 1 window
  -> observe(no screenshot, AX disabled)
  -> terminate once (system, dispatched=true,
       effect_status=confirmed, evidence_kind=native_readback,
       observation_invalidated=true)
  -> apps=[] and windows=[]
```

The non-interactive run used an isolated seven-tool allowlist and explicit MCP
pre-approval; nexus-cua's separate bounded authority still admitted only the
fixture identity, launch activation, click, and cooperative termination. Three
earlier setup probes performed no desktop mutation: they exposed qualified MCP
names, code-mode nested-tool routing, and Codex's outer write-approval policy.
They are not runtime failures and were not counted as acceptance evidence. The
temporary installed fixture was moved to Trash after the final empty-state
check.

A separate read-only whole-desktop acceptance then used a fresh ephemeral Codex
process and only three `nexus_cua` MCP tools:

```text
get_status(capture_modes includes display, allow_display_capture=true)
  -> list_displays(three displays, exactly one primary)
  -> observe_display(primary, one 3024x1964 image/png)
```

The session contained no application identities, action grants, foreground
input, or launch authority. The returned structured observation exposed no
filesystem path, and the Codex event stream contained no action tool. An
initial run truthfully failed because the project-local Codex `enabled_tools`
allowlist had not been extended with the new display tools; aligning the static
allowlist with the registered MCP surface fixed the integration gap. The final
Codex report was PASS.

Two earlier truthful failures found issues that were fixed before the pass. The
first exposed a vertically inverted macOS RGBA copy, now covered by a row-order
regression test. The second exposed HiDPI coordinate ambiguity when a model saw
a resized 1920 x 1504 image. The MCP adapter now advertises exact dimensions and
provides a bounded top-left `0..1000` normalized click tool whose conversion is
unit-tested. The adapter also permits a host-bounded 120-second visual-model
think interval while revalidating the exact native visual fingerprint at
dispatch.

An additional strict background-click dogfood was attempted with
`allow_foreground_input=false` and an explicit `delivery=background`. Codex
discovered the MCP server and confirmed the bounded authority, but the first
mutation-guard screenshot failed before any action with ScreenCaptureKit
`SCStreamErrorDomain -3811`. This is not recorded as a background-click pass.
The same host session returned zero ScreenCaptureKit displays, Apple
`screencapture -l` failed against the fixture, ChatGPT, and Safari windows, and
`replayd` logged window-screenshot creation failures while full-display
`screencapture` still worked. The runtime now preserves the bounded native
error detail, does not silently substitute occlusion-prone display cropping,
and leaves native background delivery acceptance pending a healthy capture
session.

This is development-machine dogfood evidence, not signed-package or release
evidence. Per the current development goal, eight-hour soak is deferred until a
later explicitly requested publishing gate; it is not part of current feature
acceptance. No artifact was published.

The Windows source driver now contains the matching bounded Start Menu
installed-discovery and cold-launch route, and the maintained-runner workflow
installs an exact temporary fixture shortcut before exercising the public
`lifecycle` harness command. This macOS host installed the official
`x86_64-pc-windows-msvc` Rust target and passed strict platform Clippy plus a
full-workspace Windows cross-check using the pinned rustup toolchain. That is
compile evidence only: native Windows x64 execution and Codex passes remain
unverified here.

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

The Go, Python, and TypeScript clients consume every committed request, success
result, and stable error fixture. The zero-runtime-dependency TypeScript client
also passes strict declaration compilation, sensitive-text redaction, and
same-request reconciliation tests. Local macOS tests completed real Unix-socket
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
They do not replace the maintained Windows x64 fixture/evidence gate or the
full native action acceptance that depends on it.

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
that engineering run. The final Windows VM regression ran with only the primary VM
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

## Current macOS smoke: 2026-10-13

On the current Apple Silicon macOS host, the release `nexuscua` binary was built with
`cargo build --release --locked --package nexus-cua --bin nexuscua`. `doctor --compact`
reported `platform=macos`, protocol `nexus.cua.v1`, and `screen_capture=granted`,
`accessibility=granted`, `input_control=granted`. A real sidecar was then started with
its private development root, and authenticated local IPC successfully returned
`get_capabilities` and `get_permission_status`.

The signed debug AppKit fixture was launched through LaunchServices and the standard
`native-validate` harness ran against that sidecar. Sixteen automated scenario groups
passed, including discovery, display overview, observation, semantic and foreground
mutation, stale observation, window generation, occlusion, minimized windows, negative
coordinates, reconciliation, secure redaction, and artifact cleanup. `mixed_dpi` was
`manual_required` because this host did not provide the maintained mixed-DPI topology;
the aggregate status was `partial` and `automated_topology_complete=false`. This is
current engineering evidence for the CUA runtime, not signed payload, Nexus end-to-end,
Codex acceptance, or release evidence.

## Remaining native release evidence

The current snapshot does not establish release support. Current acceptance still needs:

- an accepted complete macOS fixture path under a stable permission-owning app
  identity;
- maintained-runner mixed-DPI and multiple-display coordinate validation;
- macOS permission denial/revocation, Windows elevated/protected truthfulness,
  and the corresponding machine-readable evidence summaries;
- passing absolute latency/memory baselines, five-minute idle CPU, and the
  one-hour/eight-hour resource soaks on named maintained runners; and
- signed clean-machine package and upgrade testing on the declared macOS and
  Windows matrix.

See the [complete product PRD](PRD.zh-CN.md) for ordering and
[runtime contract](specs/runtime-contract-v1.md) for the normative gates.
