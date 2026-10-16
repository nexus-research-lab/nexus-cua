import { randomBytes } from "node:crypto";
import { lstatSync, readFileSync } from "node:fs";
import { createConnection } from "node:net";

export const PROTOCOL_VERSION = "nexus.cua.v1";
export const DEFAULT_MAX_FRAME_BYTES = 1024 * 1024;
export const DEFAULT_RECONCILIATION_HORIZON_MS = 10 * 60 * 1000;

const ERROR_CODES = new Set([
  "protocol_mismatch", "unauthorized", "invalid_request", "busy",
  "deadline_exceeded", "session_unavailable", "stale_discovery",
  "capability_denied", "reference_not_found", "stale_observation",
  "stale_clipboard",
  "permission_required", "unsupported", "foreground_required",
  "target_unavailable", "target_unresponsive", "driver_failure", "internal",
]);
const MUTATION_STATUSES = new Set([
  "not_applicable", "not_dispatched", "indeterminate",
]);
const MUTATION_ADMISSIONS = new Set(["unknown", "admitted"]);
const MUTATION_DISPOSITIONS = new Set([
  "unknown", "not_mutation", "pending", "succeeded", "not_dispatched", "indeterminate",
]);
const ACTION_KINDS = new Set([
  "focus_window", "set_window_bounds", "set_window_minimized",
  "terminate_application", "focus_element", "invoke_element", "click_point",
  "press_point", "set_value", "set_range_value", "toggle_element",
  "select_element", "set_expanded", "move_pointer", "type_text",
  "press_keys", "scroll", "drag",
]);
const ACCESSIBILITY_MODES = new Set(["disabled", "interactive", "full"]);
const ELEMENT_STATES = new Set([
  "enabled", "focused", "selected", "checked", "expanded",
]);

export class CuaError extends Error {
  constructor(code, message, retryable, recoveryAction, mutationStatus) {
    super(`nexus-cua: ${code}: ${message}`);
    this.name = "CuaError";
    this.code = code;
    this.retryable = retryable;
    this.recoveryAction = recoveryAction;
    this.mutationStatus = mutationStatus;
  }
}

export class SensitiveText {
  #value;

  constructor(value) {
    if (typeof value !== "string") throw new TypeError("sensitive text must be a string");
    this.#value = value;
    Object.freeze(this);
  }

  toJSON() { return this.#value; }
  expose() { return this.#value; }
  toString() { return "[REDACTED]"; }
  [Symbol.for("nodejs.util.inspect.custom")]() { return "SensitiveText([REDACTED])"; }
}

class ReconciliationRequest {
  constructor(requestId, command, now) {
    this.requestId = requestId;
    this._command = command;
    this._createdAt = now;
    this._lastWait = 0;
    this._completed = false;
    this._invalidated = false;
    this._inFlight = false;
  }

  toString() { return `${this.constructor.name}(${this.requestId})`; }
}

export class ActionRequest extends ReconciliationRequest {}
export class LaunchRequest extends ReconciliationRequest {}
export class ClipboardRequest extends ReconciliationRequest {}

export class MutationIndeterminateError extends Error {
  constructor(request, cause) {
    super(`nexus-cua: mutation ${request.requestId} is indeterminate: ${errorMessage(cause)}`, { cause });
    this.name = "MutationIndeterminateError";
    this.request = request;
  }
}

export class LaunchIndeterminateError extends Error {
  constructor(request, cause) {
    super(`nexus-cua: launch ${request.requestId} is indeterminate: ${errorMessage(cause)}`, { cause });
    this.name = "LaunchIndeterminateError";
    this.request = request;
  }
}

export class ClipboardIndeterminateError extends Error {
  constructor(request, cause) {
    super(`nexus-cua: clipboard mutation ${request.requestId} is indeterminate: ${errorMessage(cause)}`, { cause });
    this.name = "ClipboardIndeterminateError";
    this.request = request;
  }
}

export class Client {
  constructor(config, internal = undefined) {
    if (!config || typeof config.endpoint !== "string" || config.endpoint.length === 0) {
      throw new TypeError("endpoint is required");
    }
    this._endpoint = config.endpoint;
    this._maxFrameBytes = config.maxFrameBytes ?? DEFAULT_MAX_FRAME_BYTES;
    assertPositiveU32(this._maxFrameBytes, "maxFrameBytes");
    this._reconciliationHorizonMs = config.reconciliationHorizonMs
      ?? DEFAULT_RECONCILIATION_HORIZON_MS;
    if (!Number.isSafeInteger(this._reconciliationHorizonMs)
        || this._reconciliationHorizonMs < DEFAULT_RECONCILIATION_HORIZON_MS) {
      throw new RangeError("reconciliationHorizonMs must be at least ten minutes");
    }
    this._token = internal?.token ?? readPrivateToken(config.tokenFile);
    this._transport = internal?.transport ?? nativeRoundTrip;
    this._newRequestId = internal?.newRequestId
      ?? (() => `request_${randomBytes(16).toString("hex")}`);
    this._now = internal?.now ?? (() => Date.now());
  }

  toString() { return "Client([PRIVATE TRANSPORT])"; }

  async getCapabilities(timeoutMs = 30_000) {
    return this._requestResult({ operation: "get_capabilities" }, timeoutMs, "capabilities");
  }

  async getPermissionStatus(timeoutMs = 30_000) {
    return this._requestResult({ operation: "get_permission_status" }, timeoutMs, "permission_status");
  }

  async discoverApplications(timeoutMs = 30_000) {
    return this._requestResult({ operation: "discover_applications" }, timeoutMs, "applications_discovered");
  }

  async discoverLaunchableApplications(timeoutMs = 30_000) {
    return this._requestResult(
      { operation: "discover_launchable_applications" },
      timeoutMs,
      "launchable_applications_discovered",
    );
  }

  async launchApplication(input, timeoutMs = 30_000) {
    if (!input || typeof input.launch_ref !== "string" || input.launch_ref.trim() === "") {
      throw new TypeError("launch_ref is required");
    }
    const request = new LaunchRequest(
      this._newRequestId(),
      { operation: "launch_application", input: { ...input } },
      this._now(),
    );
    return { output: await this._waitForMutation(request, timeoutMs, "application_launched", true), request };
  }

  async reconcileLaunch(request, timeoutMs) {
    if (!(request instanceof LaunchRequest)) throw new TypeError("request must be a LaunchRequest");
    return this._waitForMutation(request, timeoutMs, "application_launched", true);
  }

  async openSession(manifest, timeoutMs = 30_000) {
    validateManifest(manifest);
    return this._requestResult(
      { operation: "open_session", input: { manifest: { ...manifest } } },
      timeoutMs,
      "session_opened",
    );
  }

  async closeSession(sessionId, timeoutMs = 30_000) {
    return this._requestAcknowledged(
      { operation: "close_session", input: { session_id: requiredString(sessionId, "sessionId") } },
      timeoutMs,
    );
  }

  async listApps(sessionId, timeoutMs = 30_000) {
    return this._requestResult(
      { operation: "list_apps", input: { session_id: requiredString(sessionId, "sessionId") } },
      timeoutMs,
      "apps",
    );
  }

  async listWindows(sessionId, appRef = null, timeoutMs = 30_000) {
    if (appRef !== null) requiredString(appRef, "appRef");
    return this._requestResult(
      { operation: "list_windows", input: { session_id: requiredString(sessionId, "sessionId"), app_ref: appRef } },
      timeoutMs,
      "windows",
    );
  }

  async listDisplays(sessionId, timeoutMs = 30_000) {
    return this._requestResult(
      { operation: "list_displays", input: { session_id: requiredString(sessionId, "sessionId") } },
      timeoutMs,
      "displays",
    );
  }

  async observeDisplay(input, timeoutMs = 30_000) {
    validateSessionTarget(input, "display_ref");
    return this._requestResult(
      { operation: "observe_display", input: { ...input } },
      timeoutMs,
      "display_observed",
    );
  }

  async observeWindow(input, timeoutMs = 30_000) {
    validateSessionTarget(input, "window_ref");
    if (!ACCESSIBILITY_MODES.has(input.accessibility)) {
      throw new TypeError(`invalid accessibility mode ${String(input.accessibility)}`);
    }
    return this._requestResult(
      { operation: "observe_window", input: { ...input } },
      timeoutMs,
      "window_observed",
    );
  }

  async readClipboard(sessionId, timeoutMs = 30_000) {
    const output = await this._requestResult(
      { operation: "read_clipboard", input: { session_id: requiredString(sessionId, "sessionId") } },
      timeoutMs,
      "clipboard_read",
    );
    return {
      ...output,
      text: output.text === null ? null : new SensitiveText(output.text),
    };
  }

  async writeClipboard(input, timeoutMs = 30_000) {
    const value = objectValue(input, "clipboard input");
    requiredString(value.session_id, "session_id");
    requiredString(value.clipboard_ref, "clipboard_ref");
    if (!(value.text instanceof SensitiveText)) {
      throw new TypeError("clipboard text must use SensitiveText");
    }
    const request = new ClipboardRequest(
      this._newRequestId(),
      { operation: "write_clipboard", input: { ...value } },
      this._now(),
    );
    return {
      output: await this._waitForMutation(request, timeoutMs, "clipboard_written", false, true),
      request,
    };
  }

  async reconcileClipboard(request, timeoutMs) {
    if (!(request instanceof ClipboardRequest)) throw new TypeError("request must be a ClipboardRequest");
    return this._waitForMutation(request, timeoutMs, "clipboard_written", false, true);
  }

  async performAction(input, timeoutMs = 30_000) {
    validateGuardedInput(input);
    const action = actionWire(input.action);
    const request = new ActionRequest(
      this._newRequestId(),
      {
        operation: "perform_action",
        input: {
          session_id: input.session_id,
          window_ref: input.window_ref,
          observation_id: input.observation_id,
          action,
        },
      },
      this._now(),
    );
    return { output: await this._waitForMutation(request, timeoutMs, "action_performed", false), request };
  }

  async reconcileAction(request, timeoutMs) {
    if (!(request instanceof ActionRequest)) throw new TypeError("request must be an ActionRequest");
    return this._waitForMutation(request, timeoutMs, "action_performed", false);
  }

  async verifyState(input, timeoutMs = 30_000) {
    validateSessionTarget(input, "window_ref");
    return this._requestResult(
      { operation: "verify_state", input: { ...input, predicate: predicateWire(input.predicate) } },
      timeoutMs,
      "state_verified",
    );
  }

  async reconcileMutation(requestId, waitMs = 0, timeoutMs = 30_000) {
    if (typeof requestId !== "string"
        || !/^[A-Za-z0-9_.:-]{1,128}$/.test(requestId)) {
      throw new TypeError("requestId must be normalized ASCII within 128 bytes");
    }
    if (!Number.isInteger(waitMs) || waitMs < 0 || waitMs > 120_000) {
      throw new RangeError("waitMs must be between zero and 120000");
    }
    return this._requestResult(
      { operation: "reconcile_mutation", input: { request_id: requestId, wait_ms: waitMs } },
      timeoutMs,
      "mutation_reconciled",
    );
  }

  async _waitForMutation(request, timeoutMs, resultType, launch, clipboard = false) {
    if (request._inFlight) throw new Error("reconciliation request is already in flight");
    if (request._completed) throw new Error("mutation request is already complete");
    const ErrorType = launch
      ? LaunchIndeterminateError
      : clipboard
        ? ClipboardIndeterminateError
        : MutationIndeterminateError;
    if (request._invalidated || this._now() - request._createdAt > this._reconciliationHorizonMs) {
      request._invalidated = true;
      request._command = undefined;
      throw new ErrorType(request, new Error("reconciliation horizon elapsed"));
    }
    const wait = timeoutMilliseconds(timeoutMs);
    if (request._lastWait !== 0 && wait < request._lastWait) {
      throw new RangeError("reconciliation may only extend the prior wait deadline");
    }
    request._lastWait = wait;
    request._inFlight = true;
    try {
      const response = await this._roundTrip(request.requestId, wait, request._command);
      try {
        const output = decodeOutcome(response.outcome, resultType);
        request._completed = true;
        request._command = undefined;
        return output;
      } catch (error) {
        if (error instanceof CuaError && error.mutationStatus === "not_dispatched") {
          request._completed = true;
          request._command = undefined;
          throw error;
        }
        throw new ErrorType(request, error);
      }
    } catch (error) {
      if (error instanceof CuaError && error.mutationStatus === "not_dispatched") throw error;
      if (error instanceof ErrorType) throw error;
      throw new ErrorType(request, error);
    } finally {
      request._inFlight = false;
    }
  }

  async _requestResult(command, timeoutMs, resultType) {
    const requestId = this._newRequestId();
    const response = await this._roundTrip(requestId, timeoutMilliseconds(timeoutMs), command);
    return decodeOutcome(response.outcome, resultType);
  }

  async _requestAcknowledged(command, timeoutMs) {
    const requestId = this._newRequestId();
    const response = await this._roundTrip(requestId, timeoutMilliseconds(timeoutMs), command);
    decodeOutcome(response.outcome, "acknowledged", true);
  }

  async _roundTrip(requestId, timeoutMs, command) {
    const envelope = {
      protocol_version: PROTOCOL_VERSION,
      request_id: requestId,
      timeout_ms: timeoutMs,
      authorization: this._token,
      command,
    };
    const payload = Buffer.from(JSON.stringify(envelope));
    if (payload.length === 0 || payload.length > this._maxFrameBytes) {
      throw new RangeError("request frame exceeds configured bound");
    }
    const raw = await this._transport(this._endpoint, payload, this._maxFrameBytes, timeoutMs);
    let response;
    try { response = JSON.parse(Buffer.from(raw).toString("utf8")); }
    catch (error) { throw new Error(`invalid response JSON: ${errorMessage(error)}`); }
    const data = closedObject(response, ["protocol_version", "request_id", "outcome"], "response envelope");
    if (data.protocol_version !== PROTOCOL_VERSION) {
      throw new Error(`unsupported response protocol ${String(data.protocol_version)}`);
    }
    if (data.request_id !== requestId) throw new Error("response request_id mismatch");
    return data;
  }
}

export function selectApplication(applications, selector) {
  requiredString(selector, "selector");
  const exact = applications.filter((app) => app.name === selector || app.application_id === selector);
  if (exact.length === 1) return exact[0];
  if (exact.length > 1) throw new Error(`application selector ${JSON.stringify(selector)} has ${exact.length} exact matches`);
  const partial = applications.filter((app) => app.name.includes(selector) || app.application_id.includes(selector));
  if (partial.length === 1) return partial[0];
  if (partial.length === 0) throw new Error(`application selector ${JSON.stringify(selector)} did not match discovery`);
  throw new Error(`application selector ${JSON.stringify(selector)} is ambiguous across ${partial.length} matches`);
}

export function selectWindow(windows, selector) {
  requiredString(selector, "selector");
  const exact = windows.filter((window) => window.title === selector);
  if (exact.length === 1) return exact[0];
  if (exact.length > 1) throw new Error(`window selector ${JSON.stringify(selector)} has ${exact.length} exact matches`);
  const partial = windows.filter((window) => window.title.includes(selector));
  if (partial.length === 1) return partial[0];
  if (partial.length === 0) throw new Error(`window selector ${JSON.stringify(selector)} did not match discovery`);
  throw new Error(`window selector ${JSON.stringify(selector)} is ambiguous across ${partial.length} matches`);
}

function readPrivateToken(path) {
  if (typeof path !== "string" || path.length === 0) throw new TypeError("tokenFile is required");
  const status = lstatSync(path);
  if (!status.isFile() || status.isSymbolicLink()) throw new Error("token file must be a regular non-symlink file");
  if (process.platform !== "win32") {
    if ((status.mode & 0o077) !== 0) throw new Error("token file permissions must exclude group and other access");
    if (typeof process.getuid === "function" && status.uid !== process.getuid()) {
      throw new Error("token file must be owned by the current user");
    }
  }
  const bytes = readFileSync(path);
  if (bytes.length < 32 || bytes.length > 4096) throw new Error("token file size is outside the supported bound");
  const token = bytes.toString("utf8").trim();
  if (Buffer.byteLength(token) < 32 || Buffer.byteLength(token) > 4096) {
    throw new Error("transport token length is outside the supported bound");
  }
  return token;
}

function nativeRoundTrip(endpoint, payload, maxFrameBytes, timeoutMs) {
  return new Promise((resolve, reject) => {
    const socket = createConnection(endpoint);
    const header = Buffer.allocUnsafe(4);
    header.writeUInt32BE(payload.length);
    let buffered = Buffer.alloc(0);
    let expected = null;
    let settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      if (error) reject(error); else resolve(value);
    };
    socket.setTimeout(Math.min(timeoutMs + 1000, 0x7fffffff));
    socket.on("timeout", () => finish(new Error("local transport timed out")));
    socket.on("error", (error) => finish(error));
    socket.on("connect", () => socket.write(Buffer.concat([header, payload])));
    socket.on("data", (chunk) => {
      buffered = Buffer.concat([buffered, chunk]);
      if (expected === null && buffered.length >= 4) {
        expected = buffered.readUInt32BE(0);
        buffered = buffered.subarray(4);
        if (expected === 0 || expected > maxFrameBytes) {
          finish(new RangeError(`response frame length ${expected} is outside configured bound`));
          return;
        }
      }
      if (expected !== null && buffered.length >= expected) {
        if (buffered.length !== expected) {
          finish(new Error("response contains trailing frame bytes"));
          return;
        }
        finish(undefined, buffered);
      }
    });
    socket.on("end", () => {
      if (!settled) finish(new Error("local transport closed before a complete frame"));
    });
  });
}

function decodeOutcome(value, expectedResult, acknowledged = false) {
  const outcome = objectValue(value, "outcome");
  if (outcome.status === "error") {
    closedObject(outcome, ["status", "error"], "error outcome");
    throw decodeCuaError(outcome.error);
  }
  if (outcome.status !== "success") throw new Error(`unknown outcome status ${String(outcome.status)}`);
  closedObject(outcome, ["status", "result"], "success outcome");
  const result = objectValue(outcome.result, "result");
  const keys = acknowledged ? ["result_type"] : ["result_type", "data"];
  closedObject(result, keys, `${expectedResult} result`);
  if (result.result_type !== expectedResult) {
    throw new Error(`expected result ${expectedResult}, received ${String(result.result_type)}`);
  }
  if (acknowledged) return undefined;
  validateResultData(expectedResult, result.data);
  return result.data;
}

function decodeCuaError(value) {
  const error = closedObject(
    value,
    ["code", "message", "retryable", "recovery_action", "mutation_status"],
    "public error",
  );
  if (!ERROR_CODES.has(error.code)) throw new Error(`unknown public error code ${String(error.code)}`);
  if (!MUTATION_STATUSES.has(error.mutation_status)) {
    throw new Error(`unknown mutation status ${String(error.mutation_status)}`);
  }
  return new CuaError(error.code, error.message, error.retryable, error.recovery_action, error.mutation_status);
}

function validateResultData(resultType, value) {
  if (["apps", "windows", "displays"].includes(resultType)) {
    if (!Array.isArray(value)) throw new TypeError(`${resultType} result data must be an array`);
    return;
  }
  const data = objectValue(value, `${resultType} data`);
  if (resultType === "capabilities") {
    if (!new Set(["macos", "windows", "unsupported"]).has(data.platform)) throw new Error("unknown platform");
    assertClosedArray(data.capture_modes, new Set(["window", "display"]), "capture mode");
    assertClosedArray(data.input_routes, new Set(["semantic", "system", "background", "foreground"]), "input route");
    assertClosedArray(data.application_lifecycle, new Set(["launch"]), "application lifecycle operation");
    assertClosedArray(data.clipboard, new Set(["read_text", "write_text"]), "clipboard operation");
    assertClosedArray(data.actions, ACTION_KINDS, "action kind");
  } else if (resultType === "permission_status") {
    const states = new Set(["granted", "denied", "not_determined", "not_applicable", "unknown"]);
    for (const field of ["screen_capture", "accessibility", "input_control"]) {
      if (!states.has(data[field])) throw new Error(`unknown permission state ${String(data[field])}`);
    }
  } else if (resultType === "action_performed" || resultType === "clipboard_written") {
    const coherent = (data.effect_status === "confirmed" && data.evidence_kind === "native_readback")
      || (data.effect_status === "unverified" && data.evidence_kind === "dispatch_acceptance");
    if (!coherent) throw new Error(`incoherent ${resultType} effect evidence`);
  } else if (resultType === "mutation_reconciled") {
    const result = closedObject(data, [
      "request_id", "runtime_epoch", "worker_epoch", "admission", "disposition",
      "dispatched", "effect_status", "evidence_kind", "error_code",
      "fresh_observation_required",
    ], "mutation reconciliation");
    requiredString(result.request_id, "request_id");
    requiredString(result.runtime_epoch, "runtime_epoch");
    if (result.worker_epoch !== null) requiredString(result.worker_epoch, "worker_epoch");
    if (!MUTATION_ADMISSIONS.has(result.admission)) throw new Error("unknown mutation admission");
    if (!MUTATION_DISPOSITIONS.has(result.disposition)) throw new Error("unknown mutation disposition");
    if (result.dispatched !== null && typeof result.dispatched !== "boolean") {
      throw new TypeError("mutation dispatched evidence must be boolean or null");
    }
    if ((result.effect_status === null) !== (result.evidence_kind === null)) {
      throw new Error("mutation effect status and evidence must both be present or absent");
    }
    if (result.effect_status !== null) {
      const coherent = (result.effect_status === "confirmed" && result.evidence_kind === "native_readback")
        || (result.effect_status === "unverified" && result.evidence_kind === "dispatch_acceptance");
      if (!coherent) throw new Error("incoherent mutation reconciliation effect evidence");
    }
    if (result.error_code !== null && !ERROR_CODES.has(result.error_code)) {
      throw new Error("unknown mutation reconciliation error code");
    }
    if (typeof result.fresh_observation_required !== "boolean") {
      throw new TypeError("fresh_observation_required must be boolean");
    }
  } else if (resultType === "clipboard_read") {
    const clipboard = closedObject(data, ["clipboard_ref", "text", "byte_length"], "clipboard result");
    requiredString(clipboard.clipboard_ref, "clipboard_ref");
    if (clipboard.text !== null && typeof clipboard.text !== "string") {
      throw new TypeError("clipboard text must be a string or null");
    }
    if (!Number.isInteger(clipboard.byte_length) || clipboard.byte_length < 0 || clipboard.byte_length > 0xffffffff) {
      throw new RangeError("clipboard byte length must fit u32");
    }
    const expected = clipboard.text === null ? 0 : Buffer.byteLength(clipboard.text, "utf8");
    if (clipboard.byte_length !== expected) throw new Error("clipboard byte length mismatch");
  }
}

function actionWire(action) {
  const value = objectValue(action, "action");
  if (!ACTION_KINDS.has(value.kind)) throw new TypeError(`unsupported action kind ${String(value.kind)}`);
  if (value.kind === "set_window_bounds") validateRect(value.bounds);
  if (value.kind === "click_point") {
    if (!new Set(["left", "middle", "right"]).has(value.button)
        || !Number.isInteger(value.count) || value.count < 1 || value.count > 255) {
      throw new TypeError("invalid click button or count");
    }
    const delivery = value.delivery ?? "foreground";
    if (!new Set(["background", "foreground"]).has(delivery)) throw new TypeError("invalid click delivery");
    return { ...value, delivery };
  }
  if (value.kind === "set_range_value"
      && (!Number.isFinite(value.value) || Math.abs(value.value) > 1_000_000_000_000)) {
    throw new RangeError("numeric range value is outside the supported range");
  }
  if ((value.kind === "set_value" && !(value.value instanceof SensitiveText))
      || (value.kind === "type_text" && !(value.text instanceof SensitiveText))) {
    throw new TypeError("sensitive action text must use SensitiveText");
  }
  if (value.kind === "press_keys"
      && (!Array.isArray(value.keys) || value.keys.length === 0
        || value.keys.some((key) => typeof key !== "string" || key.length === 0))) {
    throw new TypeError("key chord must contain non-empty keys");
  }
  return { ...value };
}

function predicateWire(predicate) {
  const value = objectValue(predicate, "predicate");
  if (!new Set(["window_title_contains", "element_exists", "element_state", "bounds_contained"]).has(value.kind)) {
    throw new TypeError(`unsupported predicate kind ${String(value.kind)}`);
  }
  if (value.kind === "element_state" && !ELEMENT_STATES.has(value.state)) {
    throw new TypeError(`invalid element state ${String(value.state)}`);
  }
  return { ...value };
}

function validateManifest(manifest) {
  const value = objectValue(manifest, "manifest");
  if (!new Set(["read_only", "bounded"]).has(value.mode)) throw new TypeError("invalid permission mode");
  if (!Array.isArray(value.application_refs) || !Array.isArray(value.allowed_actions)) {
    throw new TypeError("manifest refs and actions must be arrays");
  }
  assertClosedArray(value.allowed_actions, ACTION_KINDS, "action kind");
  assertPositiveU32(value.ttl_seconds, "ttl_seconds");
  if (value.restore_foreground_after_input && !value.allow_foreground_input) {
    throw new TypeError("foreground restoration requires foreground input authority");
  }
  if (value.allow_clipboard_write && !value.allow_clipboard_read) {
    throw new TypeError("clipboard writes require clipboard-read authority");
  }
  if (value.mode === "read_only") {
    if (value.allowed_actions.length !== 0 || value.allow_foreground_input || value.restore_foreground_after_input || value.allow_clipboard_write) {
      throw new TypeError("read_only manifest cannot contain mutation authority");
    }
    if (value.application_refs.length === 0 && !value.allow_display_capture && !value.allow_clipboard_read) {
      throw new TypeError("read_only manifest requires an application, display, or clipboard authority");
    }
  } else if (value.application_refs.length === 0 && !value.allow_display_capture && !value.allow_clipboard_read && !value.allow_clipboard_write) {
    throw new TypeError("bounded manifest requires an application, display, or clipboard authority");
  }
}

function validateSessionTarget(input, field) {
  const value = objectValue(input, "input");
  requiredString(value.session_id, "session_id");
  requiredString(value[field], field);
}

function validateGuardedInput(input) {
  validateSessionTarget(input, "window_ref");
  requiredString(input.observation_id, "observation_id");
}

function validateRect(value) {
  const rect = objectValue(value, "bounds");
  for (const field of ["x", "y", "width", "height"]) {
    if (!Number.isFinite(rect[field])) throw new RangeError("window bounds must be finite");
  }
  if (rect.width <= 0 || rect.height <= 0) throw new RangeError("window bounds must be positive");
}

function timeoutMilliseconds(value) {
  assertPositiveU32(value, "timeoutMs");
  return value;
}

function assertPositiveU32(value, name) {
  if (!Number.isInteger(value) || value <= 0 || value > 0xffffffff) {
    throw new RangeError(`${name} must fit a non-zero u32`);
  }
}

function assertClosedArray(value, allowed, name) {
  if (!Array.isArray(value) || value.some((item) => !allowed.has(item))) {
    throw new Error(`unknown ${name}`);
  }
}

function requiredString(value, name) {
  if (typeof value !== "string" || value.trim() === "") throw new TypeError(`${name} is required`);
  return value;
}

function objectValue(value, name) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${name} must be an object`);
  }
  return value;
}

function closedObject(value, fields, name) {
  const object = objectValue(value, name);
  const actual = Object.keys(object).sort();
  const expected = [...fields].sort();
  if (actual.length !== expected.length || actual.some((field, index) => field !== expected[index])) {
    throw new Error(`${name} contains missing or unknown fields`);
  }
  return object;
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
