# Protocol Compatibility Policy

Status: normative for the `0.1.x` development line.

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

During the pre-release `0.1.x` line, a wire-shape change may update
`nexus.cua.v1` only in a reviewed milestone commit before a signed preview tag.
Once a preview build is tagged, an incompatible change requires a new protocol
identifier and parallel schemas/fixtures. Additive enum variants are treated
as incompatible for clients pinned to a closed contract.

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

## Retry compatibility

The canonical serialized command plus `request_id` identifies one execution.
Clients may lengthen only `timeout_ms` while reconciling. The service retains
completed responses for a configurable horizon of 10 minutes by default and
will not evict an unexpired result to admit new mutation work. After that
horizon an unresolved mutation is indeterminate; a client must not create a new
request ID and replay it. Restarting the runtime invalidates its in-memory
ledger and every runtime-local reference.
