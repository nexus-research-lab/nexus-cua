# Runtime dispatch policy

Nexus Computer Use Runtime can compose an administrator-managed policy and a
user policy at the common runtime dispatch boundary. SDK, private IPC, CLI, and
MCP requests all reach this same check before session state or native actors.
Adapters may narrow authority further, but cannot bypass or widen either
runtime layer.

Policy files are closed JSON snapshots. The runtime reads them once during
construction, rejects missing, symlinked, oversized, writable-by-other Unix
files, unknown fields, duplicate entries, and invalid argument constraints,
then retains only the parsed layer and its SHA-256 digest. There is no reload
endpoint. Restart the runtime to change policy.

Each configured layer is deny-by-default:

1. A matching `deny.operations` or `deny.actions` entry rejects immediately.
2. The operation must appear in `allow.operations`.
3. `perform_action` must also be granted by `allow.actions` or by an
   `allow.action_rules` entry whose argument limits match.
4. `open_session` may not advertise an action kind absent from the layer.
5. Both managed and user layers must pass, so the user layer can narrow but
   never widen the managed ceiling.

Action rules can bound sensitive-text byte length, key count, click count and
delivery, pointer/drag duration, and absolute scroll delta. Policy denials do
not include typed text and are `not_dispatched` for mutations.

Start the private sidecar with either or both immutable layers:

```bash
nexuscua serve --dev-root .cache/nexus-cua \
  --managed-policy-file /managed/nexus-cua-policy.json \
  --policy-file /user/nexus-cua-policy.json
```

The current source-build MCP process accepts the same two policy flags plus the
explicit pair `--development-authority-file` and
`--allow-unsigned-development-authority`. This is only the documented
M3-incomplete development fixture; the removed generic authority-file flag is
not accepted, file permissions do not make it a production trust root, and this
route cannot produce acceptance evidence.

The MCP library now exposes the model-neutral authenticated provider contract
defined by the [Chinese PRD](PRD.zh-CN.md). A closed grant must match the fresh
adapter nonce, registration, runtime/worker epoch, principal, work scope,
mutation request namespace, and short expiry. Mismatches and replay fail before
tool publication, remaining session TTL is narrowed to the grant, and every
mutation must use the issued request namespace. The OS-backed registered
provider client, protected Keychain/DPAPI storage, renewal, and revocation
lifecycle remain M3/M4 work. The model and workspace must never receive the
provider secret, raw grant, sidecar token, or authority path. See the bounded
[example policy](examples/runtime-policy.json).

This policy controls CUA calls. It does not pretend to filter an application's
own network traffic or direct host filesystem tools. Those require a separately
negotiated desktop-worker environment policy and, when enforcement below the
application layer is required, an isolated environment provider.
