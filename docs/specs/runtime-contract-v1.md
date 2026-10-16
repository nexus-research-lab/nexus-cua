# Nexus Computer Use Runtime Contract v1

Status: normative for the `0.1.x` development line.

This document defines the product scope, native execution model, overload
behavior, and performance targets for the first releasable runtime. Protocol
wire shapes remain normative in `protocol-v1.md`.

## Product scope

Nexus Computer Use Runtime is a model-neutral native desktop execution engine
for Nexus and other bounded hosts. It provides:

- running application and top-level window discovery;
- bounded installed-application discovery and provenance-bound cold launch on
  drivers that negotiate the lifecycle capability;
- exact-window screenshots suitable for an external vision model;
- separately authorized read-only capture of one exact physical display for
  desktop overview and window selection;
- separately authorized bounded plain-text clipboard reads and guarded
  compare-and-replace writes;
- bounded accessibility snapshots with element roles, names, values, states,
  actions, hierarchy, and geometry;
- semantic invoke/text-value/numeric-range/focus/select/toggle/expand
  operations when the platform exposes the matching public pattern;
- system-routed top-level window move/resize and minimize/restore operations;
- foreground focus, click and bounded pointer-hold, pointer, keyboard, text,
  wheel, and drag operations;
- best-effort exact-target background pixel clicks with explicit delivery and
  no implicit foreground escalation;
- fresh-observation guards and deterministic post-action verification;
- private authenticated local IPC, an embeddable Rust API, and diagnostics CLI.

It does not contain model inference, OCR, a planning loop, browser DOM/CDP,
product approval UI, display-scoped mutation authority, or unrestricted
background input. A connected Agent Runtime owns model inference and its
planning loop.
Nexus exposes Computer Use to that runtime through a round-scoped CLI and
built-in Skill. The `v1` authority unit is one exact top-level window;
system-shell surfaces require a future explicit surface contract rather than a
silent widening of window authority.

## Capability truth

Capabilities describe the active driver, not an aspirational union. A driver
must omit an action when it cannot implement its semantics using the selected
public OS route. It must never silently turn a semantic background action into
a foreground click or activate a window without foreground authority.

Driver routes are explicit:

| Platform | Primary capture | Semantic route | System route | Background click route | Foreground route |
| --- | --- | --- | --- | --- | --- |
| macOS | ScreenCaptureKit | AXUIElement actions/attributes | AX window attributes | process-targeted CGEvent | system CGEvent |
| Windows | Windows.Graphics.Capture | UI Automation patterns | Win32 window APIs | exact-HWND client messages | SendInput |

The background route is currently negotiated only for `click_point`. It is a
delivery request, not a claim that every application accepts synthetic
background events. The driver never activates the target or substitutes the
foreground route. Callers verify through a fresh observation and request an
explicit foreground action only under separate host authority. Background
keyboard, scroll, drag, and pointer-move delivery remain unsupported until a
public route can preserve the same targeting and failure semantics.

Successful actions report both route and effect evidence. Bounds,
minimize/restore, exact process exit, and any foreground transition that the
driver actually reads back may report `confirmed/native_readback`. Event and
provider acceptance without an independent read-back reports
`unverified/dispatch_acceptance`, even when the native API returned success.
Drivers cannot manufacture a confirmed result from dispatch success, and the
runtime fails closed on an incoherent pair. Any mutation that may only have
partially executed remains an indeterminate error under the original request
identity rather than a partial-success result.

Foreground restoration is a separate host grant, not an automatic driver
preference. For temporary foreground input, the platform actor binds the exact
previous owner generation and restores it only while the guarded target still
owns foreground. It never steals focus back after a user or system switch.
Restoration failure does not rewrite successful action evidence: it is reported
truthfully in the closed `foreground_restoration` field. Explicit focus actions
intentionally retain focus and are never auto-restored.

Application termination uses the system route and remains observation-bound.
The macOS capture actor owns `NSRunningApplication` and requests `terminate`;
the Windows input actor revalidates the process creation time, posts `WM_CLOSE`
to the exact generation's current top-level windows, and waits on the process
handle. Both confirmation waits are bounded. Neither driver escalates to forced
termination, and a timeout is indeterminate rather than success.

Application launch is a trusted-host bootstrap mutation, not session input.
Installed discovery is bounded and produces short-lived opaque references tied
to a native registration identity and provenance. The runtime performs fresh
native revalidation, consumes the reference once, and never admits a
caller-authored executable path, argument vector, working directory, URL, or
PID. A successful launch returns a fresh running-generation discovery reference
that still requires normal session admission. Native launch and usable-window
confirmation stay on the platform actor and use bounded waits. Once the native
launch call is admitted, an unconfirmed result is indeterminate and must not be
replayed under a new request identity. macOS implements this route through
reviewed application bundles and `NSWorkspace`. Windows uses a dedicated COM
actor to scan bounded, non-reparse-point `.lnk` registrations in the user and
machine Start Menu roots, binds both shortcut and executable file generations,
resolves only local executable targets without Shell tracking/repair, and calls
`ShellExecuteExW` only after fresh revalidation. Unsupported or proxy targets
are omitted rather than widened into arbitrary command execution.

The macOS keyboard route targets the process bound to the selected application
generation only after the driver has confirmed that application is active. PID
routing closes a focus-change data-leak race; it does not grant background
input. Failure to establish foreground state fails before the requested key or
text event is posted.

Compatibility capture drivers, if shipped, have distinct capability and
diagnostic identities. Private APIs and executable third-party drivers are not
permitted.

## Native actor model

Each process owns bounded platform command buses and four long-lived roles:

- Capture actor: owns one-shot capture state on macOS and target-keyed frame
  pools, capture sessions, and GPU resources on Windows.
- Semantic actor: owns `AXUIElement` state on macOS or UI Automation COM MTA
  state on Windows. Native element objects never leave this actor.
- Input actor: serializes side effects and is the final delivery/foreground-
  authority enforcement point. It owns background clicks, foreground input,
  and, on Windows, no-activate window-system mutations; background and system
  calls do not inherit foreground keyboard or pointer authority.
- Clipboard actor: owns `NSPasteboard` on macOS or the Win32 clipboard owner
  window on Windows. It serializes bounded plain-text reads and guarded writes;
  native pasteboard handles and text buffers never leave this actor unwrapped.

The capture and semantic branches may run concurrently. One runtime-wide lane
admits at most one foreground mutation for the OS desktop. It uses fail-fast
admission rather than a pending queue; concurrent foreground work returns a
stable, retryable `busy/not_dispatched` before observation consumption or native
side effects. Native actor queues remain bounded for all routes.

## Runtime policy composition

Every public adapter reaches one immutable runtime policy stack before session
lookup or native dispatch. Optional managed and user JSON layers are each
deny-by-default; both must admit the operation, and the user layer cannot widen
the managed layer. Hard denies run before allows. Action rules can narrow text
bytes, key/click counts, delivery preference, pointer duration, and scroll
delta. Session creation cannot advertise an action kind absent from either
configured layer. Mutation denial is proven `not_dispatched` and never consumes
the guarded observation.

Configured files fail runtime construction when absent, unsafe, oversized,
malformed, or semantically invalid. They are loaded once with no hot-reload
path. Status surfaces expose only SHA-256 content digests. The detailed schema
and CLI flags are documented in [runtime dispatch policy](../runtime-policy.md).

## Target identity and observation coherence

A private target identity includes the process lifetime, native window
identity, and a driver generation. It is not just a PID, HWND, or window title.

Before session creation, the trusted policy host receives a discovery
descriptor and an opaque reference valid for at most 30 seconds. The runtime
binds that reference to its process epoch, the native process generation, and
the normalized executable or bundle identity. `open_session` performs a fresh
enumeration and fails with `stale_discovery` unless all three still match.
macOS provenance reports bundle identifier, executable path, and code-signing
identity when available; Windows reports normalized image path and available
publisher/signature state. Missing signature metadata is descriptive and does
not itself deny authority; the embedding host owns that policy decision.

Observation uses a two-phase coherence check:

1. Read target generation and geometry.
2. Capture requested pixels and semantic state in parallel.
3. Read generation and geometry again.
4. If they match, publish the coherent observation. If they differ, discard
   both results and retry exactly once; a second mismatch returns
   `stale_observation`.

Animation or a blinking cursor alone does not invalidate authority. Window
replacement, process restart, geometry/DPI change, semantic target-path change,
explicit OS invalidation, TTL expiry, or any successful mutation does.

Display overview is a separate read-only scope. `list_displays` projects fresh
opaque refs for exact physical displays, and `observe_display` revalidates the
selected native display identity and logical bounds before capture. A display
observation contains no element refs and is never inserted into the guarded
window-observation set, so no action can cite it. A caller must list and observe
the intended exact window before mutation. This permits lightweight access to
the user's real desktop without pretending it is a VM or widening window
authority.

Clipboard is a separate non-window scope. `read_clipboard` requires its own
session grant and returns at most the configured UTF-8 byte bound plus an opaque
`ClipboardRef`; the runtime retains only one native change marker per session,
not the text. `write_clipboard` additionally requires bounded-mode write
authority and the latest unconsumed ref. It consumes the ref before dispatch,
rejects an external native change as `stale_clipboard/not_dispatched`, replaces
plain text only, and returns a fresh guard after native read-back. Clipboard
text is redacted from debug output and forbidden from logs and artifacts.

## Coordinates

There are exactly two public coordinate spaces:

- `screen_points`: logical top-left screen coordinates used by window and
  accessibility bounds. Values may be fractional and may be negative on a
  multi-display desktop.
- `screenshot_pixels`: unsigned integer coordinates in the exact image
  artifact returned by the guarded observation.

Every screenshot observation publishes its pixel size and a checked affine
mapping from screenshot pixels to screen points. Pixel actions accept only
`screenshot_pixels` from that exact observation. The runtime validates image
bounds before the driver converts them to the OS input coordinate system.

## Semantic snapshots

The default view is `interactive`: actionable, focusable, editable, selected,
or scrollable elements plus the minimum ancestor chain needed to preserve
hierarchy. `full` is an explicit diagnostic option and remains bounded.

Elements report `enabled` and `focused` as required booleans. `selected`,
`checked`, and `expanded` are nullable because a platform pattern may be absent
or a toggle may be indeterminate. Unsupported and mixed state is never coerced
to `false`. The `element_state` predicate selects by normalized role/name and
compares one closed state against a fresh semantic snapshot, so verification
does not reuse an observation-scoped element reference.

Traversal must:

- batch properties/patterns through `AXUIElementCopyMultipleAttributeValues`
  or a UI Automation cache request;
- be iterative rather than recursively consuming the process stack;
- enforce node, depth, string-byte, provider-call, and wall-time limits;
- report `complete=false` and a stable truncation reason when bounded;
- produce observation-scoped element references, never public native handles.

Traversal budgets are enforced whenever the native provider returns control to
its owning actor. Some operating-system accessibility calls cannot be safely
interrupted while they are inside a hung third-party provider. The sidecar
process boundary and the host's bounded reconciliation/termination policy are
the ultimate hard bound for that case.

The current implementation pulls fresh semantic state and performs an exact
element signature preflight before each action. Future notification caches may
reduce provider work, but notifications can only be freshness hints and can
never become authorization by themselves.

## Capture lifecycle

Windows window observations acquire a short capture lease and reuse a native pipeline.
The pool retains only the newest unconsumed frame, has an LRU target cap, and
drops idle pipelines through deadline-driven cleanup. macOS `0.1.x` uses the
public `SCScreenshotManager` one-shot route until a stream pool satisfies the
same geometry, memory, shutdown, and permission behavior.

Physical-display capture uses the same bounded artifact and encoding path but
not the window action-guard cache. macOS binds public ScreenCaptureKit display
objects and Windows creates a WGC item for a freshly enumerated monitor. Both
revalidate display identity and bounds and return one transient image; neither
route creates input authority.

Raw GPU/IOSurface/D3D buffers stay inside the capture actor. RGBA normalization,
PNG encoding, and SHA-256 hashing run on bounded compute workers. Artifacts use
runtime-chosen private paths and expire with the session. Each process owns a
random artifact generation below the host root and removes that generation on
graceful teardown; crash cleanup remains an explicit host-supervisor step.

An adapter that converts an artifact to an inline image should use the checked
single-use take operation. The runtime revalidates the registered artifact's
identity, owned canonical path, declared length, and SHA-256 digest, returns
bounded bytes, then removes the exact file and registration. Adapter responses
must not expose the runtime-local path. Any retained bytes remain subject to an
explicit aggregate memory bound and are cleared when their guarded observation
is superseded.

## Deadlines, cancellation, and idempotency

Every request has a bounded caller wait deadline. Admission creates the
idempotency record and detached execution: after that point the runtime records
the result rather than claiming cancellation. A caller that times out retries
the same `request_id`, optionally with a longer wait, to reconcile. Bounded
native queues return `busy` before their actor executes the command.

Every public failure names its mutation disposition. Runtime and driver
preflight failures are `not_dispatched`; a failure returned after admission to
the native mutation call defaults to `indeterminate` unless the owning actor
can prove it did not dispatch. `target_unresponsive` distinguishes a bounded
native-provider timeout from a disappeared target. An observation is read-only
and therefore never indeterminate. Supervisors may terminate an unresponsive
sidecar after an indeterminate result, but must never replay that mutation under
a new request ID.

`request_id` is an in-process idempotency key. Concurrent identical retries
join the first execution; completed retries replay the exact response. Reusing
the identity for a different canonical command fails closed. Completed
responses are retained for a configurable horizon of 10 minutes by default.
An unexpired completion is never evicted to admit new work; ledger capacity
returns `busy`. After the horizon, clients treat a missing result as
indeterminate and never retry a mutation under a new ID. After process restart
the ledger, runtime epoch, discovery references, and sessions are all invalid,
so a retried mutation cannot regain authority.

Session and discovery expiry are driven by one nearest-deadline scheduler, not
by incoming requests or active polling. Opening, closing, discovering, and
expiring work reschedules that deadline. Expired sessions reject immediately
and their artifacts are removed while the service is otherwise idle. Embedded
hosts call `Runtime::shutdown`; the sidecar stops admission, waits for admitted
runtime commands, shuts down the scheduler, and removes live session artifacts
before returning.

## Performance budgets

Budgets are measured on a release build after one warm-up, on a current
supported OS and ordinary desktop hardware. They are engineering gates, not API
promises for a hung third-party application.

| Operation | Warm p95 target | Hard behavior |
| --- | ---: | --- |
| IPC dispatch overhead | <= 3 ms | frame rejected above configured bound |
| Application/window discovery | <= 50 ms | fresh bounded OS enumeration |
| 1920x1080 window capture | <= 100 ms | newest-frame policy, one in flight/target |
| Interactive semantic snapshot (<=1000 nodes) | <= 120 ms | partial at 250 ms provider budget |
| Combined pixel + semantic observation | <= 180 ms | parallel branches, one coherence retry |
| Semantic action preflight + dispatch | <= 50 ms | serialized input actor |
| Foreground action preflight + dispatch | <= 60 ms | serialized input actor |

Additional budgets:

- idle CPU below 0.5% over five minutes with no active request;
- resource-soak RSS leak detection uses a 1 MiB net-growth noise floor for VM
  page and allocator settling; file and handle counts use no noise allowance;
- macOS memory gates use `proc_pid_rusage` physical footprint, excluding
  reclaimable purgeable capture backing; Windows uses process working set;
- no unbounded queue, tree, frame pool, artifact set, log field, or retry loop;
- at most 64 local connections and 64 distinct in-flight requests by default;
- at most 64 live capability sessions by default, with an embedding host able
  to select a smaller non-zero bound;
- discovery references live for at most 30 seconds, with at most 256
  applications per snapshot and 2,048 unexpired references by default;
- completed request results remain replayable for 10 minutes by default, and
  admission fails rather than evicting an unexpired ledger entry;
- at most four warm Windows target capture pipelines by default;
- at most two retained frames per pipeline;
- a 4K capture pipeline target below 128 MiB of resident GPU/CPU buffers;
- eight-hour release soak without monotonic handle, COM, IOSurface, GPU, file,
  or resident-memory growth.

Shared CI enforces formatting, strict lint, protocol/runtime contracts, and
native compilation on macOS and Windows. Before a release tag, maintained
hardware workers must also record benchmark distributions, compare them with a
pinned baseline, and pass the absolute p95 and soak gates above.

## Development completeness

The `0.1.x` line is not release-complete until hardware capture/action fixtures,
the eight-hour resource soak, benchmark baselines, and signed package smoke
tests pass on both operating systems. A warm macOS stream pool and AX
notification cache are performance optimizations, not protocol blockers; the
one-shot/fresh-read routes remain the correctness baseline.

## Nexus enablement boundary

Nexus decides whether Computer Use is enabled. Disabled means no model-facing
Computer Use capability and no host-issued session authority. Browser
enablement is independent:

- Computer Use on, Browser off: native desktop control works, including visible
  browser chrome as an ordinary app, but DOM/CDP/browser history/network tools
  do not.
- Computer Use off, Browser on: existing browser control remains unchanged.
- both on: the Skill routes web semantics to Browser and native application
  semantics to Computer Use; enabling one never silently grants the other.
