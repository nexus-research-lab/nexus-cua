# Nexus Computer Use Protocol v1

Status: normative for the `0.1.x` development line.

The protocol identifier is `nexus.cua.v1`. Messages use closed, tagged JSON
objects. Unknown fields are rejected by decoders at every trust boundary. The
`cua` namespace identifies the computer-using-agent domain served by Nexus
Computer Use Runtime; it does not mean the runtime contains an agent.

`request_id` is a non-empty, normalized ASCII identity of at most 128 bytes.
It may contain letters, digits, `-`, `_`, `.`, and `:`. Caller wait time is not
part of request identity: a timed-out caller retries the exact command under
the same ID with an equal or longer bounded `timeout_ms` to reconcile.

## Lifecycle

1. A host starts the daemon with a private local endpoint and authorization
   token file.
2. A client reads capabilities and permission status. An authenticated policy
   host may optionally discover installed applications, launch one through a
   short-lived opaque launch reference, and receive a fresh running-application
   discovery reference.
3. The authenticated policy host calls `discover_applications` and receives
   display metadata plus short-lived opaque discovery references.
4. The host opens a `read_only` or `bounded` session using only unexpired
   discovery references. A separately authorized display-only or clipboard-only
   session may omit application refs. It then lists authorized
   applications/windows or physical displays, or reads the clipboard.
5. It observes a window and receives an `observation_id`, element references,
   and optional transient screenshot artifact.
6. A mutation supplies the exact session, window, and observation identities.
7. The runtime checks capability, target ownership, observation age and driver
   fingerprint before executing.
8. A successful mutation invalidates all observations for that window.
9. Closing or expiring the session releases leases and deletes artifacts.

## Authorization

`read_only` permits diagnostics, enumeration, observation, and verification.
It never permits input.

`bounded` additionally requires exact running applications selected by
`DiscoveryRef`, an explicit action allowlist, a foreground-input flag, and a
finite session TTL. A manifest cannot contain caller-authored application IDs.
`read_only` rejects all action or foreground-input authority. It normally
requires at least one discovered application; a manifest with
`allow_display_capture=true` or `allow_clipboard_read=true` may instead omit
application refs. Clipboard write authority requires read authority and is
rejected in `read_only` mode.

Discovery is a transport-authenticated, read-only bootstrap operation. Each
descriptor contains a display name, stable bundle/executable identifier,
foreground state, best-effort platform provenance, and a random runtime-local
reference. The reference grants no authority by itself, expires within 30
seconds, and binds the runtime epoch, process generation, and normalized
bundle/executable identity. `open_session` re-enumerates the host and requires
the same generation and identity. Missing, expired, restarted, or replaced
targets return `stale_discovery` with recovery action `discover_applications`.
No PID, HWND, or native object is accepted as public authority.

Installed-application discovery is a separate, bounded, transport-authenticated
bootstrap operation. A descriptor contains a stable application identity,
display metadata, native provenance, and a random short-lived `LaunchRef`.
`launch_application` accepts only that reference plus a boolean ceiling on
foreground activation; it never accepts a path, executable, arguments, working
directory, URL, PID, or native registration handle. The runtime re-enumerates
the installed registration and requires the same identity, launch target, and
provenance before consuming the reference exactly once. It rejects an already
running matching identity rather than creating an ambiguous second generation.
After native admission, failure is indeterminate and is reconciled only under
the original request ID. Success returns a fresh running-application
`DiscoveryRef`, which must still pass normal `open_session` revalidation.
Drivers advertise `application_lifecycle = ["launch"]` only where this complete
route is implemented. macOS uses reviewed application bundles; Windows uses
reviewed executable shortcuts discovered only under the current user's and
machine-wide Start Menu registration roots.

The runtime bounds live sessions, allowlist count, individual identifier size,
and aggregate allowlist bytes. Capacity exhaustion is a retryable `busy`
response and never evicts another live authority session.

Version 1 exposes exact top-level windows for actions and a separate read-only
physical-display overview. `allow_display_capture` is an explicit host grant;
it is never inferred from foreground input, application authority, or window
capture. `list_displays` returns session-scoped opaque `DisplayRef` values with
logical top-left bounds and a primary flag. `observe_display` revalidates the
exact display and returns a `DisplayObservation` with transient PNG metadata.
That observation can never guard `perform_action`, contains no element refs,
and is not a fallback when exact-window capture fails. The caller must select,
list, and observe an authorized exact window before mutation.

Plain-text clipboard access is a third, independently authorized scope. Drivers
advertise `read_text` and `write_text` separately. `read_clipboard` returns a
bounded nullable UTF-8 value, its exact byte length, and a session-scoped opaque
`ClipboardRef`; the runtime does not retain the value. `write_clipboard`
requires the latest unconsumed ref, consumes it before native dispatch, and
fails with `stale_clipboard/not_dispatched` if the native change marker no
longer matches. Success returns a fresh ref and coherent native read-back
evidence. The protocol supports plain text only; clipboard content is sensitive
and must never be logged or persisted.

`set_window_bounds` changes logical top-left screen geometry and
`set_window_minimized` minimizes or restores the exact guarded window. Both
require their own action-kind grants, are observation-bound, invalidate prior
observations, and report the `system` delivery route; neither implies
foreground keyboard or pointer authority.

`terminate_application` requests cooperative exit of the exact process
generation that owns the guarded window. It has a separate action-kind grant,
accepts no PID, path, command line, or force flag, requires no foreground-input
grant, and reports the `system` route. Once native dispatch may have occurred,
the runtime invalidates observations for that application generation in every
session and removes matching discovery references. A bounded confirmation
timeout is `indeterminate`; callers verify application absence and never retry
under a new request identity.

`set_range_value` accepts an observed element reference and one absolute finite
number. The same immutable observation must expose a coherent numeric current
value, inclusive minimum and maximum, optional positive step sizes, and a
native read-only flag. The runtime rejects disabled, read-only, non-finite, or
out-of-range requests before driver dispatch. The driver then uses the native
semantic range API; it never approximates the request with pointer or keyboard
input and never requires foreground-input authority.

`click_point` carries a closed `delivery` preference. Omission means
`foreground` for wire compatibility. `background` targets the exact guarded
application/window through the negotiated public platform route, does not
require the session's foreground-input flag, and must either report
`background` delivery or fail without silently activating or retrying in the
foreground. A successful dispatch is not proof that the application accepted
the event, so the caller observes fresh state. It may issue a new explicit
foreground action only when the session separately grants foreground input;
an indeterminate background mutation is reconciled under its original request
identity and is never retried as a new action.

Every successful `perform_action` result separates native delivery from direct
effect evidence. `effect_status = confirmed` is permitted only with
`evidence_kind = native_readback`, meaning the owning native actor boundedly
read back the requested operating-system state or confirmed exact process exit.
It does not claim unrelated downstream application behavior. Pointer,
keyboard, semantic, and best-effort background dispatch normally return
`unverified` plus `dispatch_acceptance`; callers must use a fresh observation or
`verify_state` before treating the application-visible effect as established.
The runtime rejects incoherent status/evidence pairs. A platform refusal remains
a `not_dispatched` error, while partial or uncertain native dispatch remains an
`indeterminate` error rather than a misleading partial success.

A bounded session may independently set
`restore_foreground_after_input = true`, but only together with foreground
input authority. For temporary pointer, keyboard, text, scroll, and drag
actions, the owning native actor records the exact prior foreground process
generation, performs the authorized input, and restores that owner only if the
guarded target still owns foreground. A user or system focus change after
dispatch yields `skipped_owner_changed` and is never overwritten. The action
result always carries a closed `foreground_restoration` value: `not_requested`,
`not_needed`, `restored`, `skipped_owner_changed`, `target_unavailable`, or
`failed`. Explicit `focus_window`, semantic/background delivery, lifecycle,
and window-system actions report `not_requested`. The runtime rejects both a
missing required restoration result and any driver attempt to restore without
host authority.

The protocol deliberately has no unrestricted mode.

## Observation integrity

Platform handles are not public authority. The runtime maps them to random,
session-scoped references. Element references are scoped to one observation.
The runtime rejects expired, invalidated, cross-session, cross-window, and
driver-stale observations with stable error codes.

Display refs are reissued by fresh display enumeration and reveal no native
monitor handle. Display observations are read-only artifacts, not mutation
guards, and their IDs are rejected anywhere a window observation is required.

Semantic observations expose required `enabled`/`focused` facts, nullable
`selected`/`checked`/`expanded` facts, and nullable coherent numeric range
metadata. Null means unavailable or non-binary, not false. `verify_state` can
evaluate a closed `element_state` predicate against a fresh platform snapshot
selected by role and/or accessible name.

## Image transport

Screenshots are written to a private runtime-owned artifact path. The caller
cannot choose this path. The response includes an opaque artifact reference,
MIME type, dimensions, byte length, SHA-256 digest, and absolute local path.
Artifacts expire with their session.

## Reconciliation

The canonical serialized `command` is the request identity payload;
`authorization` and `timeout_ms` are not. Concurrent requests with the same
`request_id` and command join one execution. Reusing the ID with a different
command fails closed. A low-level transport caller may extend its bounded wait
with the same command and ID. Public CLI/MCP adapters instead use the read-only
`reconcile_mutation` operation, which observes the retained entry by original
ID and never redispatches the command.

Completed results remain replayable for a configurable horizon whose default
is 10 minutes. The bounded ledger never evicts an unexpired result to admit a
new mutation; capacity returns `busy`. After the horizon the outcome is
indeterminate and an SDK must not manufacture a new request ID. A sidecar
restart creates a new runtime epoch, destroys the ledger, and invalidates every
discovery, session, observation, and artifact reference.

`mutation_reconciled` returns the queried ID, current runtime epoch, optional
remote worker epoch, admission and disposition, optional dispatch/effect/error
evidence, and whether a fresh observation is required. An unknown entry is not
proof that the old effect did not occur; the caller compares the retained epoch
with the epoch recorded when it submitted the mutation.

Every error carries a required `mutation_status`:

- `not_applicable` means the command was not a mutation;
- `not_dispatched` means the runtime proved the mutation did not reach the
  target; and
- `indeterminate` means dispatch may have occurred and the caller must not
  issue an equivalent mutation under a new request ID.

Transport validation, authority checks, stale observations, coordinate checks,
native queue admission, and native action preflight report `not_dispatched` for
a failed `perform_action`. Once a native action call is admitted, an otherwise
unclassified failure is conservatively `indeterminate`. Only an indeterminate
failure or admitted request deadline is reconciled by waiting longer with the
same request ID. A new request ID is a new mutation and is forbidden as a
recovery mechanism.

`target_unresponsive` is the stable error for a native accessibility provider
that exceeds the driver's bounded provider timeout. Observation failures always
use `not_applicable`; action preflight uses `not_dispatched`; and a timeout from
inside the native mutation call uses `indeterminate`.

## Schemas and compatibility fixtures

The normative generated request and response schemas live under
`schemas/nexus.cua.v1/`. `make schema-check` regenerates them from the Rust wire
types and fails on drift. Compatibility fixtures under
`fixtures/compatibility/nexus.cua.v1/` cover every command, success result, and
stable error code plus unknown fields, unknown tagged variants, protocol
mismatch, and frame boundaries. See [compatibility policy](../compatibility.md)
for versioning rules.
