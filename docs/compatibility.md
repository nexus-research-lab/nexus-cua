# Protocol Compatibility Policy

Status: normative for the `0.2.0-rc.4` testing prerelease.

Package and wire compatibility are independent. Hosts pin the package version
they install and negotiate the exact `protocol_version` they speak. The current
wire identifier is `nexus.cua.v1`; a readable envelope with another identifier
receives a current-format `protocol_mismatch` response and no command executes.

## Closed wire contract

Requests, responses, commands, results, errors, actions, predicates, and
platform provenance are closed tagged values. Unknown fields and variants fail
decoding. Public references are opaque strings: consumers may compare or store
them only for their documented lifetime and must not parse authority from
their contents.

The published `v0.1.0` tag was a pipeline test without production users. The
2026-11 development baseline intentionally resets the closed v1 contract:
all pointer actions now take `NormalizedPoint` (0..1000), and scroll requires
an explicit target point. Old SDKs and test binaries must be rebuilt together;
there is no legacy pixel-coordinate fallback or parallel compatibility layer.
Hosts must pin a matching runtime and SDK source revision. The `v0.2.0-rc.1` testing prerelease includes this reset, user-takeover errors,
application-scene capture and optional post-action observation. It requires the
matching SDK revision; it is not a stable production compatibility promise.

`v0.2.0-rc.2` fixes native macOS window identity and focus recovery without
changing this wire contract. Hosts still pin the runtime and SDK versions.
`v0.2.0-rc.3` distinguishes between-action input (bounded idle wait and fresh
observation within the existing grant) from native-dispatch takeover (latched
`user_active`). No wire fields or error codes change.
`v0.2.0-rc.4` marks macOS AX window lookup failures before a window mutation
as `not_dispatched`; a fresh metadata observation and authorized `focus_window`
can recover the same target grant. It does not weaken post-dispatch uncertainty.

## Generated schemas

`schemas/nexus.cua.v1/request.schema.json` and
`schemas/nexus.cua.v1/response.schema.json` are generated from the Rust source
types. Change the types first, run `make schema-write`, and commit both together.
CI runs `make schema-check` and rejects hand-edited or stale generated files.

## Compatibility fixtures

`fixtures/compatibility/nexus.cua.v1/` is the language-neutral conformance
corpus. Every command, success result, and stable error code appears in its
inventory. Negative fixtures cover unknown fields and variants; transport
fixtures cover a readable protocol mismatch and maximum frame behavior.
Rust tests exhaustively match the enum inventory, so adding a variant without a
fixture fails review or CI.

Client implementations must also prove that authorization tokens and sensitive
typed text are redacted from diagnostics. Screenshot and accessibility content
must remain transient and must not be copied into fixture or test logs.

The official Go, Python, and TypeScript clients consume this same corpus in
their unit tests. All reject unknown closed values, preserve opaque identifiers, read the
transport token only from a host-private file, and retain the exact mutation
request identity needed for reconciliation.

## Retry compatibility

The canonical serialized command plus `request_id` identifies one execution.
Clients may lengthen only `timeout_ms` while reconciling. The service retains
completed responses for a configurable horizon of 10 minutes by default and
will not evict an unexpired result to admit new mutation work. After that
horizon an unresolved mutation is indeterminate; a client must not create a new
request ID and replay it. Restarting the runtime invalidates its in-memory
ledger and every runtime-local reference.

Clients must decode the required `mutation_status` on every error. They may
retry a failed mutation as a new request only when the status is
`not_dispatched` and their host policy still authorizes it. `indeterminate`
permits same-request reconciliation only. Unknown error codes or mutation
statuses fail closed under the versioned protocol rules above.
