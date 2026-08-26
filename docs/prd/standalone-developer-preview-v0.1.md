# Nexus Computer Use Runtime — Standalone Developer Preview PRD

Status: approved implementation target for the `0.1.x` development line.

Implementation progress: M0 and M1 are complete. M2 engineering is implemented;
its exit gate remains blocked on accepted evidence from the maintained macOS
Apple Silicon and Windows x64 runners. M3 client engineering is implemented;
its full live native acceptance remains dependent on M2. M4 remains pending.
The milestone sections retain their imperative wording because they are the
reviewed acceptance contract.

Audience: Nexus Computer Use Runtime maintainers and Codex implementation agents.

## 1. Objective

Deliver the first independently adoptable Nexus Computer Use Runtime developer
preview without coupling the runtime to Nexus product identities or to any model
provider.

At the end of this phase, a developer on a clean supported macOS or Windows
machine must be able to:

1. install a versioned runtime package;
2. verify native permissions and capabilities;
3. start the private local sidecar;
4. discover a running application without already knowing its platform
   identifier;
5. open a bounded session selected from that discovery result;
6. observe an exact window and perform one authorized action through an official
   client; and
7. understand the security boundary, compatibility policy, and current native
   limitations from the public documentation.

The target release is `v0.1.0-alpha.1`. It is a transparent developer preview,
not a production-support claim.

## 2. Product position

Nexus Computer Use Runtime is a local execution runtime for computer-using
agents. It is model-neutral, policy-first, and limited to explicitly authorized
native desktop surfaces.

The runtime is not an agent. It does not own model inference, planning, memory,
OCR, end-user approval UI, or product identity. An Agent Runtime owns model
inference, planning, and memory. A trusted product host owns policy, approval,
runtime lifecycle, and product identity, and consumes either the Rust API or
the private sidecar protocol.

The same core serves two consumers:

- Nexus uses an official Go client beneath a round-scoped CLI and built-in
  Skill.
- Independent hosts use the published Rust crates, official reference clients,
  or a separately packaged policy-preserving adapter.

## 3. Success metrics

The preview is complete only when all of the following are true:

- A new developer can complete the reference quickstart in at most 15 minutes,
  excluding operating-system permission approval and package download time.
- The quickstart does not require hand-written protocol JSON, private token
  access by a model, or application identifiers copied from source code.
- Every public wire schema is committed, versioned, and checked for accidental
  drift in CI.
- Official Go and Python clients pass the same cross-language compatibility
  fixtures against the real sidecar transport.
- Deterministic native fixture tests cover discovery, screenshot capture,
  semantic observation, one semantic mutation, one foreground mutation,
  stale-observation rejection, and session cleanup on both supported operating
  systems.
- Release packages include checksums, provenance, an SBOM, third-party notices,
  and a clean-machine smoke record.
- The public release page, README, support matrix, troubleshooting guide, and
  limitations all describe the same implemented capability set.

## 4. Scope

### 4.1 Protocol and lifecycle correctness

#### Trusted-host application discovery

At the M0 baseline, the session API required an application allowlist before the
caller could list the applications needed to construct that allowlist. M1 adds
a distinct transport-authenticated, read-only discovery operation for trusted
hosts.

Requirements:

- Discovery does not require an existing capability session.
- It returns only public application descriptors: display name, stable
  application identifier, foreground state, provenance summary, and a random
  opaque discovery reference.
- It does not return PID, HWND, AX object, process handle, COM object, or another
  native authority value.
- It grants no observation or mutation authority.
- `OpenSession` remains the only operation that creates desktop authority.
- `OpenSession` authority is created from unexpired discovery references, not
  from caller-authored application identifier strings. Stable application
  identifiers remain catalog/display facts and are not authority by themselves.
- A discovery reference binds the runtime process epoch, discovered process
  generation, executable/bundle identity, and a maximum 30-second discovery
  lifetime. It is single-runtime and cannot survive sidecar restart.
- `OpenSession` re-resolves the selected application and requires the same
  process generation and application identity. Restart, executable replacement,
  bundle/path identity change, expiry, or sidecar restart returns a stable
  `stale_discovery`-class error and requires rediscovery.
- A running process replacement never inherits an old session. The host must
  rediscover it and obtain new user/policy approval when required by the host.
- On macOS, the descriptor reports bundle identifier, executable path, and
  available signing team/designated identity without making a missing signature
  an ambient denial. On Windows, it reports normalized executable path and
  available publisher/signature identity. Raw native handles remain private.
- A host can select an exact discovery result and open a bounded session without
  implementing its own platform discovery code.

Because no public release exists yet, the implementation may revise the `v1`
wire shape rather than carry a compatibility shim for the current development
shape. The resulting schema must be internally consistent and fully tested.

#### Deadline-driven session expiry

Session expiry and screenshot deletion must not depend on a later request.

Requirements:

- The runtime schedules cleanup at the nearest session deadline.
- The scheduler performs no active idle polling.
- Expired sessions reject new work and delete their transient artifacts even
  when the service otherwise remains idle.
- Opening, closing, and expiring sessions must safely reschedule the next
  deadline.
- Embedded and sidecar consumption paths receive explicit lifecycle and
  shutdown semantics.
- Tests use controlled time and do not rely on long sleeps.

#### Version and schema discipline

- Commit request and response schemas under `schemas/nexus.cua.v1/`.
- Add canonical JSON compatibility fixtures for every command, result, and
  stable public error.
- CI regenerates schemas and fails on an uncommitted difference.
- Document the `0.1.x` compatibility policy and the conditions requiring a new
  protocol identifier.
- Define a readable protocol-mismatch response path. Capability discovery must
  not pretend to negotiate versions it cannot decode.

The canonical fixture inventory for this release includes capabilities,
permissions, trusted-host discovery, open/close session, list windows, observe,
perform action, verify state, every success result, and every stable public
error. Fixtures also assert unknown-field rejection, canonical tagged-variant
serialization, sensitive-value redaction, maximum frame handling, and the
protocol-mismatch envelope.

#### Request reconciliation contract

- Duplicate in-flight requests with the same ID and command join one execution.
- Reusing an ID with different command bytes fails closed.
- Completed results remain reconcilable for a documented default horizon of at
  least ten minutes. The bound is configurable by the embedding host.
- The runtime must reject new mutating admissions rather than evict an
  unexpired mutation result needed for reconciliation.
- SDKs may extend only the wait deadline while reconciling the same request.
- After the reconciliation horizon, an SDK reports the mutation as
  indeterminate and must not submit it again under a new ID.
- A sidecar process epoch change invalidates reconciliation. SDKs must never
  replay an old mutation after restart; they may observe fresh state instead.

### 4.2 Official client surfaces

#### Go client

Create `sdk/go` as the first supported non-Rust client. It is also the future
client for the Nexus host adapter; the native runtime remains an independently
installed, version-pinned sidecar package.

It must provide:

- native Unix-socket and Windows named-pipe transport;
- closed typed requests, responses, errors, and identifiers;
- request ID generation and same-ID timeout reconciliation;
- capability and permission inspection;
- trusted-host application discovery;
- bounded session lifecycle helpers;
- list-window, observe, perform-action, verify, and close operations;
- context cancellation that stops waiting without falsely claiming an admitted
  native mutation was cancelled; and
- tests driven by the committed compatibility fixtures and a live sidecar.

The client must not expose a convenience API that creates unrestricted or
ambient desktop authority.

#### Python client

Create `sdk/python` for the public agent-development ecosystem.

It must expose the same protocol semantics as the Go client, including closed
variants, opaque identifiers, redacted sensitive values, bounded sessions, and
same-request reconciliation. It must support the public quickstart without
requiring users to construct raw envelopes.

TypeScript is deferred to the next ecosystem milestone unless it can be added
without delaying the preview gates.

#### Diagnostic CLI

Keep `nexus-cua request` as a maintainer diagnostic. Add only narrow human and
fixture-oriented commands needed for installation verification. Do not turn the
diagnostic command into an unrestricted agent-facing API.

### 4.3 Deterministic native validation

#### Preview support matrix

Required release-validation hosts are:

| Platform | Architecture | Preview status |
| --- | --- | --- |
| macOS 14 or newer | Apple Silicon | Required supported preview path |
| Windows 11 23H2 or newer | x64 | Required supported preview path |
| macOS 14 or newer | Intel | Universal package build; experimental until maintained hardware passes the same matrix |
| Windows 11 24H2 or newer | ARM64 | Package and evidence when maintained hardware is available; otherwise explicitly experimental |

GitHub compilation on macOS and Windows Server is not a substitute for the
interactive macOS and Windows 11 validation hosts above. Each release evidence
record names the exact OS build, hardware or VM boundary, display topology, and
runtime package digest.

Build one small deterministic fixture application for macOS and one for
Windows. The fixtures may be platform-native, but must expose the same logical
controls and test identities:

- named top-level window;
- button with observable invocation result;
- editable text field;
- checkbox or toggle;
- selectable list item;
- expandable control where supported;
- scrollable region;
- draggable target;
- explicit state text for verification; and
- a controlled window-replacement or geometry-change action.

The native validation harness must cover:

- discovery and exact-window selection;
- screenshot dimensions and pixel-to-screen mapping;
- interactive and full semantic snapshots, including partial metadata;
- secure text redaction;
- semantic focus, invoke, set-value, toggle, select, and expand where the
  platform fixture exposes the matching pattern;
- foreground focus, click, move, type, keys, scroll, and drag;
- stale observation after mutation, geometry change, and process/window
  replacement;
- minimized, occluded, multiple-display, mixed-DPI, and negative-coordinate
  behavior;
- permission denied and permission revoked behavior;
- sidecar restart, expired session, artifact deletion, and same-request timeout
  reconciliation; and
- elevated or protected targets returning truthful failure rather than a false
  success.

Expected edge behavior is closed:

| Scenario | Required result |
| --- | --- |
| Minimized window | Discovery reports `minimized=true`; observation either uses a platform route proven to represent the exact window or returns a stable unsupported/target-unavailable error without restoring it implicitly |
| Occluded window | Exact-window capture contains the target surface rather than the occluding application, or returns a truthful unsupported error |
| Mixed DPI / negative coordinates | Screenshot mapping reaches the intended logical point within one physical output pixel and never crosses the guarded window |
| Permission denial or revocation | The next affected operation returns `permission_required`; no mutation is reported as dispatched |
| Process/window replacement | Existing discovery, window, and observation references fail stale/target-unavailable and never retarget |
| Elevated/protected Windows target | Access failure is explicit; UIPI or secure-desktop denial is never reported as success |
| Hung accessibility provider during observation or action preflight | Return stable `target_unresponsive`/deadline failure with `mutation_dispatched=false`; observation never becomes indeterminate |
| Hung accessibility provider after mutation dispatch admission | Return an indeterminate mutation result; the supervisor may terminate the sidecar and must never replay the mutation |

Hosted CI continues to compile and test portable/native code. Maintained real
hardware jobs are the authority for GUI behavior and must publish an evidence
summary for each release candidate.

### 4.4 Performance and resource evidence

Implement repeatable benchmark and soak harnesses for the budgets already
defined in the [runtime contract](../specs/runtime-contract-v1.md#performance-budgets).

The preview requires:

- warm p50/p95 distributions for discovery, window capture, semantic snapshot,
  combined observation, semantic action, foreground action, and IPC overhead;
- peak and steady-state memory measurements for 1080p and 4K capture;
- handle, file, COM, IOSurface/GPU resource counts where available;
- five-minute idle CPU evidence;
- a one-hour engineering soak for rapid regression feedback; and
- an eight-hour release soak with no monotonic resource growth before the alpha
  tag.

Measurements must record hardware and OS version. Developer anecdotes do not
replace release evidence.

Every operation must meet the absolute p95, idle CPU, capacity, and memory
thresholds in the linked contract. A release evidence report includes the raw
sample count and p50/p95 values; merely executing the benchmark is not a pass.

M2 freezes two named release-runner manifests under `benchmarks/runners/`: the
slowest maintained Apple Silicon host and the slowest maintained Windows x64
host in the required support matrix. Each manifest records CPU, memory, GPU,
display topology, power mode, OS build, and toolchain. The first accepted M2
evidence becomes the pinned baseline. Replacing a runner requires a documented
old/new comparison and cannot weaken an absolute budget or silently reset a
regression. Faster ad hoc machines may diagnose performance but cannot supply
release-gate evidence.

### 4.5 Distribution and supply chain

Produce versioned artifacts for:

- macOS universal (`aarch64` and `x86_64`);
- Windows x64; and
- Windows ARM64 when the maintained validation host and toolchain are available.

Release requirements:

- pinned source revision and locked dependencies;
- package version exactly matches the Git tag;
- SHA-256 checksums;
- GitHub build provenance attestation;
- CycloneDX or SPDX SBOM;
- complete third-party notices generated from the resolved graph;
- macOS code signing and notarization pipeline;
- Windows Authenticode signing pipeline;
- clean-machine `doctor`, sidecar start, discovery, observation, and shutdown
  smoke tests; and
- `cargo publish --dry-run` for every crate in dependency order.

Distribution contracts:

- GitHub assets are
  `nexus-cua-v0.1.0-alpha.1-macos-universal.tar.gz`,
  `nexus-cua-v0.1.0-alpha.1-windows-x86_64.zip`, and, when validated,
  `nexus-cua-v0.1.0-alpha.1-windows-aarch64.zip`.
- Archives contain the binary, README, license, third-party notices, SBOM,
  package manifest, and package-specific checksum metadata.
- Rust crates publish in dependency order: protocol, runtime, platform,
  transport, then CLI.
- The Go module path is
  `github.com/nexus-research-lab/nexus-cua/sdk/go`; Go releases use the required
  `sdk/go/v0.1.0-alpha.1` submodule tag on the same source commit.
- The Python distribution is `nexus-cua`, the import package is `nexus_cua`,
  and supported interpreters are Python 3.10 through 3.13. The Nexus GitHub
  organization owner must reserve the PyPI project before M3 publishes an
  artifact. If that exact distribution name is unavailable, M3 stops for an
  explicit product naming decision; the implementer must not invent a fallback
  public identity.
- SDKs accept an explicit sidecar path or endpoint configuration. They never
  download or self-update executable code at runtime.
- A trusted host package manager may download official release assets, but must
  verify the platform signature, checksum, provenance, package manifest, and
  compatibility before activation. This installer remains outside the runtime.
- Upgrade and rollback use pinned side-by-side packages; sessions, request
  ledgers, tokens, and artifact generations never cross a sidecar process epoch.

Actual signing, notarization, registry publication, and release creation require
organization credentials. The implementation must prepare and validate the
pipeline, then report the exact credential blocker instead of weakening the
release gate. Official GitHub prerelease assets must not be published unsigned.
Missing credentials may complete M0-M3 engineering, but M4 and the phase remain
blocked until an organization owner performs the documented signing step.

### 4.6 Public adoption experience

Add:

- an installation guide for source builds and release packages;
- a 15-minute Python quickstart;
- a Go embedding example;
- a sidecar supervision example;
- a supported OS/architecture matrix;
- macOS and Windows permission troubleshooting;
- upgrade, rollback, and crash-recovery guidance;
- a security-model page explaining that the transport token belongs only to a
  trusted host;
- `CONTRIBUTING.md`, issue templates, and a minimal code of conduct;
- repository topics and homepage/documentation metadata; and
- one reproducible demo capture showing observation, semantic selection, an
  authorized action, and post-action verification against the fixture app.

The README must lead with install, verified demo, and integration choices. It
must label the release as alpha and avoid production-support language.

### 4.7 Optional agent adapter boundary

The runtime has one canonical host contract: the embeddable Rust API or private
sidecar protocol, with official clients preserving the same semantics.
Agent-facing CLI-plus-Skill and MCP integrations are sibling adapters above
that contract, not alternate modes inside the runtime. A Skill alone is only
instructions and cannot provide transport, consent, policy, or authority.

Generic agent adapters are explicitly deferred until after
`v0.1.0-alpha.1`; they are not part of M0-M4. The downstream Nexus integration
does not add scope here: Nexus continues to own its round-scoped CLI and Skill.
For ecosystem adoption, later milestones may ship two separate versioned
packages:

- a scoped agent CLI with runtime-specific Skill bundles; and
- an MCP adapter for MCP-capable agent runtimes.

When added, either adapter must sit above an official client or an equivalent
conformant host implementation. It must:

- keep the transport token private;
- own or receive an explicit application/action policy;
- never expose raw `OpenSession` manifest construction to the model;
- preserve observation freshness and request reconciliation;
- report the runtime's exact capability set; and
- remain outside the core runtime crates; and
- avoid changing the Nexus product integration, which continues to use the Go
  client plus its own round-scoped CLI and Skill.

## 5. Explicit non-goals for `v0.1.0-alpha.1`

- Model inference, provider routing, OCR, agent planning, or memory.
- Nexus user, chat, round, receipt, or preference objects.
- Browser DOM, CDP, network, tab, history, or background-page access.
- Linux support.
- Complete-desktop capture or implicit system-surface authority.
- Application launch/termination, clipboard, and window management unless a
  separately reviewed protocol change is completed without delaying the
  preview.
- A consumer-facing approval UI.
- Claims of production readiness or universal application compatibility.

## 6. Delivery milestones

### M0 — Land the current baseline

- The expected starting commit is `b7320a94c11439ea38045df9ee7159c09eb59c96`.
  If `HEAD`, upstream, or the working-tree ownership differs, stop and report
  the drift before committing.
- The expected handoff tree is exactly 28 modified tracked files plus five
  untracked paths. The prior evaluation task owns every path in the inventory
  below except `docs/prd/standalone-developer-preview-v0.1.md`, which is the
  handoff contract added by the parent Nexus task. `docs/roadmap.md` is prior
  evaluation work with the handoff PRD link added by the parent task.
- Review the existing uncommitted naming, documentation, and cohesion refactor
  as one baseline. Do not assume any path outside the inventory belongs to this
  phase. If status, count, or ownership differs, stop before staging or
  committing and report the exact drift.
- Preserve all validated behavior and existing user-owned changes.
- Run `make check`, schema drift checks, package verification, and Markdown link
  validation.
- Record the pre-change commit in the handoff so the baseline remains
  recoverable through ordinary Git history.
- Commit and push only the reviewed coherent baseline directly to `main`; do not
  create a PR. Never reset, discard, or overwrite an unexpected working-tree
  change.

Expected modified tracked paths:

```text
AGENTS.md
CHANGELOG.md
Makefile
README.md
SECURITY.md
THIRD_PARTY_NOTICES.md
crates/cli/Cargo.toml
crates/cli/src/commands.rs
crates/cli/src/main.rs
crates/platform/Cargo.toml
crates/platform/src/lib.rs
crates/platform/src/macos/mod.rs
crates/platform/src/windows/mod.rs
crates/protocol/Cargo.toml
crates/protocol/src/capability.rs
crates/protocol/src/lib.rs
crates/runtime/Cargo.toml
crates/runtime/src/lib.rs
crates/runtime/src/service.rs
crates/transport/Cargo.toml
crates/transport/src/error.rs
crates/transport/src/lib.rs
crates/transport/src/server.rs
docs/architecture.md
docs/dependency-policy.md
docs/product-integration.md
docs/specs/protocol-v1.md
docs/specs/runtime-contract-v1.md
```

Expected untracked paths:

```text
crates/platform/src/observation.rs
crates/runtime/src/session.rs
docs/platform-validation.md
docs/prd/standalone-developer-preview-v0.1.md
docs/roadmap.md
```

### M1 — Correct the public contract

- Implement trusted-host discovery.
- Implement deadline-driven expiry and artifact cleanup.
- Commit schemas and compatibility fixtures.
- Add protocol and lifecycle tests.
- Update normative specifications and changelog in the same change.

Exit gate: no bootstrap discovery dead end, no request-driven-only expiry, and
all existing plus new contract tests pass on macOS and Windows CI.

### M2 — Prove native behavior

- Add deterministic macOS and Windows fixture applications.
- Add the native validation harness and hardware workflow.
- Add benchmark and soak harnesses.
- Publish the first platform evidence report.

Exit gate: both operating systems complete the required read and mutation path
against deterministic fixtures with truthful permission and stale-target
behavior.

### M3 — Make third-party integration practical

- Implement the Go client and live-sidecar tests.
- Implement the Python client and quickstart.
- Add embedding and supervision examples.
- Validate that neither client leaks transport authority to an agent-facing
  surface.

Exit gate: a fresh developer can finish the documented quickstart without raw
JSON or platform-specific discovery code.

M2 fixture work and M3 client scaffolding may proceed in parallel after M1's
wire schema is frozen. M3 live-sidecar acceptance remains blocked on M2's exact
native behavior and evidence.

### M4 — Publish the developer preview

- Complete release, signing, SBOM, checksum, provenance, and clean-machine
  workflows.
- Finish support, security, troubleshooting, compatibility, and contribution
  documentation.
- Record the final alpha evidence and known limitations.
- Tag and publish `v0.1.0-alpha.1` as a GitHub prerelease after all available
  credential-dependent gates pass. “Available” refers to enabled target
  architectures, not optional signing: every published official asset must be
  signed under the applicable platform policy.

Exit gate: release assets are reproducible, installable, verifiable, and honest
about support boundaries.

## 7. Engineering constraints

- Keep protocol, runtime, platform, transport, SDK, and adapter dependencies
  directed; provider and Nexus product semantics must not enter the core.
- Preserve bounded queues, traversals, sessions, artifacts, retries, and native
  actor ownership.
- Do not log screenshots, accessibility values, typed text, tokens, command
  payloads, native handles, or platform object identities.
- Every protocol or capability change updates the normative specification,
  schema fixture, tests, README capability statement, and changelog together.
- Prefer cohesive implementation modules over thin forwarding layers.
- No release claim may exceed maintained hardware evidence.
- Work directly on `main` and push only after the relevant gate passes; do not
  create a pull request unless the user explicitly changes this workflow.

## 8. Definition of done

The phase is done only when all M0-M4 exit gates are satisfied. If an external
credential dependency is the only remaining blocker, report `M0-M3 complete,
M4 blocked` with an exact operator action; do not call the phase complete and do
not publish unsigned official assets. A passing portable unit test suite alone
is not completion.

The final handoff must include:

- exact commits and released version;
- macOS and Windows evidence summaries;
- benchmark and soak results;
- package checksums and SBOM location;
- Go and Python quickstart verification;
- remaining known limitations; and
- any credential-dependent release step still requiring an organization owner.
