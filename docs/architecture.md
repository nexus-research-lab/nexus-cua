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
host ----> transport ------> runtime ------> protocol
 |            ^                 ^                ^
 |            +---- cli <--- provider            |
 +------------------------------+----------------+
                 platform ------+----------------+
```

- `protocol` contains the stable public wire types and no operating-system code.
- `runtime` keeps command orchestration separate from authorized session state,
  opaque-reference projection, capability enforcement, stale-observation
  rejection, and transient artifacts.
- `platform` implements the internal driver trait with public OS APIs.
- `transport` exposes authenticated local IPC and never listens on TCP.
- `host` optionally registers multiple independent desktop runtimes, issues
  bounded lease/epoch capabilities, and frames authenticated remote requests
  over a caller-supplied secure byte stream. It does not provision machines or
  open a listener.
- `provider` authenticates owner-private host registrations with Keychain/DPAPI
  protected secrets, revalidates the exact launching host executable, and
  issues short challenge-bound adapter grants without exposing credentials to
  model input or configuration.
- `cli` is the `nexuscua` sidecar/MCP process entrypoint, diagnostic client,
  and composition root. The Cargo package remains `nexus-cua`; the old binary
  name is not registered as a compatibility alias.
- `sdk/go` and `sdk/python` are typed trusted-host clients above the private
  transport. They do not own policy, sidecar installation, or agent behavior.

## Integration layering

```text
agent runtime -> agent adapter -> trusted product host -> nexus-cua -> native OS
                    |                    |
              CLI + Skill or MCP    policy + lifecycle
```

The first arrow is not part of the core runtime contract. A product may expose
a scoped CLI plus Skill, an MCP server, or another custom tool surface. Those
adapters translate agent intent into the host's narrower policy context; they
never replace the trusted host or receive ambient desktop authority from the
runtime.

Nexus uses a downstream CLI-plus-Skill adapter. The Nexus Product process owns
policy, approval, package management, supervision, receipts, and audit. Its
connected Agent Runtime owns model invocation and the agent loop. The Agent SDK
Bridge connects those components, but neither the Bridge nor `nexus-cua`
contains reasoning.

The runtime is asynchronous, but native objects are never moved through an
arbitrary async worker pool. Each driver owns long-lived operating-system
actors with the thread and run-loop model required by that platform.

```text
                         +-> capture actor ----> one-shot / warm capture session
runtime -> platform bus -+-> semantic actor ---> AX / UI Automation cache
                         +-> input actor ------> serialized foreground input
                         +-> clipboard actor --> guarded native pasteboard
```

The platform bus is bounded. Backpressure and idempotent caller deadlines are
part of the contract rather than accidental properties of the Tokio scheduler.
Once admitted, a command runs to a recorded result even if its caller stops
waiting; retries reconcile through the same request identity.

Independent desktops are composed above this per-runtime actor model:

```text
trusted scheduler -> bounded worker registry -> exact worker lease + epoch
                                          |
remote secure stream -> authenticated gateway -> worker dispatcher -> runtime
                                          |
                                 checksum-bound artifact ticket
```

The gateway never returns worker-local screenshot paths. Placement has no
pending queue, and a worker reconnect produces a fresh epoch that invalidates
old leases before dispatch.

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
launch-discovery, running-discovery, application, window, display, element,
observation, clipboard, session, and artifact references. Discovery is
available only to an authenticated policy host and does not create authority.
Installed launch refs
are short-lived, single-use, and bound to freshly revalidated native
registration provenance; they never contain caller-authored paths or arguments.
Successful launch returns a separate running-generation discovery ref. Session
creation revalidates that ref against the same runtime epoch, process
generation, and executable identity. Every session mutation requires a fresh
observation of the exact target window.

A host may separately authorize physical-display overview. Display enumeration
and capture use their own opaque refs and produce transient read-only
observations. They do not populate the window observation table, cannot guard
an action, and never substitute for failed exact-window capture. This keeps the
real user desktop visible to a CUA without requiring a VM while preserving the
exact-window mutation boundary.

Clipboard authority is independent from application, display, and input
authority. A read returns bounded plain text plus a session-scoped opaque guard;
the runtime retains only the native change marker. A write consumes that guard
before dispatch and fails stale when another actor changed the clipboard. Native
pasteboard handles remain on a dedicated bounded platform actor, and clipboard
text never enters runtime logs or durable state.

## Data lifetime

Screenshots, accessibility text, and clipboard text are untrusted, transient
observations.
Screenshot files are created in one random runtime generation below a
host-selected private root, use restrictive permissions, and are deleted when
their session expires or closes. Graceful runtime teardown deletes its entire
generation without deleting the host root. Neither screenshot bytes nor typed
text may enter logs.

Adapters that return image bytes instead of artifact references use the
runtime's checked single-use artifact take: identity, path ownership, length,
and digest are revalidated before the exact file is removed. The MCP adapter
uses this route, redacts the private path from structured content, and keeps
only a bounded 32 MiB aggregate in-memory image cache for normalized actions
and zoom. A killed stdio child therefore cannot strand already-returned MCP
screenshots on disk.

One nearest-deadline task expires discovery refs and sessions without polling.
It owns no native object, stores no frame, and wakes only when a deadline or
lifecycle revision changes. Explicit shutdown first closes admission, waits for
admitted runtime calls, stops this task, and removes remaining session
artifacts.

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

Multiple Agent sessions may share one bounded runtime, but one OS desktop still
has one serialized foreground-input lane. The runtime uses fail-fast admission
for that lane, so contention returns `busy/not_dispatched` without consuming the
observation or building an unbounded worker queue. A separate host grant can
restore the exact prior foreground process generation after temporary input;
the native actor skips restoration if foreground changed after dispatch.
Remote desktop workers and optional
VM-backed checkpoint/reset/clone live above the local-only sidecar boundary;
their truthful capability split is defined in
[`execution environments`](specs/execution-environments.md).

## v0.1 implementation boundary

The current implementation uses public APIs:

- macOS: ScreenCaptureKit is the primary capture route, AXUIElement provides
  semantic observation/actions, and CGEvent provides foreground input. After
  active-application confirmation, keyboard events are routed to the bound
  process so a concurrent focus change cannot leak typed content to another
  application; pointer events retain global foreground semantics. Installed
  bundles are enumerated within standard application roots and launched through
  `NSWorkspace` only after bundle/file/signing revalidation.
- Windows: Windows.Graphics.Capture is the primary capture route, UI Automation
  provides cached semantic observation/actions, and SendInput provides
  foreground input. A dedicated COM actor resolves only bounded local Start
  Menu executable shortcuts without Shell tracking/repair and launches a
  freshly revalidated registration through `ShellExecuteExW`.

Semantic accessibility actions may work without foreground activation. The
negotiated best-effort background pixel-click route targets an exact process or
window and never activates or silently escalates; other pixel/keyboard input
remains foreground-only. Private macOS background APIs are explicitly outside
the baseline and must never become a hidden fallback. Legacy capture APIs may
exist only as separately reported compatibility drivers; they cannot be a
silent fallback that changes privacy, fidelity, or performance semantics.

Implementation does not imply supported-release status. The maintained-hardware
fixture, benchmark, soak, and signed-package gates in the runtime contract must
pass before an operating-system route is declared release-supported.
