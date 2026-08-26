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
2. A client reads capabilities and permission status.
3. The authenticated policy host calls `discover_applications` and receives
   display metadata plus short-lived opaque discovery references.
4. The host opens a `read_only` or `bounded` session using only unexpired
   discovery references, then lists authorized applications/windows.
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
`read_only` also requires at least one discovered application but rejects all
action or foreground-input authority.

Discovery is a transport-authenticated, read-only bootstrap operation. Each
descriptor contains a display name, stable bundle/executable identifier,
foreground state, best-effort platform provenance, and a random runtime-local
reference. The reference grants no authority by itself, expires within 30
seconds, and binds the runtime epoch, process generation, and normalized
bundle/executable identity. `open_session` re-enumerates the host and requires
the same generation and identity. Missing, expired, restarted, or replaced
targets return `stale_discovery` with recovery action `discover_applications`.
No PID, HWND, or native object is accepted as public authority.

The runtime bounds live sessions, allowlist count, individual identifier size,
and aggregate allowlist bytes. Capacity exhaustion is a retryable `busy`
response and never evicts another live authority session.

Version 1 exposes exact top-level windows only. Complete-desktop capture is not
represented by a dormant manifest flag or inferred from foreground authority.

The protocol deliberately has no unrestricted mode.

## Observation integrity

Platform handles are not public authority. The runtime maps them to random,
session-scoped references. Element references are scoped to one observation.
The runtime rejects expired, invalidated, cross-session, cross-window, and
driver-stale observations with stable error codes.

## Image transport

Screenshots are written to a private runtime-owned artifact path. The caller
cannot choose this path. The response includes an opaque artifact reference,
MIME type, dimensions, byte length, SHA-256 digest, and absolute local path.
Artifacts expire with their session.

## Reconciliation

The canonical serialized `command` is the request identity payload;
`authorization` and `timeout_ms` are not. Concurrent requests with the same
`request_id` and command join one execution. Reusing the ID with a different
command fails closed. A timed-out caller may only extend its bounded wait and
retry the same command and ID.

Completed results remain replayable for a configurable horizon whose default
is 10 minutes. The bounded ledger never evicts an unexpired result to admit a
new mutation; capacity returns `busy`. After the horizon the outcome is
indeterminate and an SDK must not manufacture a new request ID. A sidecar
restart creates a new runtime epoch, destroys the ledger, and invalidates every
discovery, session, observation, and artifact reference.

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
