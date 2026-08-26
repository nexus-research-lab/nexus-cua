# Native Fixture Contract

The native fixtures are deterministic applications used to validate the real
macOS and Windows Computer Use drivers. They are test targets, not examples of
an agent-facing API.

Both applications implement the machine-readable contract in
[`contract.json`](contract.json). A harness must select the application through
trusted-host discovery, open a bounded session, locate the exact named window,
and drive the controls only through the public `nexus.cua.v1` protocol. It must
not use a platform process identifier, native window handle, or fixture-private
automation API as execution authority.

## Shared behavior

Each fresh process presents one visible window with a `960 × 720` content area
on macOS or a `960 × 720` top-level frame on Windows, titled
`Nexus CUA Native Fixture · Generation 1`. It exposes the same accessible names
and logical state on both platforms:

- an increment button and deterministic counter;
- a writable text field and a secure text field;
- a checkbox or toggle;
- a single-select list;
- an expandable section;
- a bounded scroll region with a marker initially outside the viewport;
- a draggable token and drop target;
- a state text element describing all non-sensitive state;
- a window-replacement action that destroys the current native top-level
  window and creates generation `n + 1`; and
- a geometry action that moves and resizes the current top-level window by a
  deterministic bounded delta;
- a minimize action used to prove that observation never restores a target
  implicitly; and
- a solid-magenta occluding window used to prove exact-window capture does not
  fall back to composited desktop pixels; and
- a next-display action used to validate screenshot mapping on maintained
  multiple-display, mixed-DPI, and negative-coordinate topologies; and
- two controlled provider-failure actions: one blocks the UI thread before a
  later action preflight, and one blocks inside the native invoke provider
  after mutation admission.

The state text format is stable and ordered:

```text
Fixture State: generation=1; counter=0; text=ready; checked=false; selection=none; expanded=false; dropped=false
```

The secure field is initialized with `fixture-secret`. Its value must never
appear in protocol output, harness reports, logs, screenshots promoted to test
evidence, or failure messages.

## Required scenario groups

The automated harness records every scenario as `passed`, `failed`,
`unsupported`, or `manual_required`. It never converts a missing permission,
driver capability, display topology, or protected target into a pass.

1. **Discovery and identity** — discover by the published application name,
   open a session from its short-lived discovery ref, and list the exact window.
2. **Observation** — capture pixels, validate screenshot mapping, compare
   interactive and full semantic views, and confirm truthful partial metadata.
3. **Semantic mutation** — focus, invoke, set value, toggle, select, and expand,
   observing again after every successful mutation.
4. **Foreground mutation** — focus the window, click, move, type, press keys,
   scroll, and drag through fresh screenshot coordinates.
5. **Staleness** — reject reuse of an invalidated observation, reject the old
   window after replacement, and rediscover after process restart.
6. **Lifecycle** — expire and close sessions, remove transient artifacts, and
   reconcile a timed-out admitted request with the same request ID.
7. **Desktop conditions** — minimized, occluded, multiple displays, negative
   coordinates, and mixed DPI where the runner topology supports them.
8. **Failure truthfulness** — permission denial/revocation, protected targets,
   and an intentionally hung accessibility provider.

## Platform implementations

- `macos/` is a Swift Package executable packaged as an AppKit application.
  Hardware automation launches the bundle through LaunchServices and waits for
  its exact accessibility window; invoking `Contents/MacOS` directly is not an
  equivalent application-readiness path.
- `windows/` is a self-contained .NET 8 WPF application using controls with
  native UI Automation patterns. Run `build.ps1 Release win-x64` on the
  maintained release runner, or select `win-arm64` for engineering validation.

Fixture build products and reports belong under ignored `target/` directories.
No generated binary is committed.

## Failure and permission probes

Set `NEXUS_CUA_FIXTURE_FAULT_ARM_MS` and
`NEXUS_CUA_FIXTURE_FAULT_HANG_MS` before launching a fresh fixture, then run
the two probes separately:

```bash
nexus-cua-native-harness --endpoint "$endpoint" --token-file "$token" \
  fault preflight --arm-delay-ms 3000
nexus-cua-native-harness --endpoint "$endpoint" --token-file "$token" \
  fault dispatch
```

The first requires `target_unresponsive/not_dispatched` for action preflight
and `target_unresponsive/not_applicable` for observation. The second requires
an admitted deadline and same-request result that are both `indeterminate`.
Restart the fixture between probes. On Windows,
`tools/native-harness/run-windows-fault.ps1` owns that lifecycle.

The `permission` subcommands record deliberately denied permissions, a
two-stage in-place macOS revocation, and an elevated Windows target. Revocation
must use the same running sidecar for `revoke-prepare` and `revoke-verify`; the
operator changes the OS grant between those commands. Windows global capture,
UIA, and input grants are reported as `not_applicable`; its required denial
boundary is the separate elevated/protected-target probe.

The hardware workflow uses a stable signed macOS test identity, resets its
post-event, Accessibility, and Screen Recording decisions only after all other
evidence, and leaves re-approval to the runner operator. Windows release
enforcement launches the fixture with `RunAs`; an operator must approve the UAC
prompt. The elevated fixture has a bounded automatic-exit timer.

Raw JSON files are not themselves a release pass. The `evidence` command merges
them with the repository-pinned runner manifest, source revision, and tested
runtime SHA-256. `--enforce` fails for a missing topology, permission boundary,
fault probe, benchmark, five-minute idle run, or eight-hour release soak.

Performance runners may launch either fixture with
`NEXUS_CUA_FIXTURE_PROFILE=1080p` or `NEXUS_CUA_FIXTURE_PROFILE=4k`. These
profiles change only the initial native window size; the default validation
profile and logical control contract remain unchanged. Evidence always records
the observed screenshot pixel dimensions because operating-system scaling can
make logical and physical sizes differ.
