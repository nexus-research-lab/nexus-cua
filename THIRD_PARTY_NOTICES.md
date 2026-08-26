# Third-party notices

Nexus Computer Use Runtime is MIT licensed. Its Rust dependency graph is pinned
in `Cargo.lock`, and the official Go client's dependency graph is pinned in
`sdk/go/go.sum`. The Python client has no third-party runtime dependency.
The direct runtime dependencies and their declared license families are:

| Area | Packages | Declared licenses |
| --- | --- | --- |
| Core | `async-trait`, `clap`, `fs2`, `hex`, `png`, `serde`, `serde_json`, `sha2`, `thiserror`, `time`, `tokio`, `tracing`, `uuid`, `zeroize` | MIT and/or Apache-2.0 |
| Schema | `schemars` | MIT |
| Constant-time comparison | `subtle` | BSD-3-Clause |
| macOS bindings | `accessibility-sys`, `core-foundation`, `core-graphics`, `foreign-types` | MIT and/or Apache-2.0 |
| macOS accessibility helper | `macos-accessibility-client` | Apache-2.0 |
| Apple framework bindings | `block2`, `objc2`, `objc2-app-kit`, `objc2-core-graphics`, `objc2-foundation`, `objc2-screen-capture-kit` | MIT, Zlib, and/or Apache-2.0 |
| Windows bindings | `windows`, `windows-sys`, and their support crates | MIT and/or Apache-2.0 |
| Go Windows named pipes | `github.com/Microsoft/go-winio` | MIT |
| Go platform system calls | `golang.org/x/sys` | BSD-3-Clause |

Transitive dependency names, exact versions, checksums, sources, and feature
resolution are recorded in `Cargo.lock` and `sdk/go/go.sum`. License files and
notices supplied by each package remain authoritative. This project does not
contain code copied from another Computer Use implementation.
