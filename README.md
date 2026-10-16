# Nexus Computer Use Runtime

Nexus Computer Use Runtime is a model-neutral, policy-first execution layer for
computer-using agents. It observes and controls exact native application
windows on macOS and Windows through a versioned protocol, replaceable platform
drivers, and host-issued capability sessions.

The single product and development source of truth is the Chinese
[complete PRD](docs/PRD.zh-CN.md). Low-level English documents remain only for
protocol, runtime, platform-evidence, and operational details.

The repository identifier is `nexus-cua`. In this project, **Computer Use**
names the capability, while **CUA** names the broader computer-using-agent
domain that consumes it. The runtime is not itself an agent and does not contain
a model or planning loop.

## Why this exists

Nexus needs dependable native desktop control whose session is bounded to
reviewed applications and actions and whose mutations are each bound to a fresh
observation of one exact window. Building that core as an independent runtime
also gives other products a small, model-neutral integration surface instead of
coupling desktop automation to Nexus internals.

The project has one delivery priority and one permanent design constraint:

1. **Delivery priority — Nexus-ready:** predictable authorization, lifecycle,
   performance, and failure behavior for the built-in Nexus Computer Use
   feature.
2. **Design constraint — ecosystem-neutral:** a documented Rust API and
   language-neutral sidecar protocol that other hosts can integrate without
   inheriting Nexus product semantics.

Both paths use the same runtime. Nexus-specific policy, approval UI, agent
instructions, receipts, and chat identity stay in Nexus. Model inference and
the agent loop stay in the connected Agent Runtime. SDKs and agent adapters sit
above this repository's versioned host contract.

## What ships here

| Layer | Responsibility |
| --- | --- |
| `nexus-cua-protocol` | Closed wire types, opaque references, capabilities, actions, and stable errors |
| `nexus-cua-runtime` | Sessions, authorization, reference projection, stale-state guards, verification, and transient artifacts |
| `nexus-cua-platform` | Native macOS and Windows capture, accessibility, discovery, and input actors |
| `nexus-cua-transport` | Authenticated local-only Unix socket or Windows named-pipe transport with bounded idempotency |
| `nexus-cua` (Cargo package) | Public `nexuscua` entrypoint, service composition, diagnostics, schema export, controlled requests, and MCP startup |
| `nexus-cua-mcp` (internal Cargo library) | Shared host-authorized MCP server core for stdio and Streamable HTTP |
| `nexus-cua-provider` (internal Cargo library) | Keychain/DPAPI-backed host registration, exact launcher identity, and challenge-bound grants |
| `skills/common` + `skills/mcp` + `skills/cli` | Canonical shared operating contract and generated single-interface MCP/CLI Skill bindings |
| `sdk/go` | Official typed trusted-host client with same-request mutation reconciliation |
| `sdk/python` | Official typed Python 3.10–3.13 client and standalone quickstart |
| `sdk/typescript` | Official typed Node.js client with zero runtime dependencies and same-request mutation reconciliation |

The managed MCP registration alias and handshake identity are both
`nexuscua`. Public MCP tools use unprefixed leaves such as `observe_window`, so
a host exposes `mcp__nexuscua__observe_window`. The unpublished prefixed raw
tools and separate MCP binary were deleted without aliases, deprecation
wrappers, or dual registration. Nexus's built-in Computer Use path uses its
round-scoped `nexus computer` broker and does not mount this MCP server.

The MCP transport is configurable without changing tools or authority:
`stdio` is the local default and `http` means current Streamable HTTP. HTTP
defaults to JSON responses and can select the request-scoped SSE representation
with `--http-response-mode request_scoped_sse`; this is not a separate
transport, and the legacy HTTP+SSE wire is not supported. CI connects a real
rmcp client over stdio and sends real initialize/tools-list requests through
both HTTP representations, comparing the complete published tool schemas. It
also injects post-admission client timeouts and cancelled/disconnected waits
across stdio and both HTTP representations, then reconciles the same mutation
ID and proves a single native dispatch/effect. HTTP binds to loopback and an
OS-assigned port by default; bare installation never starts a listener. HTTP
requests require a private Bearer capability before MCP body dispatch and
browser `Origin` requests are rejected by default. Accepted connections and
authenticated requests default to 64 each; I/O idle, absolute connection, and
whole-request deadlines bound slow clients, while shutdown cancellation wakes
the underlying sockets instead of waiting for those deadlines. A real loopback
test covers capacity occupation/reclamation, a partial authenticated body, and
prompt shutdown.

The MCP and CLI Skills are generated from one bounded catalogue and the shared
operating contract with `make skill-write`; `make skill-check` rejects
tool/catalogue or generated-content drift. Install profiles select exactly one
binding. The CLI binding obtains exact current argument schemas from the running
service with `nexuscua session catalogue`; it never probes MCP or receives the
private service capability.

The MCP library has a closed authenticated authority-provider contract binding
each grant to a fresh adapter nonce, registration, runtime/worker epoch,
principal/work scope, request-ID namespace, and short expiry. The source
dogfood command accepts an unsigned JSON fixture only with the explicit
`--development-authority-file --allow-unsigned-development-authority` pair;
that route is never release evidence. Production launch uses only
`nexuscua mcp launch --registration <non-secret-id>`: a MAC-authenticated,
owner-private registration binds the exact parent-host executable, while its
secret is stored in macOS Keychain or Windows DPAPI-protected state. The
Codex stdio integration assistant now implements bounded `inspect`, a
ten-minute authenticated `plan`, real-TTY-only `apply`, exact-state `remove`,
and rollback through the supported `codex mcp` commands. Apply installs exactly
one generated MCP Skill and writes only a non-secret registration ID into the
host command. The human-only `authority plan/apply/status/revoke` lifecycle now
creates the separate CLI registration profile: planning and application require
a real terminal, apply rechecks the exact host executable, status cross-checks
authenticated receipt and registration state, and revocation records an
authenticated intent before removing the capability material. Agent-facing
service/session/app/window/display/clipboard/action/verify commands now use an
authenticated, loopback-only internal service without accepting an endpoint or
token. The service stops when its registration disappears, a bound stop request
arrives, or its grant TTL expires, and CLI revocation waits for service cleanup.
Trusted-host automatic service startup, grant renewal, Streamable HTTP host
installation, and complete active-process revocation across every adapter remain
under development.
Registration and authenticated integration state are serialized with
owner-private cross-process locks and capped at 128 and 512 records
respectively; removal reclaims capacity.

The public protocol currently supports:

- running application and exact top-level window discovery;
- bounded installed-application discovery, provenance-bound cold launch on
  negotiated drivers, and cooperative observation-bound termination;
- exact-window PNG capture with checked pixel-to-screen coordinate mapping;
- separately authorized, read-only physical-display overview with opaque
  display refs; display observations can never authorize actions;
- separately authorized, bounded plain-text clipboard reads and guarded
  compare-and-replace writes with redacted values and opaque change refs;
- bounded interactive or diagnostic `full` accessibility traversal modes,
  either of which may report an explicit partial result;
- explicit selected, checked, and expanded element facts plus fresh semantic
  state predicates;
- semantic focus, invoke, set-value, toggle, select, and expand operations;
- system-routed top-level window move/resize and minimize/restore operations;
- foreground focus, click, bounded pointer hold, pointer movement, text, key,
  scroll, and drag input, plus caller-selected best-effort background pixel
  clicks that never activate or silently fall back to the foreground;
- fresh-observation authorization and deterministic post-action verification;
- explicit `not_dispatched` versus `indeterminate` mutation failures, bounded
  native-provider timeouts, explicit adapter-facing mutation IDs, and read-only
  same-request reconciliation with runtime-epoch evidence;
- closed action effect evidence distinguishing bounded native read-back from
  dispatch-only results that still require fresh observation;
- read-only or explicitly bounded sessions with finite lifetimes; and
- authenticated local IPC plus an embeddable Rust runtime.

## Deliberate boundaries

This repository does not contain model inference, OCR, an agent loop, durable
conversation history, user-facing approval UI, browser DOM/CDP access, or Nexus
domain objects. OCR, when needed, belongs to the observing host or model layer.
The runtime also does not silently widen exact-window authority into display
capture, display-scoped actions, or unrestricted background input. A host may
grant read-only display overview independently so an agent can inspect the real
user desktop before selecting an exact window.

Those are architectural boundaries, not missing dependencies:

- **Nexus owns the product control plane.** It decides whether Computer Use is
  enabled, which round receives authority, what the user sees, which runtime
  package is installed, and what enters product audit.
- **The Agent Runtime owns reasoning.** It invokes models, runs the agent loop,
  selects tools, and follows the Computer Use Skill supplied by Nexus.
- **The runtime owns execution safety.** It validates the host-issued session,
  target, observation, coordinates, action, deadline, and driver capability.
- **The platform driver owns native state.** AX objects, COM interfaces, capture
  queues, and input serialization remain on their required native actors.
- **Browser remains independent.** Visible browser windows may be controlled as
  native applications, but DOM, tabs, history, and network access require a
  separately enabled Browser capability.

## Architecture

```text
Nexus Product <----> Agent SDK Bridge <----> Agent Runtime
control plane                              model + agent loop
      ^                                           |
      | round-scoped broker              Computer Use Skill
      +------------- nexuscua CLI <--------------+
      |
package manager + CUA host adapter
      |
version-pinned nexus-cua sidecar / private IPC
      |
capability-bounded runtime
      |
macOS actors or Windows actors
```

The Agent SDK Bridge connects Nexus to the Agent Runtime, but Computer Use does
not put reasoning into `nexus-cua`. In the Nexus path, the Skill instructs the
Agent Runtime to call the round-scoped Nexus CLI. That command returns through
Nexus's product boundary; only the trusted Nexus host adapter can reach the
private sidecar.

Every public application, window, display, observation, element, session, and
artifact uses an opaque reference. A window mutation must present a live bounded
session and the fresh observation of the exact target window; a clipboard write
must present the fresh single-use guard returned by a separately authorized
clipboard read. Successful window mutations invalidate their observation.
Foreground mutations use one fail-fast lane per desktop instead of an
unbounded queue. A host may separately authorize restoration of the exact prior
foreground application after temporary input; restoration never overrides a
later user or system focus change.
Physical-display observations are a disjoint read-only scope and are never
accepted as action guards.

See [architecture](docs/architecture.md), the
[protocol specification](docs/specs/protocol-v1.md), and the
[runtime contract](docs/specs/runtime-contract-v1.md) for normative details.
Optional immutable managed/user narrowing is described in the
[runtime policy guide](docs/runtime-policy.md).
The distinction between the user's real desktop, remote desktop workers, and
optional VM-backed isolation is defined in
[execution environments](docs/specs/execution-environments.md).

The optional `nexus-cua-host` crate supplies bounded multi-desktop
registration/leases, worker epoch affinity, authenticated remote framing, and
path-free checksum-bound screenshot transfer. It does not provision VMs or
open a network listener; trusted products provide the TLS/SSH/RPC channel and
register only environments whose policy/lifecycle capabilities are real.
The `nexus-cua-remote-harness` executable validates that contract across an
actual controller and desktop-worker process over temporary mutual TLS. Its
sanitized `nexus.cua.remote-transport.v1` summary binds both processes to one
binary digest and records resource bounds without retaining credentials,
screenshots, or worker-local paths.

## Integration paths

### Nexus

Nexus will install one exact signed runtime through its UI or `nexuscfg`, select
the CLI-bound Skill, and expose the target public `nexuscua` command through a
Nexus-owned scoped broker. The command never receives private sidecar authority.
This downstream work starts only after this repository passes its standalone
and Codex acceptance gates; no Nexus repository implementation is claimed here.

### Independent hosts

Independent Rust hosts may embed the runtime. Go, Python, and TypeScript hosts
may supervise the private sidecar through the official SDKs. MCP hosts use the
MCP-bound Skill and local stdio adapter; shell-capable trusted hosts may use the
CLI-bound Skill. Both adapters project the same canonical actions and must
receive authenticated host authority. The diagnostic `nexuscua request`
surface is never an Agent API.

Start with the [Codex MCP dogfood guide](docs/codex-mcp.md), the
[complete product PRD](docs/PRD.zh-CN.md), the
[15-minute Python quickstart](docs/quickstart-python.md), the
[Go embedding example](sdk/go/examples/observe/main.go), and the
[sidecar supervision contract](docs/sidecar-supervision.md). These source
workflows are implemented and tested, but no official alpha package has been
published yet.

## Development quick start

Prerequisites are Rust 1.88 or newer and a macOS or Windows development host.
The preview validation matrix is declared, but the repository has not yet
earned a supported release claim on that matrix. Native observation and
actions additionally require the operating-system permissions reported by
`doctor`.

```bash
make check
make doctor
make schema
```

Windows does not require GNU Make. Use the single PowerShell source quality gate;
it covers the same Rust, schema, generated Skill, documentation, release-helper,
native-harness, Go, Python, and TypeScript checks as `make check`. It is not a
substitute for Windows 11 maintained-hardware native acceptance:

```powershell
./scripts/check-windows.ps1
```

Start an isolated development service with private state below `.cache/dev`:

```bash
make dev
```

Useful targets:

| Command | Purpose |
| --- | --- |
| `make check` | Formatting, strict Clippy, unit, contract, and IPC tests |
| `make package-verify` | Build and compile the unpublished multi-crate package graph |
| `make doctor` | Active driver capabilities and current OS permission state |
| `make smoke` | Authenticated native local-IPC smoke test |
| `make schema` | Closed request and response JSON schemas |
| `make schema-check` | Fail when committed schemas drift from Rust wire types |
| `make docs` | Validate local Markdown paths and heading anchors |
| `make sdk-check` | Format, type-check/compile, and test the Go, Python, and TypeScript clients |
| `make python-package-verify` | Build the Python wheel from local package metadata |
| `make release` | Optimized workspace build |
| `make mcp-build` | Optimized local MCP adapter build |
| `cargo run --bin nexuscua -- integration inspect --json` | Read-only exact Codex alias, Skill, and optional registration status |
| `make native-fixture-macos` | Build the deterministic AppKit fixture |
| `make native-validate` | Run the strict real-driver fixture path |
| `make native-benchmark` | Measure release-runner p50/p95 distributions |
| `make native-soak` | Run diagnostic, idle, engineering, or release resource evidence |
| `make native-fault` | Prove provider timeout and mutation-disposition behavior |
| `make native-permission` | Run denied, revoked, or protected-target probes |
| `make native-evidence` | Aggregate raw hardware reports and enforce the release gate |
| `make remote-validate REMOTE_SOURCE_REVISION=<git-sha>` | Run two-process mTLS conformance and write sanitized evidence |
| `make workflow-check` | Reject external GitHub Actions not pinned to exact commits |

The current manual no-publish candidate workflow is a sealed legacy engineering
workflow: it has no tag or publication trigger, but its native-first/sign-after
ordering is not acceptable M6 evidence. It must not be used to claim an accepted
candidate. The target pipeline builds and signs immutable payloads first, runs
native and Codex acceptance against those exact digests, and only then assembles
the SBOM, manifest, provenance, and archive. The complete CI/CD contract is in
the [PRD](docs/PRD.zh-CN.md).

The code MSRV remains Rust 1.88. `make package-verify` requires Cargo 1.90 or
newer because it packages the unpublished, interdependent workspace crates as
one graph.

## Status

The codebase is in active `0.1.x` development. The protocol, authorization
runtime, local IPC, and native macOS/Windows implementations are functional,
but the project is not release-complete.

The deterministic GUI fixtures, controlled provider faults, permission and
protected-target probes, benchmark/soak harnesses, and machine-readable evidence
gate now exist. Real Windows 11 ARM64 engineering validation covers the complete
read/mutation path and both provider-timeout boundaries, but it is not the
required Windows x64 release runner. Official Go, Python, and TypeScript client engineering,
shared compatibility-fixture tests, local sidecar smoke, and package builds are
implemented; their full native acceptance remains dependent on the maintained-hardware
gate. Current completion blockers include maintained Apple Silicon and Windows
x64 evidence, trusted-host CLI service installation/startup, registered HTTP host
integration, authority renewal and complete cross-adapter revocation, engineering
performance evidence, accepted signed-package channel smoke, and
two-platform Codex MCP acceptance. The eight-hour resource soak is deferred to
the later publication gate. Nexus integration remains a downstream deliverable.
Aspirational work is not advertised as an active driver capability.

The prioritized delivery sequence is documented in the
[complete PRD](docs/PRD.zh-CN.md). Current machine-tested evidence is recorded in
[platform validation](docs/platform-validation.md).

## Naming

- **Canonical project name:** Nexus Computer Use Runtime
- **Capability/UI name:** Computer Use
- **Repository identifier:** `nexus-cua`
- **Installed public command:** `nexuscua`
- **Managed MCP alias and handshake identity:** `nexuscua`
- **MCP tool form:** unprefixed leaf, exposed as `mcp__nexuscua__<leaf>`
- **Rust crate prefix:** `nexus-cua-*`
- **Wire identifier:** `nexus.cua.v1`
- **Domain shorthand:** CUA, meaning computer-using agent

The `cua` technical namespace identifies the domain served by the runtime; it
does not claim that this component contains an agent. The public binary and MCP
tool naming migration is complete in source and has no compatibility aliases.
Any later change requires an explicit package and client migration.

The inert sources under [packaging](packaging/README.md)
define the checked npm meta/platform split and exact-digest Homebrew/WinGet
generators. They do not publish, download a runtime, register a host, start a
service, or grant desktop authority. Channel staging accepts only an assembled
`engineering_candidate` or `release_candidate`, and the manifest must bind
semantically accepted native, two-process remote, and 3/3 real-Codex summaries.
Homebrew and WinGet generation also inspects the bounded archive in place,
rejects unsafe members, and requires the archived manifest, runtime, profiles,
and evidence to match the accepted candidate. Empty evidence placeholders and
the current legacy sign-after-test workflow are deliberately rejected.

## License

MIT. See [LICENSE](LICENSE), [third-party notices](THIRD_PARTY_NOTICES.md), the
[dependency policy](docs/dependency-policy.md), and [security policy](SECURITY.md).
