# Maintained Native Runners

M2 release evidence is accepted only on two repository-pinned maintained
workers:

- `macos-apple-silicon.json` — the slowest maintained Apple Silicon worker in
  the preview support matrix; and
- `windows-x64.json` — the slowest maintained Windows 11 x64 worker in the
  preview support matrix.

The macOS worker is assigned in `macos-apple-silicon.json`; its first accepted
baseline is still pending. `windows-x64.json` remains intentionally absent
until an operator assigns that physical worker. Do not infer a manifest from a
hosted runner or the Windows ARM64 Parallels engineering VM. The native harness
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
does not authorize publication and is not a substitute for the M4 distribution
signature and notarization gates.

An accepted run emits `summary.json` under the platform evidence directory with
contract `nexus.cua.native-evidence.v1`. The summary binds the raw reports to
the exact source revision, tested release-executable SHA-256, and this manifest.
It is accepted only when the full correctness matrix, permission/protected
boundaries, absolute benchmark gates, five-minute idle profile, and eight-hour
release profile pass on the same named runner.

The first accepted `summary.json` for each maintained runner must be reviewed
and committed under `benchmarks/baselines/`; until then, M2 has no pinned
baseline and remains incomplete. Later runner or baseline replacement follows
the comparison rule in the PRD and must preserve all absolute budgets.

The macOS release job intentionally resets the stable identity's TCC decisions
after collecting the granted-path evidence. The operator must re-approve that
identity before the next run. The Windows protected-target job is interactive:
the operator approves the fixture's UAC prompt while the sidecar remains at the
normal user integrity level. These are release-runner procedures, not developer
machine shortcuts.
