# Nexus CUA Protocol v1

Status: normative for the `0.1.x` development line.

The protocol identifier is `nexus.cua.v1`. Messages use closed, tagged JSON
objects. Unknown fields are rejected by decoders at every trust boundary.

## Lifecycle

1. A host starts the daemon with a private local endpoint and authorization
   token file.
2. A client reads capabilities and permission status.
3. The client opens a `read_only` or `bounded` session.
4. It lists applications/windows and receives opaque references.
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

`bounded` additionally requires an exact application allowlist, an explicit
action allowlist, a foreground-input flag, and a finite session TTL. An empty
application or action allowlist authorizes nothing.

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
