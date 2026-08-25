# Architecture

Nexus CUA is a local, model-neutral desktop driver. It owns observation,
authorization, action delivery, and verification. It does not own agent
reasoning or user-facing consent.

## Dependency direction

```text
cli -> transport -> runtime -> protocol
                         \-> platform
platform -----------------> protocol
```

- `protocol` contains the stable public wire types and no operating-system code.
- `runtime` owns sessions, opaque references, capability enforcement, stale
  observation rejection, and transient artifacts.
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
plus an authorization token. Holding that token permits creating only
`read_only` or explicitly bounded sessions. It never implies unrestricted
desktop access.

Platform identifiers stay behind the runtime. Public callers receive opaque
application, window, element, observation, session, and artifact references.
Every mutation is authorized against its session and requires a fresh
observation of the exact target window.

## Data lifetime

Screenshots and accessibility text are untrusted, transient observations.
Screenshot files are created by the runtime under a host-selected private root,
use restrictive permissions, and are deleted when their session expires or
closes. Neither screenshot bytes nor typed text may enter logs.

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
  time. A timed-out provider yields an explicit partial tree, never an
  unbounded hung request.
- Screenshot encoding and hashing happen off the native capture actor. Only
  the newest unconsumed frame is retained per target.
- The service performs no active idle polling. Deadline-driven lease cleanup is
  allowed; observation requests still pull fresh state.

Normative budgets and overload behavior are defined in
`docs/specs/runtime-contract-v1.md`.

## v0.1 platform boundary

The first release uses public APIs:

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
