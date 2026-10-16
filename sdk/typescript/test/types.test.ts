import {
  Client,
  SensitiveText,
  type Action,
  type CapabilityManifest,
  type ClipboardSnapshot,
  type DisplayObservation,
} from "../src/index.js";

const client = new Client({ endpoint: "/private/socket", tokenFile: "/private/token" });
const displayOnly: CapabilityManifest = {
  mode: "read_only",
  application_refs: [],
  allowed_actions: [],
  allow_foreground_input: false,
  restore_foreground_after_input: false,
  allow_display_capture: true,
  allow_clipboard_read: false,
  allow_clipboard_write: false,
  ttl_seconds: 60,
};
const action: Action = {
  kind: "set_value",
  element_ref: "element_ref",
  value: new SensitiveText("private"),
};

async function compilePublicSurface(): Promise<void> {
  await client.openSession(displayOnly);
  const displays = await client.listDisplays("session_ref");
  const observed: DisplayObservation = await client.observeDisplay({
    session_id: "session_ref",
    display_ref: displays[0].display_ref,
  });
  observed.screenshot.mapping.pixel_size.width.toFixed(0);
  const clipboardOnly: CapabilityManifest = {
    mode: "bounded",
    application_refs: [],
    allowed_actions: [],
    allow_foreground_input: false,
    restore_foreground_after_input: false,
    allow_display_capture: false,
    allow_clipboard_read: true,
    allow_clipboard_write: true,
    ttl_seconds: 60,
  };
  await client.openSession(clipboardOnly);
  const clipboard: ClipboardSnapshot = await client.readClipboard("session_ref");
  if (clipboard.text !== null) clipboard.text.expose();
  const pending = await client.writeClipboard({
    session_id: "session_ref",
    clipboard_ref: clipboard.clipboard_ref,
    text: new SensitiveText("private"),
  });
  await client.reconcileClipboard(pending.request, 60_000).catch(() => undefined);
  await client.performAction({
    session_id: "session_ref",
    window_ref: "window_ref",
    observation_id: "window_observation_ref",
    action,
  });
}

void compilePublicSurface;

// @ts-expect-error unknown actions must fail at compile time
const invalidAction: Action = { kind: "run_shell", command: "whoami" };
void invalidAction;
