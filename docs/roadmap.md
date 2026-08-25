# Delivery Roadmap

Nexus Computer Use Runtime is developed Nexus-first and ecosystem-neutral. The
same core must be dependable enough for Nexus product use and neutral enough
for another host to integrate without importing Nexus identities, policy, or
agent behavior.

This roadmap separates release requirements from optional breadth. A feature is
not advertised by `get_capabilities` until the selected native driver and its
test evidence support the complete contract.

The active implementation contract for the first independently adoptable
release is the [Standalone Developer Preview PRD](prd/standalone-developer-preview-v0.1.md).

## Current baseline: runtime alpha

The `0.1.x` codebase implements the basic execution chain:

- closed protocol and stable error categories;
- host-issued read-only and bounded sessions;
- opaque application, window, observation, element, and artifact references;
- exact-window screenshot and accessibility observations;
- semantic and foreground action paths on macOS and Windows;
- stale-observation rejection, action invalidation, and state verification;
- bounded authenticated local IPC with request idempotency; and
- transient screenshot storage with bounded retention.

This is sufficient for integration development. Implemented does not mean
release-validated: native behavior has not yet passed the full
maintained-hardware fixture, performance, soak, and package matrix required for
a supported product claim. The current developer-machine evidence and its exact
limits are tracked in [platform validation](platform-validation.md).

## Phase 1: Nexus-ready runtime

These items block the first supported Nexus integration:

1. Build deterministic GUI fixture applications for capture, semantics, text,
   pointer, scroll, drag, focus, stale-target, and permission behavior.
2. Run the declared macOS and Windows matrices on maintained hardware, including
   mixed-DPI, multiple-display, minimized-window, app-restart, and hung-provider
   cases.
3. Record release benchmark baselines and pass the absolute latency, memory,
   idle-CPU, and eight-hour resource-soak gates.
4. Produce pinned, signed packages with checksums, an SBOM, third-party notices,
   and clean-machine smoke evidence.
5. Implement the downstream Nexus supervisor, Go client, setting, approval
   binding, receipts, built-in Skill, and round-scoped `nexus computer` command.
6. Exercise disable, owner-switch, permission-revocation, crash-restart, timeout
   reconciliation, and sidecar-upgrade flows end to end with Nexus.

## Phase 2: ecosystem-ready distribution

After the Nexus path proves the core contract, improve independent adoption
without weakening it:

1. Keep the generated protocol schemas and versioned compatibility fixtures
   frozen and verified as the protocol evolves.
2. Provide small reference clients for Go, TypeScript, and Python that preserve
   closed variants, opaque references, sensitive values, and retry identity.
3. Publish sidecar lifecycle and packaging examples for non-Nexus hosts.
4. Add a separately packaged reference agent adapter, such as MCP, above the
   bounded host contract. It must not expose the private transport token or
   manufacture unrestricted authority.
5. Document compatibility policy, supported OS versions, native permission
   ownership, troubleshooting, and upgrade/rollback procedures.

The runtime remains useful without these adapters; adapters are consumers of
the protocol, not additional authority layers.

## Phase 3: deliberate capability expansion

Broader automation belongs behind new negotiated capabilities and explicit
authority units. Candidate work includes:

- application launch and termination with provenance-bound authority;
- window move, resize, and minimize/restore operations;
- clipboard operations with sensitive-data policy and audit semantics;
- complete-desktop or system-surface observation as a new scope, never an
  implicit fallback from exact-window authority;
- best-effort background pixel input only where public platform routes can
  preserve truthful targeting and delivery semantics; and
- Linux support after its native actor, capture, accessibility, permission, and
  input contracts have a maintained test matrix.

Each addition requires protocol design, capability reporting, bounded resource
behavior, platform evidence, and product authorization before implementation is
considered complete.

## Permanent non-goals

The repository will not own:

- model inference, provider selection, or in-runtime OCR;
- agent planning, memory, or task completion logic;
- Nexus user, chat, round, subscription, or preference objects;
- user-facing consent or approval UI;
- browser DOM, CDP, history, network, or background-tab access; or
- runtime download or self-update of executable driver code.

Keeping these concerns outside the runtime is what allows one safety contract to
serve Nexus and independent hosts.
