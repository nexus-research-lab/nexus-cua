# Codex MCP dogfood

Status: source-build development dogfood. No package is published. This guide
records the current macOS path; it is not the cross-platform packaged-candidate
gate in the
[完整产品与开发 PRD](PRD.zh-CN.md)。

The current `nexuscua mcp serve` entrypoint implements explicit `stdio` and
Streamable HTTP (`http`) selection with identical tools and authority semantics.
Streamable HTTP defaults to JSON and may instead return request-scoped SSE with
`--http-response-mode request_scoped_sse`; SSE is not a separate transport and
the legacy HTTP+SSE wire is not supported. The automated conformance test uses
a real rmcp stdio client and real HTTP initialize/tools-list exchanges in both
response modes, then compares every published tool definition and rejects a
legacy authenticated GET route. It also gates a native mutation after admission,
drops the wait by stdio timeout/cancellation and HTTP deadline/disconnect in both
response modes, reconciles the original ID, and asserts one dispatch and one
effect. A separate real-loopback test proves accepted-connection capacity,
idle-client reclamation, a whole-request deadline for a partial authenticated
body, and shutdown that interrupts the underlying sockets. These are source
transport conformance tests, not packaged Codex acceptance evidence. This Codex
runbook continues to use stdio because Codex launches the local process directly.

This path proves that Codex discovers `nexuscua` as a real local MCP server and
uses MCP tools for `discover installed -> launch -> observe -> action/verify ->
terminate`. Calling the runtime or sidecar directly from a shell does not
satisfy this acceptance test. The maintained macOS run has passed this exact
sequence with machine-readable `mcp_tool_call` events, a `counter=0` to
`counter=1` transition from a fresh observation, confirmed cooperative
termination, and empty final application and window lists.

The host alias, handshake identity, and public command are all `nexuscua`.
Raw tools are unprefixed leaves, so the qualified observation tool is
`mcp__nexuscua__observe_window`. Because no package had been published, the old
prefixed names and separate MCP binary were deleted directly: there is no alias,
deprecation period, dual tool surface, or old-config migration.

## Trust boundary

`nexuscua mcp serve` embeds one runtime and never exposes a sidecar token. The
production library path accepts only a challenge-bound grant from an authenticated
authority provider. The grant binds a fresh adapter nonce, registration,
runtime/worker epoch, principal, work scope, mutation request namespace, authority
ceiling, and expiry before any desktop tool is published. If an authorized application
is running, the adapter resolves its exact stable identity to a fresh discovery
reference and opens one finite session. Otherwise it remains sessionless and
can expose only separately authorized installed discovery and cold launch until
the launched process generation passes normal session admission. Tool
parameters cannot add applications, action kinds, launch activation,
foreground input, or lifetime.

The command below deliberately opts into `--development-authority-file` and
`--allow-unsigned-development-authority`. A file inside a Codex-writable checkout
is suitable only for this deterministic local development test; it is not a
product security boundary and cannot satisfy packaged-candidate acceptance.
The removed generic `--authority-file` spelling is not accepted. The production
entrypoint is `nexuscua mcp launch --registration <non-secret-id>`; it uses a
MAC-authenticated owner-private registration, macOS Keychain or Windows
DPAPI-protected secret storage, and exact parent-host executable verification.
The source tree now includes a production-shaped Codex stdio integration
transaction: it authenticates a short-lived plan, requires `--confirm` on a
real terminal, installs the generated MCP Skill, stores provider secrets in the
OS-backed store, invokes only supported `codex mcp add/remove` entrypoints, and
refuses stale or changed state. Streamable HTTP host installation, standalone
authority/session commands, grant renewal, and complete revocation remain open
before packaged acceptance.

This unsigned-file route remains a development-only mechanics exception for
testing image/tool mechanics with a read-only model shell and the fixed fixture.
It proves neither authority bootstrap nor product security, contributes no
M0-M6 acceptance evidence, and must be removed when the integration lifecycle
and packaged acceptance harness land. The provider grant contract, OS-backed
secret store, registered launcher, host-identity verification, tamper rejection,
replay rejection, and stdio integration rollback are implemented and unit-tested.

## Authenticated stdio integration assistant

For a local source build, first inspect without changing Codex:

```bash
target/release/nexuscua integration inspect --host codex --json
```

An OS user may then create a ten-minute plan from an owner-private authority
ceiling and review the full JSON change list. Planning does not edit Codex or
install the Skill:

```bash
target/release/nexuscua integration plan \
  --host codex \
  --profile mcp \
  --authority-policy-file /absolute/owner-private/authority.json \
  --principal local-user \
  --work-scope fixture-acceptance
```

Only from a real terminal, apply the returned opaque plan reference:

```bash
target/release/nexuscua integration apply --plan <plan-ref> --confirm
```

The transaction rejects an occupied exact alias or Skill target, rechecks the
whole Codex MCP configuration digest and both executable identities, consumes
the plan before mutation, and rolls back its registration, Skill, and exact
owned alias on failure. It never writes the authority path, provider secret,
grant, or Bearer token to Codex. Remove uses the authenticated receipt and
refuses to delete changed or unrelated host state:

```bash
target/release/nexuscua integration remove \
  --registration <non-secret-registration-id> \
  --confirm
```

These commands intentionally target stdio only at this stage. They do not turn
the source-build run into packaged M6 acceptance and should not be run by an
Agent or unattended automation.

## Build and prepare the fixture

From the repository root on macOS:

```bash
make mcp-build
fixtures/native/macos/build.sh release
mkdir -p .cache/codex-mcp/artifacts
cp docs/examples/codex-mcp-authority.json .cache/codex-mcp/authority.json
chmod 600 .cache/codex-mcp/authority.json
```

For an already-running observation/action test, open the built fixture after
preparation. For cold-launch acceptance, keep it stopped and copy the exact
bundle into the current user's standard application directory:

```bash
mkdir -p "$HOME/Applications"
ditto "target/native-fixtures/macos/Nexus CUA Native Fixture.app" \
  "$HOME/Applications/Nexus CUA Native Fixture.app"
```

The example authority permits cold launch and activation of only the exact
fixture identity, one physical click inside its window, and cooperative
termination. It also permits read-only physical-display overview; that grant
does not authorize any display-scoped action. Clipboard read and guarded
replacement are separately enabled in the example; disable both flags when a
dogfood run does not need them. To prove background delivery,
set `allow_foreground_input` to
`false`, keep `click_point` authorized, and pass `delivery = "background"` to
the normalized click tool. For a semantic test, replace the click action with
`invoke_element`.

`restore_foreground_after_input` is an independent host ceiling and requires
`allow_foreground_input = true`. When enabled, temporary pointer/keyboard input
attempts to return the exact prior application generation without overriding a
user or system focus change. Inspect `foreground_restoration` on every action;
`restored` and `not_needed` are positive outcomes, while
`skipped_owner_changed`, `target_unavailable`, and `failed` are truthful
non-restoration outcomes. Explicit focus and lifecycle tools report
`not_requested`.

Confirm native permissions with `cargo run --package nexus-cua -- doctor`.
Permission status is diagnostic, not proof: acceptance still requires the live
observation and action calls to succeed.

## Configure Codex

Create a machine-local `.codex/config.toml` in the repository. Replace every
absolute path with the checkout path on that machine:

```toml
[mcp_servers.nexuscua]
command = "/absolute/path/to/nexus-cua/target/release/nexuscua"
args = [
  "mcp",
  "serve",
  "--transport",
  "stdio",
  "--development-authority-file",
  "/absolute/path/to/nexus-cua/.cache/codex-mcp/authority.json",
  "--allow-unsigned-development-authority",
  "--artifact-root",
  "/absolute/path/to/nexus-cua/.cache/codex-mcp/artifacts",
  "--max-observation-age-seconds",
  "120",
  "--tool-profile",
  "full",
]
cwd = "/absolute/path/to/nexus-cua"
enabled = true
required = false
startup_timeout_sec = 15
tool_timeout_sec = 15
enabled_tools = [
  "get_status",
  "list_apps",
  "list_launchable_apps",
  "launch_application",
  "list_windows",
  "list_displays",
  "observe_display",
  "observe_window",
  "read_clipboard",
  "write_clipboard",
  "zoom",
  "click_normalized",
  "click_view_normalized",
  "type_text",
  "press_keys",
  "scroll",
  "drag_normalized",
  "invoke_element",
  "set_element_value",
  "set_range_value",
  "reconcile_mutation",
  "terminate_application",
  "perform_action",
  "verify_state",
]
default_tools_approval_mode = "writes"
```

Codex acceptance uses `full` so it exercises the complete public action
surface. For Kimi, GLM, or another image-capable function-calling harness, use
`--tool-profile vision`; that profile hides the complex generic action union
and exposes flat bounded tools for normalized click/drag, zoom, type, keys,
scroll, invoke, and value replacement. See the visual-model requirements in the
[complete PRD](PRD.zh-CN.md).

Code-mode Codex versions may route an MCP call through their built-in
programmatic-tool orchestration layer. That is still a native MCP call: the
machine-readable event must be `mcp_tool_call` with `server = "nexuscua"` and
the intended tool name. An acceptance prompt may allow that orchestration
layer to discover and invoke the temporary
`tools.mcp__nexuscua__*` surface, but must forbid shell, filesystem,
browser, network, and non-`nexuscua` nested tools.

`codex exec` cannot answer an interactive `writes` approval in a fully
unattended run. Do not use the global dangerous bypass for acceptance. Instead,
keep the server's `enabled_tools` allowlist minimal and pre-approve only the two
fixture lifecycle tools in the temporary acceptance configuration:

```toml
[mcp_servers.nexuscua.tools.launch_application]
approval_mode = "approve"

[mcp_servers.nexuscua.tools.terminate_application]
approval_mode = "approve"
```

This outer Codex approval is independent of the host-owned nexus-cua authority
file. Both layers must admit the operation; neither can widen the other.

The local config is ignored by Git because it contains machine-specific launch
paths and activates desktop tools. Codex accepts project MCP configuration only
for trusted projects. Restart the Codex client after changing the file, then
check `/mcp` in the app or run `codex mcp list` from the repository.

The adapter's bounded 120-second observation age accommodates remote visual
model latency. The runtime still revalidates the exact native target and visual
fingerprint immediately before dispatch; the setting is not permission to act
on changed pixels.

## Acceptance sequence

The sequence below is the current single-action source-build regression. Before
packaged-candidate acceptance, this guide must add Windows x64 preparation and
the PRD's two separately guarded mutations: normalized counter increment and a
freshly observed semantic `Enable Feature` toggle. The PRD scenario is
normative when the two documents differ.

Use only the unprefixed raw MCP tools under the registered `nexuscua` alias.
This section is a source-build runbook, not packaged-candidate evidence:

1. Read status and require the intended platform permissions and authority. If
   status contains `authority_provider.request_namespace`, prefix every mutation
   `request_id` with that exact value followed by `:`. For
   cold-start acceptance, require `session.active = false` first.
   For display-overview acceptance, require `capture_modes` to include
   `display`, `allow_display_capture = true`, and an active session. List
   displays, require exactly one primary, observe it once, and require a real
   image plus path-free structured content. Do not treat that observation as an
   action guard; list and observe the exact window before any mutation.
2. If no session is active, list launchable applications, require exactly the
   authorized stable fixture identity, and launch it once. Do not supply or
   infer a path, arguments, PID, or native registration handle. Preallocate a
   stable `request_id`, pass it to the mutation, retain it, and require
   `dispatched = true`; reconcile an indeterminate result under the same request
   identity with `reconcile_mutation` rather than launching again.
3. List windows and select the one exact visible fixture title; ignore hidden or
   untitled helper surfaces.
4. Observe it with a screenshot. Use `accessibility = "disabled"` for the
   provider-neutral visual path or `interactive` when live AX/UIA is available.
5. Use the returned fresh `observation_id` for exactly one authorized action.
   Preallocate and retain a new stable `request_id` for that exact mutation.
   Visual models should call `click_normalized`, whose top-left x/y
   coordinates run from 0 through 1000 and remain stable when the MCP client
   resizes a Retina/HiDPI image before inference.
   Pixel clicks default to `delivery = "foreground"`. A background test must
   request `delivery = "background"`, require the target to remain non-
   foreground before and after dispatch, and verify application state in a
   fresh observation. The adapter never silently escalates; a foreground retry
   must be a separate explicit action under foreground authority.
   When the target is small, call `zoom` with a region in the same
   full-image coordinate space. It returns a cropped image plus an opaque
   `view_ref`; call `click_view_normalized` so the server maps crop
   coordinates back through the original guarded screenshot. The model never
   has to calculate pixel-scale or crop offsets itself.
6. Observe again because successful mutation invalidates the old observation.
7. Confirm the fixture counter changed exactly once, either visually in the new
   image or with a supported deterministic state predicate.
8. Observe without a screenshot, allocate a stable termination `request_id`,
   cooperatively terminate the exact generation, and require final application
   and window listings to be empty.

Clipboard acceptance is independent of the window sequence: read once, retain
the opaque `clipboard_ref`, write a bounded non-secret fixture string with that
same ref, then read again and verify the new value. Restore the original value
with the newest ref before ending the run. A stale or failed write requires a
new read; never retry with a consumed ref, and never use real user clipboard
content as test output or model-visible evidence.

Successful action results carry a closed effect/evidence pair. Treat
`confirmed/native_readback` as confirmation only of the requested direct OS
state. Treat `unverified/dispatch_acceptance` as delivery evidence only and
always establish the application-visible effect from a fresh observation or
verification. Partial or uncertain execution is an indeterminate error, never
a reason to submit a new equivalent action.

Only one foreground action is admitted per desktop session at a time. A second
Codex/agent worker receives retryable `busy/not_dispatched`; it must obtain a
fresh observation before a later attempt and must not assume it was queued.

For lifecycle acceptance, observe without a screenshot when image capture is
irrelevant, call `terminate_application` with that fresh guard, then
list applications again and require the exact process generation to be absent.
The tool requests native cooperative exit (`terminate` on macOS, `WM_CLOSE` on
Windows), never accepts a caller-supplied PID, and does not escalate to a forced
kill. A timeout is indeterminate and must be verified rather than retried.

The run fails if MCP is unavailable, Codex uses shell execution as a substitute,
the image is missing or fails its runtime length/SHA-256 check, a stale
observation is accepted, the action exceeds host authority, or post-action state
cannot be established. Never retry an indeterminate mutation under a new
request identity.

Stop the fixture after the test. Remove the temporary installed fixture copy if
the cold-launch path created one. The adapter verifies and consumes the
runtime's disk PNG before returning each MCP image block, removes the local
path from structured content, and retains at most 32 MiB of image bytes in
memory for guarded normalized actions and zoom. This keeps screenshot bytes
off disk even if Codex kills the stdio child instead of disconnecting
gracefully. A normal disconnect additionally closes the session and clears the
in-memory cache.
