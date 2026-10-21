# Changelog

All notable changes to Nexus Computer Use Runtime will be documented in this
file.

## [Unreleased]

### Added

- Added the standalone `pelican-bike.html` SVG animation demo, with a coastal
  scene, an animated pelican cyclist, and pause, replay, and speed controls.
- Added the single fail-closed `scripts/run_codex_acceptance` entry point and
  `make codex-acceptance` target for macOS Apple Silicon. It binds the current
  commit, exact candidate and runner digests, pinned signing identity, fixed
  prompt, three machine-readable Codex attempts, MCP tool policy, and cleanup
  before writing an accepted summary.

### Changed

- The macOS release candidate now reads the same Developer ID and notarization
  secrets as Nexus (`MACOS_DEVELOPER_ID_CERTIFICATE_BASE64`,
  `MACOS_DEVELOPER_ID_CERTIFICATE_PASSWORD`, `MACOS_DEVELOPER_ID_APPLICATION`,
  `APPLE_NOTARY_APPLE_ID`, `APPLE_NOTARY_TEAM_ID`, `APPLE_NOTARY_PASSWORD`) and
  generates its temporary keychain password per run instead of requiring a
  secret.
- Added the manual `macOS signing preflight` workflow. It imports the Developer
  ID certificate, checks the configured identity and team, signs and verifies a
  throwaway binary, and authenticates notary credentials without publishing.
- Windows x64 native evidence and candidate signing jobs are disabled until a
  maintained Windows runner exists; set the repository variable
  `NEXUS_CUA_WINDOWS_ENABLED=true` to re-enable them.

- SDK window selectors now accept the opaque `window_ref` returned by
  `list_windows` before falling back to exact or unique title matches.

- MCP adapters now clear expired or reaped runtime sessions from their local
  active-session state, so status cannot report a dead session as active.

- The current execution snapshot is macOS Apple Silicon first. Windows x64
  implementation and platform evidence remain in the global M0–M6 contract
  and are assigned to a separate continuation.

- Registered adapters now revalidate their authenticated provider grant before
  native execution. The local provider checks the current MAC-authenticated
  registration, rejects removal or tampering, and renews the same bounded
  challenge near expiry without changing its request namespace or runtime/worker
  binding.
- CLI operation failures now return nonzero exit status while preserving the
  complete runtime/MCP response on stdout. Execution failures emit a stable,
  payload-free `nexus.cua.cli-error.v1` report on stderr; unknown outcomes never
  imply that a mutation was not dispatched. Service logs also use stderr.
- Added the host-bound CLI adapter service and Agent-facing
  `session/app/window/display/clipboard/action/verify` command families. They
  derive an internal capability only after exact host/profile authentication,
  accept no endpoint or token, use bounded stdin for generic mutation inputs,
  and expose authoritative current schemas through `session catalogue`.
  Service stop requests are instance-bound; registration loss, explicit stop,
  or grant TTL expiry shuts the service down, and authority revocation waits for
  its authenticated state to be cleaned up.
- Generated the single-interface CLI Skill beside the MCP Skill from the same
  shared operating contract and canonical catalogue, with binding-specific
  digests, packaging checks, and official Skill validation. The CLI Skill never
  contains human authority commands or an MCP fallback.
- Moved CLI and HTTP capability derivation behind the registered provider,
  including exact parent-host and adapter-profile checks, so the installed
  Agent command path cannot derive a cross-profile transport capability.
- Added the human-only `nexuscua authority plan/apply/status/revoke` lifecycle
  for the separate CLI registration profile. Plans are OS-secret authenticated
  and ten-minute bounded; plan/apply require real terminals; apply revalidates
  the exact host executable; status cross-checks receipt and registration; and
  revoke persists an authenticated intent before removing capability material,
  reporting `revoked` only after registration is unavailable.
- Serialized registration and authenticated integration-state mutation with
  owner-private cross-process file locks and added reclaimable hard limits of
  128 registrations and 512 plan/receipt/revocation records.
- Added the first-class `nexuscua version [--json]` command and made every HTTP
  MCP tuning argument explicitly HTTP-only; stdio rejects those options instead
  of silently ignoring them.
- Pinned every external GitHub Action to a lowercase 40-character commit SHA
  and added a zero-dependency workflow gate, with positive and negative tests,
  to prevent floating tags from returning.
- Made distribution staging reject evidence-by-presence. Accepted candidate
  manifests must bind the exact native, remote-transport, and Codex summaries;
  their contracts, revision/platform/runtime identity, evidence class, complete
  remote matrix, three Codex business passes, fault reconciliation, cleanup,
  and policy-violation state are validated before and after payload copying.
  Homebrew/WinGet generation now streams and bounds the candidate archive,
  rejects unsafe members without extracting them, requires an exact embedded
  manifest, and applies the same semantic gate to the archived payload.
- Added real MCP transport conformance across a connected rmcp stdio client and
  Streamable HTTP JSON/request-scoped SSE exchanges. The tests compare complete
  tool definitions, reject a legacy authenticated GET endpoint, inject
  post-admission timeout/cancellation/disconnection, reconcile the original
  mutation ID, and prove one native dispatch and effect per intent. HTTP now
  bounds accepted TCP connections, I/O inactivity, absolute connection life,
  and authenticated request duration; a real loopback slow-body test proves
  capacity reclamation and cancellation-driven shutdown.
- Added closed unbound, MCP, and downstream Nexus install profiles plus
  no-download npm meta/platform staging and exact-archive Homebrew/WinGet
  generators. Channel staging rejects legacy or digest-mismatched payloads,
  npm packages have no lifecycle scripts, and source checks execute real local
  `npm pack` operations without publishing.
- Classified the existing sign-after-test workflow as `unaccepted_legacy`,
  added an explicit engineering/release candidate selector, and bound its
  evidence requirement, manifest, archive names, and artifact names to that
  selector. It remains intentionally ineligible for M6 acceptance.
- Replaced the native evidence release boolean with the
  `nexus.cua.native-evidence.v2` engineering/release class contract. Summaries
  now bind every raw report, runner manifest, source revision, and tested
  runtime digest; enforcement and release-candidate validation reject class
  downgrade. The hardware workflow keeps release-only operator probes separate
  and requires idle plus engineering soaks before the additional release soak.
- Unified the source-build product entrypoint as `nexuscua`; MCP now starts via
  `nexuscua mcp serve`, reports the `nexuscua` handshake identity, and publishes
  only unprefixed tool leaves. The unpublished old binary/tool names have no
  compatibility aliases.
- Added the current stateless Streamable HTTP MCP transport beside stdio in the
  shared server core. It defaults to loopback with an OS-assigned port, requires
  Bearer authentication before MCP dispatch, rejects browser Origin requests,
  bounds bodies/in-flight calls, and exposes no legacy HTTP+SSE route.
- Added explicit stable `request_id` inputs to every MCP mutation, the read-only
  `reconcile_mutation` tool, and `nexuscua mutation reconcile`. Both adapter
  paths use the same bounded dispatcher ledger and return runtime epoch,
  admission, disposition, dispatch/effect evidence, and fresh-observation state
  without redispatching the original mutation.
- Added a canonical operation catalogue, shared Computer Use operating
  contract, and generated `nexuscua-computer-use` MCP and CLI Skills with
  version/binding digest metadata and CI drift checks.
- Added one Windows PowerShell source quality gate covering the same Rust,
  generated-contract, documentation, release-helper, native-harness, Go,
  Python, and TypeScript checks as `make check`; Windows CI now invokes it
  directly. Markdown validation is shared through a zero-dependency Python
  implementation.
- Added the model-neutral authenticated authority-provider challenge/grant
  contract. Grants are closed, short-lived, debug-redacted, and bound to the
  registration, fresh adapter nonce, runtime/worker epoch, principal/work
  scope, and mutation request namespace before tool publication. The generic
  unsigned authority-file flag was removed; source dogfood must opt into an
  explicitly named unsigned-development fixture route.
- Added the OS-backed registered authority provider and production
  `nexuscua mcp launch --registration` entrypoint. Registration files are
  owner-private and HMAC-authenticated, registration secrets live in macOS
  Keychain or Windows DPAPI-protected storage, every launch revalidates the
  exact parent-host executable, and challenge replay or registration tampering
  fails closed before MCP tool publication. Registered Streamable HTTP derives
  its Bearer capability from the protected secret instead of argv or config.
- Added a fail-closed Codex stdio integration assistant with exact-alias
  inspection, short-lived authenticated plans, real-terminal confirmation,
  whole-config stale checks, generated single-binding Skill installation,
  authenticated receipts, exact-owned removal, and rollback after uncertain
  host writes. Host output and plan payloads are bounded and zeroized, while
  Codex configuration receives only `nexuscua mcp launch --registration <id>`.
- Replaced tag-triggered GitHub release publication with a manual,
  artifact-only release-candidate workflow. Candidates now fail closed on
  same-revision macOS and Windows x64 native evidence, exact tested-runtime
  digests, distribution signatures, macOS notarization, SPDX SBOMs, manifests,
  clean-host signed-runtime smoke evidence, and archive checksums. Release
  workflow dependencies are pinned to immutable action revisions.
- Native hardware evidence now exercises a universal macOS runtime and retains
  only sanitized summaries, signing metadata, and exact tested executables;
  tokens, screenshots, accessibility content, and fixture logs are excluded
  from uploaded artifacts.

- Corrected the Nexus integration boundary: the Agent Runtime owns reasoning
  and the agent loop, Nexus owns its CLI/Skill broker and managed sidecar
  installation, and optional CLI/Skill or MCP adapters remain above the core
  host contract.
- Session manifests now select short-lived trusted-host discovery references;
  session creation revalidates runtime epoch, process generation, and native
  executable identity before granting authority.
- Session and discovery expiry now use a nearest-deadline scheduler, including
  idle artifact deletion and an explicit graceful runtime shutdown path.
- Request reconciliation now retains completed results for a configurable
  10-minute default horizon and rejects capacity instead of evicting an
  unexpired result.
- Errors now carry a required mutation disposition. Failures proven before
  native dispatch report `not_dispatched`; admitted native failures and caller
  deadlines report `indeterminate` until reconciled under the same request ID.
- Windows permission status now reports global desktop grants as not applicable
  and treats UIPI or secure-desktop input rejection as an explicit permission
  failure instead of success.
- macOS foreground keyboard dispatch now confirms application activation and
  routes complete modifier/key sequences to the bound process, preventing a
  focus race from leaking typed content to an unrelated application.
- Go and Python trusted-host helpers now resolve applications and windows by
  exact identity first and reject ambiguous substring matches instead of
  selecting platform helpers or hidden windows by enumeration order.
- Adopted Nexus Computer Use Runtime as the canonical project name while
  retaining `nexus-cua`, crate names, environment variables, and
  `nexus.cua.v1` as the technical domain namespace.
- Separated runtime session/reference projection from command orchestration and
  consolidated platform-neutral observation integrity helpers.
- Reframed the documentation around one Nexus-first core with independent
  ecosystem integration surfaces and an explicit delivery roadmap.

### Fixed

- The packaged `nexus-cua` CLI crate now embeds its own copy of the Skill
  catalogue, and `make package-verify` covers `nexus-cua-provider`, so the
  published crate graph builds outside the repository checkout.
- MCP screenshot responses no longer expose runtime-local artifact paths. The
  adapter now atomically verifies and consumes each PNG from disk, retains only
  a 32 MiB aggregate-bounded in-memory cache for normalized actions and zoom,
  and therefore leaves no screenshot bytes behind when an MCP client kills the
  stdio child without a graceful shutdown.
- macOS background single-left-click now first performs a bounded AX hit test
  and native press on an actionable control before falling back to the public
  process-routed event path. The route never uses private SkyLight APIs or
  silently activates the target, and canvas fallbacks remain explicitly
  unverified.
- ScreenCaptureKit callback failures now retain a bounded native error domain,
  code, and localized description, making platform capture failures
  diagnosable without logging screenshot content.
- Corrected the macOS exact-window RGBA row orientation so MCP image pixels and
  screenshot coordinates share the required top-left origin.
- Normalized the macOS lifetime physical-footprint sample conservatively when
  the kernel's reported peak lags the current sample, preventing a flaky
  release-evidence assertion without lowering the memory budget.

### Added

- Added a bounded two-process remote gateway conformance harness using
  temporary mutual TLS. It verifies certificate and gateway authentication,
  exact lease/epoch affinity, path-free checksum-bound artifacts,
  post-admission disconnect reconciliation, epoch replay rejection, and frame
  limits, then emits a credential-free `nexus.cua.remote-transport.v1`
  evidence summary bound to the tested executable and source revision.
- Added separately authorized bounded plain-text clipboard read and guarded
  replacement across the protocol, runtime, native macOS and Windows actors,
  MCP adapter, and Go, Python, and TypeScript SDKs. Writes require a fresh
  single-use opaque change guard, stale external changes fail before dispatch,
  and clipboard text is redacted and never retained by the runtime.
- Added a separately authorized read-only physical-display scope across the
  protocol, runtime, macOS ScreenCaptureKit, Windows WGC, MCP, Go, Python, and
  native harness. Display observations use opaque refs and bounded transient
  PNGs but are structurally ineligible as action guards. A real Codex MCP run
  enumerated the development host's three displays and captured the unique
  primary display without exposing a filesystem path or invoking an action.
- Added Windows x64 MSVC full-workspace cross-check and strict platform Clippy
  evidence. Native Windows hardware and Codex evidence remain separate gates.
- Added an official zero-runtime-dependency Node.js/TypeScript trusted-host
  client with complete typed protocol coverage, local Unix-socket/Windows-pipe
  framing, private token-file checks, closed error/evidence validation,
  display operations, sensitive-text redaction, and same-request launch/action
  reconciliation. It shares the 16-operation compatibility corpus with Go and
  Python and has strict declaration compile tests.
- Added a host-selected MCP `vision` tool profile for visual models with flat,
  closed schemas for normalized drag, text, key, scroll, semantic invoke, and
  text and numeric value replacement actions. The profile hides the complex generic action
  union, retains the same runtime authority boundary, and has schema-size,
  unknown-field, and union-complexity regression tests. Added current Kimi,
  GLM, and DeepSeek harness guidance without adding provider code to the core.
- Added observation-bound numeric range metadata and `set_range_value` across
  the protocol, runtime, MCP, Go, and Python surfaces. macOS uses settable AX
  numeric value attributes and Windows uses UI Automation RangeValuePattern;
  disabled, read-only, non-finite, and out-of-range requests fail before native
  dispatch.
- Added the optional `nexus-cua-host` crate with bounded least-loaded desktop
  worker placement, finite lease/epoch affinity, fail-fast capacity, an
  authenticated length-prefixed remote stream contract, same-worker request
  reconciliation, and path-free checksum-bound screenshot tickets. Added
  separate direct network/filesystem environment policy descriptors and an
  optional snapshot/reset/clone provider interface; the real local desktop
  implementation truthfully returns unsupported for machine lifecycle.
- Added immutable deny-by-default managed and user runtime policy layers at the
  common SDK/IPC/CLI/MCP dispatch boundary. Both layers must pass, hard denies
  override allows, bounded action-argument rules narrow sessions, mutation
  denial is `not_dispatched`, and status exposes only startup-snapshot hashes.
- Added host-authorized foreground restoration for temporary pointer and
  keyboard input. macOS and Windows bind the exact prior foreground process
  generation, restore it only while the guarded target still owns foreground,
  and report a closed restoration outcome without overriding a later user or
  system focus change. A runtime-wide fail-fast foreground lane now returns
  `busy/not_dispatched` under contention instead of queueing workers.
- Added closed action effect evidence. Successful mutations now distinguish
  `confirmed/native_readback` from `unverified/dispatch_acceptance`; the runtime
  rejects incoherent pairs, SDKs decode them as closed values, and partial or
  uncertain native execution remains an indeterminate error rather than a
  misleading success.

- Added bounded installed-application discovery and single-use,
  provenance-bound cold launch authority. The public protocol accepts only an
  opaque short-lived launch reference and activation ceiling, revalidates the
  native registration, rejects ambiguous already-running identities, and
  returns a fresh running-generation discovery reference. macOS implements the
  route through `NSWorkspace`; Windows uses a bounded dedicated COM actor and
  freshly revalidated local Start Menu executable shortcuts.
- Added dedicated MCP launch-discovery and launch tools. The adapter can start
  with no running authorized application, cold-launch only an exact identity
  fixed by the host policy, and rebuild its bounded session around the new
  process generation without exposing paths, arguments, PIDs, or sidecar
  credentials.

- Added an explicit `background`/`foreground` delivery preference for guarded
  pixel clicks. Background clicks do not require foreground authority, never
  activate the target, and never silently escalate: macOS posts to the exact
  process and Windows posts client-coordinate button messages to the exact
  window. Delivery is reported as `background`; callers must observe to verify
  application effect and may request a fresh foreground action only when the
  host separately authorized it.
- Added separately authorized `set_window_bounds` and
  `set_window_minimized` actions for top-level move/resize and
  minimize/restore. macOS uses bounded AX window attributes and Windows uses
  no-activate Win32 window APIs; both report a distinct `system` delivery route
  and invalidate guarded observations.
- Added separately authorized, observation-bound `terminate_application`.
  macOS requests cooperative `NSRunningApplication` exit and Windows posts
  `WM_CLOSE` only after process-generation revalidation; both use bounded
  confirmation, report the `system` route, invalidate matching observations
  across sessions, and never escalate to a forced kill.

- Added a bounded local `nexus-cua-mcp` stdio adapter for Codex and other MCP
  hosts. It embeds the runtime without exposing sidecar credentials, resolves a
  host-authored application/action ceiling at startup, returns verified PNG
  image blocks, and preserves fresh-observation and indeterminate-mutation
  semantics.
- Added a bounded top-left `0..1000` normalized click tool to the MCP adapter so
  visual-model coordinates remain stable when a client resizes HiDPI images.
- Added bounded MCP zoom views with opaque crop references and server-side
  coordinate back-mapping, allowing provider-neutral visual models to inspect
  small targets without learning runtime pixel-scale math.
- Added a dedicated `nexus_cua_terminate_application` MCP tool so lifecycle
  authority stays explicit and visual agents never provide native process IDs.

- Added selected, checked, and expanded accessibility facts on macOS AX and
  Windows UI Automation snapshots, plus a closed fresh `element_state`
  verification predicate in the Rust, Go, and Python contracts.
- Defined the execution-environment boundary for local user desktops, remote
  desktop workers, optional VM isolation, direct network/filesystem policy, and
  truthful snapshot/reset/clone capability reporting.
- Added a model-neutral `press_point` action backed by bounded, serialized
  `CGEvent`/`SendInput` mouse-down and mouse-up sequences. The matching native
  fixture changes state only after a real hold and is part of release evidence;
  no cross-request raw button-down authority is exposed.

- Official typed Go, Python, and TypeScript clients with private Unix-socket/Windows
  named-pipe transports, closed protocol values, opaque identifiers, token-file
  ownership, and same-request mutation reconciliation.
- Cross-language compatibility-fixture tests, opt-in live-sidecar tests, a
  15-minute Python fixture quickstart, Go embedding/supervision examples, and
  SDK CI/package verification.

- Committed `nexus.cua.v1` request/response schemas, exhaustive compatibility
  fixtures, schema-drift CI, Markdown link checks, and a compatibility policy.
- Transport-authenticated application discovery with runtime-local opaque refs,
  public macOS code-signing/Windows Authenticode provenance summaries, and
  stable `stale_discovery` recovery.
- Initial model-neutral Computer Use protocol and runtime architecture.
- Authenticated bounded local IPC, request idempotency, CLI diagnostics, and
  development Makefile workflows.
- Native actor, coordinate, observation coherence, and performance contracts
  for macOS and Windows.
- Native macOS `ScreenCaptureKit`/`AXUIElement`/`CGEvent` and Windows
  `Windows.Graphics.Capture`/UI Automation/`SendInput` drivers.
- Bounded accessibility traversal, exact window-generation identities,
  visual stale-state guards, and private transient PNG artifacts.
- Concurrent request joining and post-timeout reconciliation under stable
  idempotency keys.
- A single automatic observation-coherence retry, bounded PNG worker pool, and
  per-session screenshot retention limit.
- Minimum-Rust macOS and Windows CI gates.
- Manual definitions for signed, notarized, SBOM-bearing universal macOS and
  Windows x64 candidate packages with SHA-256 checksums; no publication step
  exists and no official package has been published.
- Publishable version-pinned Rust crates and an exact-window v1 schema without
  dormant desktop or background-input grants.
- Stable executable-path fallback identities for macOS applications without a
  bundle identifier.
- Bounded non-password accessibility values on macOS and Windows, with secure
  text fields redacted before they cross the native actor boundary.
- Structured request-wait and detached-execution latency events without command
  payloads or typed text.
- Bounded request identities, active sessions, and application-allowlist memory.
- Per-process artifact generations that are removed on graceful runtime
  teardown without deleting their host-owned root.
- Crash-recoverable artifact-generation leases that let a restarted sidecar
  remove only orphaned transient generations while preserving live runtimes.
- Deterministic native AppKit and WPF fixtures plus a standalone public-protocol
  harness covering real capture, semantics, input, staleness, lifecycle,
  occlusion, minimization, and window-generation replacement.
- Controlled hung-provider fixtures, stable `target_unresponsive` failures,
  denied/revoked permission probes, Windows protected-target validation, and a
  release evidence aggregator that binds raw reports to runner/source/runtime
  identity.
- Release-runner benchmark, idle/engineering/release soak, resource sampling,
  sidecar-restart probes, 1080p/4K fixture profiles, and a self-hosted native
  hardware evidence workflow.
- Product integration contract for Nexus CLI/Skill consumption and independent
  Computer Use/Browser enablement.
- Bounded IPC connections and in-flight requests, plus an owner-and-SYSTEM-only
  Windows named-pipe DACL.
- Native Unix-socket and Windows named-pipe end-to-end CI smoke coverage.
- Bounded transport tokens with one shared CLI token-file decoder.
- `make smoke` for the native local IPC end-to-end check.
- Fail-closed minimized-window capture and process-generation-keyed Windows WGC
  frame caching with a bounded unchanged-frame fallback.
- Current Node 24 GitHub Actions and grouped weekly dependency updates.
