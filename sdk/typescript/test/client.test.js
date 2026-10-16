import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { inspect } from "node:util";
import test from "node:test";

import {
  ActionRequest,
  Client,
  ClipboardIndeterminateError,
  ClipboardRequest,
  CuaError,
  MutationIndeterminateError,
  PROTOCOL_VERSION,
  SensitiveText,
  selectApplication,
  selectWindow,
} from "../src/index.js";

const fixtureRoot = new URL("../../../fixtures/compatibility/nexus.cua.v1/", import.meta.url);

function fixture(name) {
  return JSON.parse(readFileSync(new URL(name, fixtureRoot), "utf8"));
}

function response(request, result) {
  return Buffer.from(JSON.stringify({
    protocol_version: PROTOCOL_VERSION,
    request_id: request.request_id,
    outcome: { status: "success", result },
  }));
}

function fixtureClient(transport, requestIds) {
  let index = 0;
  return new Client(
    { endpoint: "fixture-endpoint", tokenFile: "unused" },
    {
      token: "fixture-transport-token",
      transport,
      newRequestId: () => requestIds[index++],
      now: () => 1_000,
    },
  );
}

test("compatibility fixtures cover the complete public client", async () => {
  const requests = fixture("requests.json");
  const results = fixture("results.json");
  assert.equal(requests.length, 17);
  assert.equal(results.length, 17);
  const resultByOperation = new Map(
    requests.map((request, index) => [request.command.operation, results[index]]),
  );
  const sent = [];
  const client = fixtureClient(async (_endpoint, payload) => {
    const request = JSON.parse(payload.toString("utf8"));
    sent.push(request);
    return response(request, resultByOperation.get(request.command.operation));
  }, requests.map((request) => request.request_id));

  await client.getCapabilities();
  await client.getPermissionStatus();
  await client.discoverApplications();
  await client.discoverLaunchableApplications();
  await client.launchApplication({ launch_ref: "launch_fixture", allow_foreground_activation: true });
  await client.openSession({
    mode: "bounded",
    application_refs: ["discovery_fixture"],
    allowed_actions: ["focus_window", "invoke_element", "set_range_value", "click_point"],
    allow_foreground_input: true,
    restore_foreground_after_input: true,
    allow_display_capture: true,
    allow_clipboard_read: true,
    allow_clipboard_write: true,
    ttl_seconds: 300,
  });
  await client.closeSession("session_fixture");
  await client.listApps("session_fixture");
  await client.listWindows("session_fixture", "app_fixture");
  await client.listDisplays("session_fixture");
  await client.observeDisplay({ session_id: "session_fixture", display_ref: "display_fixture" });
  await client.observeWindow({
    session_id: "session_fixture",
    window_ref: "window_fixture",
    include_screenshot: true,
    accessibility: "interactive",
  });
  const clipboard = await client.readClipboard("session_fixture");
  assert(clipboard.text instanceof SensitiveText);
  assert.equal(clipboard.text.expose(), "clipboard fixture");
  await client.writeClipboard({
    session_id: "session_fixture",
    clipboard_ref: clipboard.clipboard_ref,
    text: new SensitiveText("replacement fixture"),
  });
  await client.performAction({
    session_id: "session_fixture",
    window_ref: "window_fixture",
    observation_id: "observation_fixture",
    action: { kind: "focus_window" },
  });
  await client.verifyState({
    session_id: "session_fixture",
    window_ref: "window_fixture",
    predicate: {
      kind: "element_state",
      role: "checkbox",
      name: "Remember me",
      state: "checked",
      value: true,
    },
  });
  await client.reconcileMutation("fixture-action", 0);

  assert.deepEqual(sent, requests);
});

test("every stable public error decodes as a closed CuaError", async () => {
  for (const [index, error] of fixture("errors.json").entries()) {
    const client = fixtureClient(async (_endpoint, payload) => {
      const request = JSON.parse(payload.toString("utf8"));
      return Buffer.from(JSON.stringify({
        protocol_version: PROTOCOL_VERSION,
        request_id: request.request_id,
        outcome: { status: "error", error },
      }));
    }, [`error_${index}`]);
    await assert.rejects(client.getCapabilities(), (caught) => {
      assert(caught instanceof CuaError);
      assert.equal(caught.code, error.code);
      assert.equal(caught.mutationStatus, error.mutation_status);
      return true;
    });
  }
});

test("indeterminate mutation retains one request identity for reconciliation", async () => {
  const actionResult = fixture("results.json").find((result) => result.result_type === "action_performed");
  const requestIds = ["stable_action_request"];
  let calls = 0;
  const client = fixtureClient(async (_endpoint, payload) => {
    calls += 1;
    if (calls === 1) throw new Error("caller wait elapsed");
    const request = JSON.parse(payload.toString("utf8"));
    assert.equal(request.request_id, "stable_action_request");
    assert.equal(request.timeout_ms, 60_000);
    return response(request, actionResult);
  }, requestIds);

  let retained;
  await assert.rejects(
    client.performAction({
      session_id: "session_fixture",
      window_ref: "window_fixture",
      observation_id: "observation_fixture",
      action: { kind: "focus_window" },
    }, 30_000),
    (error) => {
      assert(error instanceof MutationIndeterminateError);
      assert(error.request instanceof ActionRequest);
      retained = error.request;
      return true;
    },
  );
  const reconciled = await client.reconcileAction(retained, 60_000);
  assert.equal(reconciled.dispatched, true);
  assert.equal(calls, 2);
  await assert.rejects(client.reconcileAction(retained, 60_000), /already complete/);
});

test("indeterminate clipboard write retains its guarded request without leaking text", async () => {
  const writeResult = fixture("results.json").find((result) => result.result_type === "clipboard_written");
  let calls = 0;
  const client = fixtureClient(async (_endpoint, payload) => {
    calls += 1;
    if (calls === 1) throw new Error("caller wait elapsed");
    const request = JSON.parse(payload.toString("utf8"));
    assert.equal(request.request_id, "stable_clipboard_request");
    assert.equal(request.timeout_ms, 60_000);
    return response(request, writeResult);
  }, ["stable_clipboard_request"]);

  let retained;
  await assert.rejects(client.writeClipboard({
    session_id: "session_fixture",
    clipboard_ref: "clipboard_fixture",
    text: new SensitiveText("do-not-print"),
  }, 30_000), (error) => {
    assert(error instanceof ClipboardIndeterminateError);
    assert(error.request instanceof ClipboardRequest);
    assert(!inspect(error.request).includes("do-not-print"));
    retained = error.request;
    return true;
  });
  const reconciled = await client.reconcileClipboard(retained, 60_000);
  assert.equal(reconciled.dispatched, true);
  assert.equal(calls, 2);
  await assert.rejects(client.reconcileClipboard(retained, 60_000), /already complete/);
});

test("closed validation rejects authority smuggling and incoherent evidence", async () => {
  const sessionResult = fixture("results.json").find((result) => result.result_type === "session_opened");
  const client = fixtureClient(async (_endpoint, payload) => {
    const request = JSON.parse(payload.toString("utf8"));
    return response(request, sessionResult);
  }, ["manifest_request"]);
  await assert.rejects(client.openSession({
    mode: "read_only",
    application_refs: [],
    allowed_actions: ["focus_window"],
    allow_foreground_input: false,
    restore_foreground_after_input: false,
    allow_display_capture: true,
    allow_clipboard_read: false,
    allow_clipboard_write: false,
    ttl_seconds: 60,
  }), /cannot contain mutation authority/);

  const badClient = fixtureClient(async (_endpoint, payload) => {
    const request = JSON.parse(payload.toString("utf8"));
    return response(request, {
      result_type: "action_performed",
      data: {
        delivery_mode: "system",
        dispatched: true,
        effect_status: "confirmed",
        evidence_kind: "dispatch_acceptance",
        foreground_restoration: "not_requested",
        observation_invalidated: true,
      },
    });
  }, ["bad_action"]);
  await assert.rejects(badClient.performAction({
    session_id: "session_fixture",
    window_ref: "window_fixture",
    observation_id: "observation_fixture",
    action: { kind: "focus_window" },
  }), (error) => error instanceof MutationIndeterminateError
      && /incoherent action_performed effect evidence/.test(error.message));
});

test("sensitive text is redacted from formatting but explicit on the wire", () => {
  const secret = new SensitiveText("do-not-print");
  assert.equal(String(secret), "[REDACTED]");
  assert.equal(inspect(secret), "SensitiveText([REDACTED])");
  assert.equal(JSON.stringify(secret), '"do-not-print"');
  assert.equal(secret.expose(), "do-not-print");
});

test("selectors prefer exact matches and reject ambiguous substrings", () => {
  const applications = [
    { name: "AutoFill (Fixture)", application_id: "com.example.helper" },
    { name: "Fixture", application_id: "dev.example.fixture" },
  ];
  assert.equal(selectApplication(applications, "Fixture").application_id, "dev.example.fixture");
  assert.throws(() => selectApplication(applications, "example"), /ambiguous/);
  const windows = [{ title: "Fixture Help" }, { title: "Fixture" }];
  assert.equal(selectWindow(windows, "Fixture").title, "Fixture");
  assert.throws(() => selectWindow(windows, "Fixt"), /ambiguous/);
});
