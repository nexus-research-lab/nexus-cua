# Nexus CUA TypeScript client

This package is the typed trusted-host Node.js client for the private
`nexus.cua.v1` sidecar protocol. It uses one bounded length-prefixed request per
local Unix socket or Windows named pipe connection. It never exposes the
transport token through the client API.

```js
import { Client } from "@nexus-research-lab/nexus-cua";

const client = new Client({
  endpoint: process.env.NEXUS_CUA_ENDPOINT,
  tokenFile: process.env.NEXUS_CUA_TOKEN_FILE,
});

const capabilities = await client.getCapabilities();
console.log(capabilities.platform, capabilities.capture_modes);
```

The host owns sidecar supervision and the token file. On Unix, the client
rejects symlinks, non-regular files, foreign ownership, and any group/other
permission bits. Mutations return an immutable `ActionRequest`, `LaunchRequest`,
or `ClipboardRequest`; retain it and call the matching reconciliation method
after an indeterminate error. Never create a fresh request identity for the
same mutation.

Use `SensitiveText` for typed text or semantic value replacement so accidental
string formatting and object inspection remain redacted. Screenshots are
private transient files described by the protocol; the host must consume them
under its own policy and never log their content or path.

`selectWindow` accepts the opaque `window_ref` returned by `listWindows` before
falling back to exact or unique title matches.

Clipboard access has independent read/write authority. `readClipboard` returns
a redacted value plus a fresh opaque guard; `writeClipboard` consumes that guard
for a bounded compare-and-replace operation and never accepts a plain string.
