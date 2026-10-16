# Native Performance Baselines

This directory receives the first reviewed, accepted native-evidence summary
from each maintained release runner. The harness emits
`nexus.cua.native-evidence.v2` with distinct `engineering_accepted` and
`release_accepted` classes as defined by `docs/PRD.zh-CN.md`.
There is intentionally no baseline yet: neither required physical runner has
been assigned, and accepted hardware evidence must not be synthesized from a
developer machine, hosted worker, or virtual machine.

Use these names when the evidence exists:

- `macos-apple-silicon.json`
- `windows-x64.json`

Each file must be copied unchanged from the hardware workflow artifact and
must bind the source revision, release-executable SHA-256, complete runner
manifest, and accepted raw evidence matrix. Review the raw artifact before
committing it. Replacing either baseline requires the documented old/new
comparison and cannot relax an absolute runtime-contract budget.
