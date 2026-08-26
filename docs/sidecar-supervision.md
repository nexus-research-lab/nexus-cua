# Sidecar supervision

Status: integration contract and example for trusted hosts. This is not an
agent-facing command surface.

A host installs and verifies an exact `nexus-cua` package before starting it.
The runtime never downloads, installs, or updates executable code. The host
then owns one private sidecar lifecycle:

1. Create an owner-private state directory.
2. Generate a fresh high-entropy transport token and store it in an
   owner-private file.
3. Select a private Unix socket or local Windows named-pipe endpoint and a
   private artifact root.
4. Start the pinned binary with explicit paths.
5. Wait for `get_capabilities`, verify `nexus.cua.v1`, and inspect permissions.
6. Admit only host-reviewed discovery/session/action requests.
7. Stop new admission before shutdown, reconcile admitted mutations, close
   sessions, and signal the sidecar.
8. After the old process is gone, start a new process with a fresh token and
   fresh sessions. Never carry a request ledger or opaque refs across epochs.

The runnable Go [supervision example](../sdk/go/examples/supervise/main.go)
implements the development form of this lifecycle. It accepts the path to an
already installed binary; it never resolves or downloads a version. Production
Windows hosts should use their service or job-object control path and explicitly
apply an owner-private ACL to the state directory. Production Unix hosts should
retain mode `0700` for the directory and `0600` for the token file.

## Authority boundary

The official clients read the token file internally and expose no token getter.
That prevents normal typed calls from accidentally returning credentials, but
it does not turn an untrusted process into a policy host. Any process that can
read the token file is trusted to request discovery and bounded sessions.

Do not pass the endpoint, token-file path, sidecar process handle, generic
request command, or artifact root to an agent. Nexus should expose its own
round-scoped `nexus computer` broker and Skill. An independent product should
expose an equivalently narrow tool whose host code selects the application,
session manifest, and allowed actions.

## Cancellation and reconciliation

Stopping a client wait does not prove that an admitted native mutation was
cancelled. The Go client returns the same `ActionRequest` when its context ends;
the Python client attaches it to `MutationIndeterminateError`. The host may
only extend the wait and reconcile that exact request ID during the configured
horizon.

If the sidecar exits, old session and observation refs are invalid. A host must
not replay the old mutation into the new process. It may report an indeterminate
result, obtain fresh state, and ask its product policy what to do next.
