# Maintained Native Runners

Native release evidence is accepted only on two repository-pinned maintained
workers:

- `macos-apple-silicon.json` — the slowest maintained Apple Silicon worker in
  the preview support matrix; and
- `windows-x64.json` — the slowest maintained Windows 11 x64 worker in the
  preview support matrix.

The macOS worker is assigned in `macos-apple-silicon.json`; its first accepted
baseline is still pending. `windows-x64.example.json` documents the required
shape, while the active `windows-x64.json` remains intentionally absent until
an operator records that physical worker's real metadata. Do not infer a
manifest from a hosted runner or the Windows ARM64 Parallels engineering VM.
The native harness
rejects a manifest whose `status` is not `active`, whose platform differs from
the selected driver, or whose architecture differs from the executable running
the evidence job.

Each JSON manifest must contain non-null values for:

```text
runner_id
status
platform
architecture
cpu
memory_bytes
gpu
display_topology
power_mode
os_build
toolchain
```

Use `platform: "macos"` with `architecture: "aarch64"`, or
`platform: "windows"` with `architecture: "x86_64"`. `display_topology` and
`toolchain` should be JSON objects detailed enough to reproduce the run.
Changing a pinned runner requires an old/new comparison in the platform
evidence report; it must not reset or weaken an absolute budget.

The macOS worker must also expose `NEXUS_CUA_TEST_SIGNING_IDENTITY` as a real,
non-ad-hoc keychain signing identity. The workflow assigns the fixed identifier
`io.nexus.cua.hardware-sidecar`, records its designated requirement, and fails
if the identity is absent. The operator must pre-authorize that stable identity
for Accessibility, Input Monitoring, and Screen Recording. This test identity
does not authorize publication and is not a substitute for the distribution
signature and notarization gates.

An accepted run emits `summary.json` under the platform evidence directory with
contract `nexus.cua.native-evidence.v2`. The summary binds every input report,
the maintained-runner manifest, exact source revision, and tested runtime by
SHA-256. `engineering_accepted` requires the full automated matrix, absolute
benchmark gates, a five-minute idle soak, and a one-hour 4K engineering soak.
`release_accepted` additionally requires a separate eight-hour 4K release soak
and the platform's operator-controlled permission or protected-target probes.
The requested class is recorded and enforcement rejects class downgrade.

The workflow retains only the accepted summary, reviewable signing metadata,
and the exact tested runtime. Tokens, screenshots, accessibility content,
fixture logs, and transient artifacts remain runner-local and are deleted with
the job workspace. The candidate workflow described in the
[complete PRD](../../docs/PRD.zh-CN.md) consumes the tested runtime instead of
rebuilding it elsewhere.

The first accepted `summary.json` for each maintained runner must be reviewed
and committed under `benchmarks/baselines/`; until then, the native release
gate has no pinned baseline and remains incomplete. Later runner or baseline replacement follows
the comparison rule in the PRD and must preserve all absolute budgets.

The macOS release job intentionally resets the stable identity's TCC decisions
after collecting the granted-path evidence. The operator must re-approve that
identity before the next run. The Windows protected-target job is interactive:
the operator approves the fixture's UAC prompt while the sidecar remains at the
normal user integrity level. These are release-runner procedures, not developer
machine shortcuts.
