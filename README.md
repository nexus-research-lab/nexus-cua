# Nexus Computer Use Runtime

Nexus Computer Use Runtime is a model-neutral, policy-first execution layer for
computer-using agents. It observes and controls exact native application
windows on macOS and Windows through a versioned protocol, replaceable platform
drivers, and host-issued capability sessions.

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
| `nexus-cua` | Service composition, diagnostics, schema export, and controlled maintainer requests |
| `sdk/go` | Official typed trusted-host client with same-request mutation reconciliation |
| `sdk/python` | Official typed Python 3.10–3.13 client and standalone quickstart |

The public protocol currently supports:

- running application and exact top-level window discovery;
- exact-window PNG capture with checked pixel-to-screen coordinate mapping;
- bounded interactive or diagnostic `full` accessibility traversal modes,
  either of which may report an explicit partial result;
- semantic focus, invoke, set-value, toggle, select, and expand operations;
- foreground focus, click, pointer movement, text, key, scroll, and drag input;
- fresh-observation authorization and deterministic post-action verification;
- explicit `not_dispatched` versus `indeterminate` mutation failures, bounded
  native-provider timeouts, and same-request reconciliation;
- read-only or explicitly bounded sessions with finite lifetimes; and
- authenticated local IPC plus an embeddable Rust runtime.

## Deliberate boundaries

This repository does not contain model inference, OCR, an agent loop, durable
conversation history, user-facing approval UI, browser DOM/CDP access, or Nexus
domain objects. OCR, when needed, belongs to the observing host or model layer.
The runtime also does not silently widen exact-window authority into
complete-desktop capture or background input.

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
      +----------- nexus computer CLI <----------+
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

Every public application, window, observation, element, session, and artifact
uses an opaque reference. A mutating command must present a live bounded
session and the fresh observation of the exact target window. Successful
mutations invalidate that observation, forcing the caller to observe again.

See [architecture](docs/architecture.md), the
[protocol specification](docs/specs/protocol-v1.md), and the
[runtime contract](docs/specs/runtime-contract-v1.md) for normative details.

## Integration paths

### Nexus

The intended Nexus integration is an independently released runtime package
managed by Nexus. Nexus downloads and stages an exact signed version, verifies
its package metadata, starts the private sidecar, and consumes it through a Go
host adapter. The sidecar never downloads or updates itself.

Nexus exposes a built-in Computer Use Skill and a round-scoped `nexus computer`
command. The command talks to Nexus's scoped broker rather than directly to the
sidecar, so the Agent Runtime never receives the transport token or generic
request command. Computer Use and Browser remain separate user settings and
separate authorities. This is intentionally a CLI-plus-Skill integration, not
an MCP integration inside Nexus.

The Nexus adapter is a downstream deliverable and is not implemented in this
repository. Its lifecycle and security contract are specified in
[product integration](docs/product-integration.md).

### Independent hosts

Rust products can embed `nexus-cua-runtime` and `nexus-cua-platform`. Go and
Python products can supervise the `nexus-cua` sidecar through the official
typed clients under `sdk/`. Committed schemas and the shared conformance corpus
remain available under `schemas/nexus.cua.v1/` and
`fixtures/compatibility/nexus.cua.v1/`.

Agent frameworks have three integration patterns above that host contract:

| Pattern | Role | Ownership |
| --- | --- | --- |
| Host SDK or private IPC | Canonical product integration | The trusted host owns policy, lifecycle, and authority |
| Scoped CLI plus Skill | Shell-capable agent integration | The host owns the command broker; the Skill contains instructions only |
| MCP adapter | Tool-protocol integration | A separately packaged adapter owns or receives explicit policy and keeps sidecar credentials private |

A Skill by itself is not a transport or an authorization boundary. CLI-plus-
Skill and MCP are sibling adapters, not alternate modes inside the runtime. The
first alpha distribution does not include generic agent adapters. They may be
shipped later as separate versioned packages above the official clients.

The diagnostic `nexus-cua request` command is intentionally not an agent API.
Production hosts should expose a narrower operation schema derived from their
own authenticated user and approval context.

Start with the [15-minute Python quickstart](docs/quickstart-python.md), the
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

On Windows, the equivalent quality gate does not require `make`:

```powershell
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.88.0 test --workspace --locked
cargo +1.88.0 run --locked --package nexus-cua -- doctor --compact
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
| `make sdk-check` | Format, vet/compile, and test the Go and Python clients |
| `make python-package-verify` | Build the Python wheel from local package metadata |
| `make release` | Optimized workspace build |
| `make native-fixture-macos` | Build the deterministic AppKit fixture |
| `make native-validate` | Run the strict real-driver fixture path |
| `make native-benchmark` | Measure release-runner p50/p95 distributions |
| `make native-soak` | Run diagnostic, idle, engineering, or release resource evidence |
| `make native-fault` | Prove provider timeout and mutation-disposition behavior |
| `make native-permission` | Run denied, revoked, or protected-target probes |
| `make native-evidence` | Aggregate raw hardware reports and enforce the release gate |

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
required Windows x64 release runner. Official Go and Python client engineering,
shared compatibility-fixture tests, local sidecar smoke, and package builds are
implemented; their full native acceptance remains dependent on the M2 hardware
gate. Release blockers include an accepted full
macOS fixture path under a stable permission identity, maintained Apple Silicon
and Windows x64 runner assignments, accepted topology/permission/
protected-target reports, benchmark baselines and eight-hour resource soaks,
and signed-package smoke tests on both operating systems. The Nexus adapter is
a downstream integration deliverable rather than a runtime release gate.
Aspirational work is not advertised as an active driver capability.

The prioritized delivery sequence is documented in the
[roadmap](docs/roadmap.md). Current machine-tested evidence, including the
Windows 11 ARM64 native smoke boundary, is recorded in
[platform validation](docs/platform-validation.md).

## Naming

- **Canonical project name:** Nexus Computer Use Runtime
- **Capability/UI name:** Computer Use
- **Repository and binary:** `nexus-cua`
- **Rust crate prefix:** `nexus-cua-*`
- **Wire identifier:** `nexus.cua.v1`
- **Domain shorthand:** CUA, meaning computer-using agent

The `cua` technical namespace identifies the domain served by the runtime; it
does not claim that this component contains an agent. These identifiers are not
renamed by documentation changes. If they change before or after the first
release, that change requires an explicit wire, package, and client migration.

## License

MIT. See [LICENSE](LICENSE), [third-party notices](THIRD_PARTY_NOTICES.md), the
[dependency policy](docs/dependency-policy.md), and [security policy](SECURITY.md).
