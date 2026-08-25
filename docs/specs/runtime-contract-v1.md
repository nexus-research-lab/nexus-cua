# Nexus Computer Use Runtime Contract v1

Status: normative for the `0.1.x` development line.

This document defines the product scope, native execution model, overload
behavior, and performance targets for the first releasable runtime. Protocol
wire shapes remain normative in `protocol-v1.md`.

## Product scope

Nexus Computer Use Runtime is a model-neutral native desktop execution engine
for Nexus and other bounded hosts. It provides:

- running application and top-level window discovery;
- exact-window screenshots suitable for an external vision model;
- bounded accessibility snapshots with element roles, names, values, states,
  actions, hierarchy, and geometry;
- semantic invoke/value/focus/select/toggle/expand operations when the platform
  exposes the matching public pattern;
- foreground focus, pointer, keyboard, text, wheel, and drag operations;
- fresh-observation guards and deterministic post-action verification;
- private authenticated local IPC, an embeddable Rust API, and diagnostics CLI.

It does not contain model inference, OCR, a planning loop, browser DOM/CDP,
product approval UI, complete-desktop capture, or unrestricted background
input. Nexus supplies its own vision model and exposes Computer Use to agents
through a round-scoped CLI/Skill. The `v1` authority unit is one exact top-level
window; system-shell surfaces require a future explicit surface contract rather
than a silent widening of window authority.

## Capability truth

Capabilities describe the active driver, not an aspirational union. A driver
must omit an action when it cannot implement its semantics using the selected
public OS route. It must never silently turn a semantic background action into
a foreground click or activate a window without foreground authority.

Driver routes are explicit:

| Platform | Primary capture | Semantic route | Foreground route |
| --- | --- | --- | --- |
| macOS | ScreenCaptureKit | AXUIElement actions/attributes | CGEvent |
| Windows | Windows.Graphics.Capture | UI Automation patterns | SendInput |

Compatibility capture drivers, if shipped, have distinct capability and
diagnostic identities. Private APIs and executable third-party drivers are not
permitted.

## Native actor model

Each process owns one bounded platform command bus and three long-lived roles:

- Capture actor: owns one-shot capture state on macOS and target-keyed frame
  pools, capture sessions, and GPU resources on Windows.
- Semantic actor: owns `AXUIElement` state on macOS or UI Automation COM MTA
  state on Windows. Native element objects never leave this actor.
- Input actor: serializes side effects and is the final foreground-authority
  enforcement point.

The capture and semantic branches may run concurrently. Input actions for one
session remain ordered. Queue admission is bounded; overload returns a stable,
retryable busy error before executing a side effect.

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

Windows observations acquire a short capture lease and reuse a native pipeline.
The pool retains only the newest unconsumed frame, has an LRU target cap, and
drops idle pipelines through deadline-driven cleanup. macOS `0.1.x` uses the
public `SCScreenshotManager` one-shot route until a stream pool satisfies the
same geometry, memory, shutdown, and permission behavior.

Raw GPU/IOSurface/D3D buffers stay inside the capture actor. RGBA normalization,
PNG encoding, and SHA-256 hashing run on bounded compute workers. Artifacts use
runtime-chosen private paths and expire with the session. Each process owns a
random artifact generation below the host root and removes that generation on
graceful teardown; crash cleanup remains an explicit host-supervisor step.

## Deadlines, cancellation, and idempotency

Every request has a bounded caller wait deadline. Admission creates the
idempotency record and detached execution: after that point the runtime records
the result rather than claiming cancellation. A caller that times out retries
the same `request_id`, optionally with a longer wait, to reconcile. Bounded
native queues return `busy` before their actor executes the command.

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
