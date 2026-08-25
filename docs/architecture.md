# Nexus Computer Use Runtime Architecture

Nexus Computer Use Runtime is a local, model-neutral desktop execution engine.
It owns observation, authorization, action delivery, and verification. It does
not own agent reasoning or user-facing consent.

The runtime is Nexus-first and host-neutral. Nexus is the primary product
consumer, while the same Rust API and sidecar protocol remain suitable for
independent hosts. No Nexus user, chat, round, preference, or receipt type
crosses the runtime boundary.

## Dependency direction

```text
cli ------> transport ------> runtime ------> protocol
 |                              ^                ^
 +----------> platform ---------+----------------+
```

- `protocol` contains the stable public wire types and no operating-system code.
- `runtime` keeps command orchestration separate from authorized session state,
  opaque-reference projection, capability enforcement, stale-observation
  rejection, and transient artifacts.
- `platform` implements the internal driver trait with public OS APIs.
- `transport` exposes authenticated local IPC and never listens on TCP.
- `cli` is the process entrypoint and composition root.

The runtime is asynchronous, but native objects are never moved through an
arbitrary async worker pool. Each driver owns long-lived operating-system
actors with the thread and run-loop model required by that platform.

```text
                         +-> capture actor ----> one-shot / warm capture session
runtime -> platform bus -+-> semantic actor ---> AX / UI Automation cache
                         +-> input actor ------> serialized foreground input
```

The platform bus is bounded. Backpressure and idempotent caller deadlines are
part of the contract rather than accidental properties of the Tokio scheduler.
Once admitted, a command runs to a recorded result even if its caller stops
waiting; retries reconcile through the same request identity.

## Trust boundaries

The desktop host starts the daemon and gives trusted clients a private endpoint
plus an authorization token. The token authenticates a fully trusted policy
host, not an untrusted model client. A compromised host or token holder can open
many sessions across application identities it can discover; structural bounds
limit each session and resource use but cannot make that compromise harmless.
The token therefore permits creating only `read_only` or explicitly bounded
sessions and must never be exposed to an agent.

In Nexus, the host additionally binds each session to the authenticated owner,
conversation round, setting, and approval state; those product facts
deliberately stay outside this repository.

The Unix socket is mode `0600`; the Windows pipe uses a protected DACL granting
access only to LocalSystem and the object owner, and rejects remote clients.
Connection tasks and distinct in-flight requests have independent hard bounds.
Capacity is rejected before a new side effect reaches the runtime.

Platform identifiers stay behind the runtime. Public callers receive opaque
application, window, element, observation, session, and artifact references.
Every mutation is authorized against its session and requires a fresh
observation of the exact target window.

## Data lifetime

Screenshots and accessibility text are untrusted, transient observations.
Screenshot files are created in one random runtime generation below a
host-selected private root, use restrictive permissions, and are deleted when
their session expires or closes. Graceful runtime teardown deletes its entire
generation without deleting the host root. Neither screenshot bytes nor typed
text may enter logs.

## Observation pipeline

An observation is a coherent join of two independent native snapshots:

1. Resolve the target's generational identity and geometry.
2. Capture pixels and fetch the requested semantic view in parallel.
3. Re-read target identity and geometry.
4. Publish only if both branches describe the same generation; retry once on
   a geometry race, otherwise return a retryable stale-target error.

Native handles, process IDs, AX objects, and COM interfaces never cross the
driver boundary. Runtime-visible driver keys include process-lifetime and
window-generation facts so PID/HWND reuse cannot silently retarget authority.

Pixel and semantic coordinates are deliberately distinct. Window and element
bounds use logical top-left screen coordinates. Pixel actions use integer
coordinates in the exact captured image. Every observation carries the affine
mapping between those spaces; callers never infer Retina or per-monitor DPI
scales themselves.

## Performance architecture

- Windows capture pipelines are pooled per window with a short idle lease and
  an LRU cap. macOS currently uses the public one-shot screenshot route; a warm
  stream pool must meet the same coherence and memory gates before replacing
  it.
- macOS accessibility attributes are fetched in batches and elements are
  revalidated immediately before action. Windows uses UI Automation cache
  requests rather than one cross-process call per property.
- Semantic traversal is iterative and bounded by nodes, depth, bytes, and wall
  time whenever the native provider returns control. Budget exhaustion yields
  an explicit partial tree. An uninterruptible call inside a hung third-party
  provider is ultimately bounded by host-side sidecar termination and receives
  an indeterminate result; it is never reported as a clean cancellation.
- Screenshot encoding and hashing happen off the native capture actor. Only
  the newest unconsumed frame is retained per target.
- The service performs no active idle polling. Deadline-driven lease cleanup is
  allowed; observation requests still pull fresh state.

Normative budgets and overload behavior are defined in
`docs/specs/runtime-contract-v1.md`.

## v0.1 implementation boundary

The current implementation uses public APIs:

- macOS: ScreenCaptureKit is the primary capture route, AXUIElement provides
  semantic observation/actions, and CGEvent provides foreground input.
- Windows: Windows.Graphics.Capture is the primary capture route, UI Automation
  provides cached semantic observation/actions, and SendInput provides
  foreground input.

Semantic accessibility actions may work without foreground activation. Pixel
input is foreground-only. Private macOS background APIs are explicitly outside
the baseline and must never become a hidden fallback. Legacy capture APIs may
exist only as separately reported compatibility drivers; they cannot be a
silent fallback that changes privacy, fidelity, or performance semantics.

Implementation does not imply supported-release status. The maintained-hardware
fixture, benchmark, soak, and signed-package gates in the runtime contract must
pass before an operating-system route is declared release-supported.
