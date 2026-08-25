# Nexus Computer Use Product Integration Contract

Status: normative for the behavior required of `0.1.x` runtime consumers. The
section labeled "Nexus adapter target" specifies a downstream design target; it
is not an implementation claim for this repository.

Nexus Computer Use Runtime is an execution component, not an agent framework. A
product owns the model loop, user consent, policy, process supervision, and
durable audit. The runtime owns native observation, narrow session authority,
action delivery, verification, and transient artifacts.

## One core, two integration paths

Nexus is the primary product consumer, but it is not encoded into the runtime.
The project deliberately supports two front doors above the same core:

- **Nexus-native:** a Go supervisor and client, built-in Computer Use Skill,
  round-scoped `nexus computer` command, product approvals, and typed receipts.
- **Independent host:** the embeddable Rust API or authenticated sidecar
  protocol, consumed through a host-owned SDK, CLI, or optional adapter.

The paths share protocol and driver behavior. They do not share product
identity or ambient authority. A generic integration must meet the same session,
target, freshness, retry, and sensitive-data rules as Nexus.

## Supported consumption modes

| Consumer | Recommended surface | Purpose |
| --- | --- | --- |
| Rust desktop product | `nexus-cua-runtime` plus `nexus-cua-platform` | In-process composition with the same protocol semantics |
| Go, TypeScript, Python, or another product | Versioned `nexus-cua` sidecar and private IPC | Process isolation and language-neutral integration |
| Maintainer or support engineer | `nexus-cua doctor`, `schema`, and `request` | Diagnostics and contract inspection |
| Third-party agent framework | Product-owned adapter above private IPC | CLI, SDK, or optional MCP compatibility without weakening runtime authority |

The generic `request` subcommand is a diagnostic client. A product must not
hand its endpoint, token file, arbitrary command file, or artifact root to a
model. Agent-facing commands belong to the embedding product so it can bind
each call to an authenticated user, conversation, round, and approval policy.
For a non-conversational host, the equivalent boundary is one authenticated job
or transaction step with its own reviewed authority scope.

## Host topology

```text
user setting + OS permissions
            |
            v
product policy / round grant ----> agent-facing CLI + Skill
            |                              |
            | private input + receipt      |
            v                              v
sidecar supervisor ----------------> Computer Use IPC
                                            |
                                 capability session
                                            |
                              exact native top-level window
```

The effective authority is the intersection of five facts:

1. The product build supports Computer Use on the current platform.
2. The owner has explicitly enabled Computer Use.
3. The pinned sidecar is healthy and speaks the expected protocol.
4. Required operating-system permissions are granted.
5. The current round has a live runtime session whose manifest permits the
   exact application and action.

No single fact implies another. In particular, possessing the transport token
does not grant an unrestricted desktop session, and an operating-system
permission does not imply product consent.

## Process lifecycle

The host should pin an exact `nexus-cua` package version, verify its checksum
and provenance, and create a private per-owner state directory. On every start
it creates a fresh transport token, starts one sidecar with a private Unix
socket or local-only Windows named pipe, and checks `get_capabilities` plus
`get_permission_status` before advertising the feature.

To grant an operation, the trusted host calls `discover_applications`, presents
the returned display/provenance facts through its policy or approval surface,
and opens the session with the selected short-lived refs. It never constructs
an allowlist from a model-authored bundle ID, image path, PID, or window handle.
`stale_discovery` means rediscover and obtain a fresh policy decision when the
identity facts changed.

The service applies mode `0600` to its Unix socket and an owner-and-SYSTEM-only
protected DACL to its Windows pipe. The token file and artifact base are still
host responsibilities and must inherit an owner-private directory ACL. Local
connections and distinct in-flight commands are independently bounded; `busy`
is a capacity signal, never permission to bypass the sidecar.

When Computer Use is disabled, the host must atomically stop issuing new round
grants, close all sessions it owns, reconcile already admitted requests for a
bounded period, and stop the sidecar. An admitted mutation may complete;
disabling cannot undo an operating-system action that already happened. The
host reports that distinction instead of claiming cancellation. If a native
provider remains hung when the bounded reconciliation period ends, the host
terminates the sidecar, reports the result as indeterminate, and never replays
that mutation under a new request identity.

Each runtime creates a private artifact generation below the host-selected
artifact root. Graceful process teardown removes only that generation. After a
crash and before restart, the host may remove stale generation directories only
after it has established that no old sidecar is alive. Screenshots promoted to
a durable product artifact must be copied explicitly; runtime paths are never
durable references.

## Round and session lifecycle

A product opens a fresh session for one approved authority scope. For a
conversational agent this is normally one physical model round or a smaller
operation; for non-conversational automation it is one authenticated job or
transaction step. Observation-only work uses `read_only`. Mutating work uses
`bounded` with:

- application identities selected from current discovery, not model-authored
  native handles;
- the minimum explicit action set;
- foreground input disabled unless the operation requires it; and
- a short TTL bounded by the host.

The session manifest authorizes application identities, not one permanent
window. Within an allowed application, the caller lists windows, observes one
exact window, and supplies that observation to every mutation. The mutation's
authority unit is therefore one observed top-level window even though a session
may operate on several windows from its application allowlist. After a
successful mutation it must observe again. A stale observation is a normal
recovery edge, not permission to bypass the guard. The product closes the
session at round or operation end, user cancellation, permission revocation,
owner switch, or sidecar health loss.

Transport retries preserve the exact `request_id` and command. A caller that
times out may increase only `timeout_ms` while reconciling. It must never create
a new request identity merely because a mutating call timed out. The default
reconciliation horizon is 10 minutes; after it ends, an unresolved mutation is
indeterminate and must not be replayed. A full ledger returns `busy` instead of
discarding an unexpired result.

## Nexus adapter target

The Nexus application should consume the sidecar through a Go host service and
expose it to an agent through a built-in Computer Use Skill plus a round-scoped
`nexus computer` command. This is intentionally not an MCP server in Nexus.

The command wrapper should reuse Nexus's private round input-slot and typed
receipt pattern. The model receives neither the sidecar token nor a path where
it can construct arbitrary protocol JSON. The host derives owner, session,
round, application allowlist, TTL, and approval state; the model supplies only
the operation-level intent allowed by the current command schema.

The user preference defaults to off. Turning it off revokes command admission
immediately. A Skill already present in an active model context may remain
described until that round ends, but every call fails closed at the host and
runtime layers. New rounds omit the capability until the user enables it
again. Sidecar crashes invalidate all opaque references and sessions; Nexus
restarts the pinned binary but never replays an old mutation under a fresh
request identity.

This repository publishes the runtime packages. Nexus owns its product-facing
preference, settings UI, sidecar supervisor, Go client, command receipts,
Skill, and audit projection. Those layers must remain outside `nexus-cua` so
other products can consume the same runtime without Nexus domain dependencies.

## Browser independence

Browser and Computer Use are separate capabilities and settings:

| Computer Use | Browser | Behavior |
| --- | --- | --- |
| off | off | No computer-control capability |
| on | off | Native visible-window control, including browser chrome and page pixels; no DOM, CDP, network, history, or tab semantics |
| off | on | Existing Browser extension behavior only |
| on | on | Browser handles page semantics; Computer Use handles native applications and browser chrome when explicitly selected |

The Skill may recommend a route, but it cannot merge authorities. Browser
failure does not silently widen a request into native pixel control, and
Computer Use availability never enables complete CDP. Switching routes requires
that the other capability is independently enabled and that its own policy
authorizes the operation.

## Compatibility and upgrades

The host performs two checks independently:

- package compatibility through the binary semantic version; and
- wire compatibility through `protocol_version` and `get_capabilities`.

During `0.1.x`, consumers should pin an exact package. A host may stage a new
binary beside the active version, run `doctor` and a signed-package smoke test,
then switch only after the previous sidecar exits. It must not mix one binary's
endpoint, token, sessions, or artifact generation with another version.

Non-Rust consumers use the committed schemas in `schemas/nexus.cua.v1/` or
export the same contract from their pinned local binary with `nexus-cua
schema`. Compatibility fixtures in `fixtures/compatibility/nexus.cua.v1/`
provide the cross-language conformance corpus; maintained reference clients are
a later milestone. Frames are four-byte big-endian lengths followed by closed
JSON and are bounded before payload allocation. Implementations must preserve
unknown-variant failure, opaque references, redacted sensitive values, stable
error codes, and same-request retry identity rather than translating the
protocol into a looser map.

## Observability

The service emits structured events for caller wait latency and detached
execution latency. Events contain request identity, operation name, success or
stable error code, and elapsed microseconds. They never contain command JSON,
typed text, accessibility values, screenshots, transport tokens, platform
handles, or artifact contents.

Products should correlate those events with their own private round receipt,
not inject conversation text into runtime logs. Release performance decisions
use hardware benchmark distributions and soak results defined by the runtime
contract; developer-machine anecdotes are not release evidence.
