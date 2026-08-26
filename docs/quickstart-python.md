# Python quickstart

Status: source-build quickstart for the unreleased `0.1.0-alpha.1` developer
preview. No official package or PyPI project is published yet.

This path starts a private local sidecar, discovers the deterministic native
fixture, opens a finite capability session, observes one exact window, invokes
the fixture counter, observes fresh state, and closes the session. It does not
require raw protocol JSON, a PID, a native window handle, or a model provider.

## Prerequisites

- macOS 14 or newer on Apple Silicon, or Windows 11 with an interactive desktop;
- Rust 1.88 or newer;
- Python 3.10 through 3.13; and
- screen-capture and accessibility permissions reported by `nexus-cua doctor`.

The repository is not release-validated yet. Follow this source workflow only
on a development machine and review the current [platform evidence](platform-validation.md).

## 1. Build and inspect the runtime

From the repository root:

```bash
cargo +1.88.0 build --workspace --release --locked
cargo +1.88.0 run --locked --package nexus-cua -- doctor
```

Build and launch the deterministic fixture on macOS:

```bash
fixtures/native/macos/build.sh release
open "target/native-fixtures/macos/Nexus CUA Native Fixture.app"
```

On Windows PowerShell:

```powershell
fixtures/native/windows/build.ps1 Release win-x64
Start-Process target\native-fixtures\windows\nexus-cua-native-fixture.exe
```

## 2. Start the private sidecar

Keep this terminal open:

```bash
target/release/nexus-cua serve --dev-root .cache/quickstart
```

The service prints its local endpoint and token-file path. On macOS the default
endpoint is `.cache/quickstart/service.sock`. On Windows, use the named-pipe
endpoint printed by the process. The token file belongs to this trusted host
workflow; do not paste its contents into a prompt, log, agent environment, or
tool response.

## 3. Install and run the typed client

In a second terminal, still at the repository root:

```bash
python -m venv .cache/quickstart-venv
. .cache/quickstart-venv/bin/activate
python -m pip install ./sdk/python
python sdk/python/examples/quickstart.py \
  --endpoint "$PWD/.cache/quickstart/service.sock" \
  --token-file "$PWD/.cache/quickstart/token" \
  --application "Nexus CUA Native Fixture"
```

PowerShell activation and invocation are:

```powershell
py -3.13 -m venv .cache\quickstart-venv
.cache\quickstart-venv\Scripts\Activate.ps1
python -m pip install .\sdk\python
python sdk\python\examples\quickstart.py `
  --endpoint '<the printed \\.\pipe\... endpoint>' `
  --token-file "$PWD\.cache\quickstart\token" `
  --application 'Nexus CUA Native Fixture'
```

A successful run prints the selected window, fresh semantic-element count,
screenshot presence, and the truthful semantic mutation result. The example
uses a short-lived discovery ref and a 60-second session authorizing only
`invoke_element`. Application and window selection prefer an exact display
name and fail closed if a fallback substring is ambiguous.

## 4. Stop cleanly

The example closes its session in a `finally` block. Stop the sidecar with
Ctrl-C, then close the fixture. The runtime deletes its transient screenshot
generation; the host-owned `.cache/quickstart` directory remains available for
the next development run.

If a mutation ever raises `MutationIndeterminateError`, reconcile
`error.request` with a longer timeout. Never create another request ID to retry
an indeterminate mutation.
