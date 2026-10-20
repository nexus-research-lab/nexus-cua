# Nexus Computer Use Runtime Go client

This module is the official typed Go client for a separately installed,
version-pinned Nexus Computer Use Runtime sidecar. It does not download or
start executable code and it does not expose the sidecar transport token.

Create a client from an explicit local endpoint and host-private token file,
then use trusted discovery to build a bounded session. The
[`examples/observe`](examples/observe/main.go) program demonstrates the complete
read-only path without raw protocol JSON or platform process identifiers.
`SelectApplication` and `SelectWindow` prefer exact identities (including the
opaque `window_ref` returned by `ListWindows`) and reject ambiguous substring
matches instead of trusting platform enumeration order.

Mutation calls return an `ActionRequest`. If the wait becomes indeterminate,
retain that value and call `ReconcileAction` with a longer timeout. Never create
a second mutation request to recover from an indeterminate result.

Clipboard access is separately authorized. `ReadClipboard` returns redacted
sensitive text plus a fresh opaque guard. Pass that guard and a `SensitiveText`
value to `WriteClipboard`; retain its `ClipboardRequest` for same-request
reconciliation, and read again after any stale or consumed guard.
