# Changelog

All notable changes to Nexus Computer Use Runtime will be documented in this
file.

## [Unreleased]

### Changed

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

### Added

- Official typed Go and Python clients with private Unix-socket/Windows
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
- CI definitions for attested universal macOS and Windows x64 release packages
  with SHA-256 checksums; no official package has been published yet.
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
