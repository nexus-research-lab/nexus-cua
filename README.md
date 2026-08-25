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
instructions, receipts, and chat identity stay in Nexus; generic adapters and
SDKs stay above this repository's versioned contract.

## What ships here

| Layer | Responsibility |
| --- | --- |
| `nexus-cua-protocol` | Closed wire types, opaque references, capabilities, actions, and stable errors |
| `nexus-cua-runtime` | Sessions, authorization, reference projection, stale-state guards, verification, and transient artifacts |
| `nexus-cua-platform` | Native macOS and Windows capture, accessibility, discovery, and input actors |
| `nexus-cua-transport` | Authenticated local-only Unix socket or Windows named-pipe transport with bounded idempotency |
| `nexus-cua` | Service composition, diagnostics, schema export, and controlled maintainer requests |

The public protocol currently supports:

- running application and exact top-level window discovery;
- exact-window PNG capture with checked pixel-to-screen coordinate mapping;
- bounded interactive or diagnostic `full` accessibility traversal modes,
  either of which may report an explicit partial result;
- semantic focus, invoke, set-value, toggle, select, and expand operations;
- foreground focus, click, pointer movement, text, key, scroll, and drag input;
- fresh-observation authorization and deterministic post-action verification;
- read-only or explicitly bounded sessions with finite lifetimes; and
- authenticated local IPC plus an embeddable Rust runtime.

## Deliberate boundaries

This repository does not contain model inference, OCR, an agent loop, durable
conversation history, user-facing approval UI, browser DOM/CDP access, or Nexus
domain objects. OCR, when needed, belongs to the observing host or model layer.
The runtime also does not silently widen exact-window authority into
complete-desktop capture or background input.

Those are architectural boundaries, not missing dependencies:

- **Nexus owns intent and consent.** It decides whether Computer Use is enabled,
  which round receives authority, and what the user sees.
- **The runtime owns execution safety.** It validates the host-issued session,
  target, observation, coordinates, action, deadline, and driver capability.
- **The platform driver owns native state.** AX objects, COM interfaces, capture
  queues, and input serialization remain on their required native actors.
- **Browser remains independent.** Visible browser windows may be controlled as
  native applications, but DOM, tabs, history, and network access require a
  separately enabled Browser capability.

## Architecture

```text
Nexus or another host
  policy · consent · agent loop · audit
                    |
          host adapter / SDK / CLI
                    |
       versioned Computer Use protocol
                    |
     capability-bounded local runtime
                    |
        replaceable platform driver
          /                     \
 macOS actors               Windows actors
 ScreenCaptureKit           Windows.Graphics.Capture
 AXUIElement                UI Automation
 CGEvent                    SendInput
```

Every public application, window, observation, element, session, and artifact
uses an opaque reference. A mutating command must present a live bounded
session and the fresh observation of the exact target window. Successful
mutations invalidate that observation, forcing the caller to observe again.

See [architecture](docs/architecture.md), the
[protocol specification](docs/specs/protocol-v1.md), and the
[runtime contract](docs/specs/runtime-contract-v1.md) for normative details.

## Integration paths

### Nexus

The intended Nexus integration is a supervised sidecar consumed by a Go host
service. Nexus exposes a built-in Computer Use Skill and a round-scoped
`nexus computer` command; it never exposes the transport token or generic
request command to the model. Computer Use and Browser remain separate user
settings and separate authorities.

The Nexus adapter is a downstream deliverable and is not implemented in this
repository. Its lifecycle and security contract are specified in
[product integration](docs/product-integration.md).

### Independent hosts

Rust products can embed `nexus-cua-runtime` and `nexus-cua-platform`. Other
languages can supervise the `nexus-cua` sidecar and export the local wire schema
today with `nexus-cua schema`. Committed schemas and compatibility fixtures are
available under `schemas/nexus.cua.v1/` and
`fixtures/compatibility/nexus.cua.v1/`; maintained reference clients remain
later distribution work. A third-party CLI, SDK, or MCP adapter may sit above
the contract, but must issue bounded sessions and preserve opaque references,
closed variants, sensitive-value handling, and retry identity.

The diagnostic `nexus-cua request` command is intentionally not an agent API.
Production hosts should expose a narrower operation schema derived from their
own authenticated user and approval context.

## Development quick start

Prerequisites are Rust 1.88 or newer and a macOS or Windows development host.
The supported OS and hardware matrix has not been declared yet. Native
observation and actions additionally require the operating-system permissions
reported by `doctor`.

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
| `make release` | Optimized workspace build |

The code MSRV remains Rust 1.88. `make package-verify` requires Cargo 1.90 or
newer because it packages the unpublished, interdependent workspace crates as
one graph.

## Status

The codebase is in active `0.1.x` development. The protocol, authorization
runtime, local IPC, and native macOS/Windows implementations are functional,
but the project is not release-complete.

Release blockers include maintained-hardware GUI fixtures, benchmark baselines,
an eight-hour resource soak, signed-package smoke tests on both operating
systems, and the downstream Nexus adapter. Aspirational work is not advertised
as an active driver capability.

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
