export const PROTOCOL_VERSION: "nexus.cua.v1";
export const DEFAULT_MAX_FRAME_BYTES: number;
export const DEFAULT_RECONCILIATION_HORIZON_MS: number;

export type Platform = "macos" | "windows" | "unsupported";
export type CaptureMode = "window" | "display";
export type InputRoute = "semantic" | "system" | "background" | "foreground";
export type ClipboardOperation = "read_text" | "write_text";
export type PermissionState = "granted" | "denied" | "not_determined" | "not_applicable" | "unknown";
export type PermissionMode = "read_only" | "bounded";
export type AccessibilityMode = "disabled" | "interactive" | "full";
export type PointerButton = "left" | "middle" | "right";
export type DeliveryPreference = "background" | "foreground";
export type DeliveryMode = "semantic" | "system" | "background" | "foreground";
export type ActionEffectStatus = "confirmed" | "unverified";
export type ActionEvidenceKind = "native_readback" | "dispatch_acceptance";
export type ForegroundRestorationStatus = "not_requested" | "not_needed" | "restored" | "skipped_owner_changed" | "target_unavailable" | "failed";
export type MutationStatus = "not_applicable" | "not_dispatched" | "indeterminate";
export type MutationAdmission = "unknown" | "admitted";
export type MutationDisposition = "unknown" | "not_mutation" | "pending" | "succeeded" | "not_dispatched" | "indeterminate";
export type ErrorCode = "protocol_mismatch" | "unauthorized" | "invalid_request" | "busy" | "deadline_exceeded" | "session_unavailable" | "stale_discovery" | "capability_denied" | "reference_not_found" | "stale_observation" | "stale_clipboard" | "permission_required" | "unsupported" | "foreground_required" | "target_unavailable" | "target_unresponsive" | "driver_failure" | "internal";
export type ActionKind = "focus_window" | "set_window_bounds" | "set_window_minimized" | "terminate_application" | "focus_element" | "invoke_element" | "click_point" | "press_point" | "set_value" | "set_range_value" | "toggle_element" | "select_element" | "set_expanded" | "move_pointer" | "type_text" | "press_keys" | "scroll" | "drag";
export type ElementState = "enabled" | "focused" | "selected" | "checked" | "expanded";
export type SignatureStatus = "verified" | "invalid" | "unsigned" | "unknown";
export type TruncationReason = "node_limit" | "depth_limit" | "byte_limit" | "deadline" | "provider_failure";

export interface Config {
  endpoint: string;
  tokenFile: string;
  maxFrameBytes?: number;
  reconciliationHorizonMs?: number;
}

export class CuaError extends Error {
  readonly code: ErrorCode;
  readonly retryable: boolean;
  readonly recoveryAction: string | null;
  readonly mutationStatus: MutationStatus;
}

export class SensitiveText {
  constructor(value: string);
  toJSON(): string;
  expose(): string;
  toString(): "[REDACTED]";
}

export interface DriverCapabilities {
  protocol_version: string;
  runtime_version: string;
  platform: Platform;
  capture_modes: CaptureMode[];
  accessibility_tree: boolean;
  input_routes: InputRoute[];
  application_lifecycle: "launch"[];
  clipboard: ClipboardOperation[];
  actions: ActionKind[];
}

export interface PermissionStatus {
  screen_capture: PermissionState;
  accessibility: PermissionState;
  input_control: PermissionState;
}

export interface ApplicationProvenance {
  platform: Platform;
  bundle_id?: string | null;
  executable_path: string | null;
  signing_team_id?: string | null;
  designated_requirement?: string | null;
  publisher?: string | null;
  signature_status?: SignatureStatus | null;
}

export interface DiscoveredApplication {
  discovery_ref: string;
  name: string;
  application_id: string;
  foreground: boolean;
  provenance: ApplicationProvenance;
  expires_at: string;
}

export interface DiscoverApplicationsOutput {
  applications: DiscoveredApplication[];
  complete: boolean;
}

export interface LaunchableApplication {
  launch_ref: string;
  name: string;
  application_id: string;
  provenance: ApplicationProvenance;
  expires_at: string;
}

export interface DiscoverLaunchableApplicationsOutput {
  applications: LaunchableApplication[];
  complete: boolean;
}

export interface LaunchApplicationInput {
  launch_ref: string;
  allow_foreground_activation: boolean;
}

export interface ApplicationLaunchOutput {
  application: DiscoveredApplication;
  delivery_mode: DeliveryMode;
  dispatched: boolean;
}

export interface CapabilityManifest {
  mode: PermissionMode;
  application_refs: string[];
  allowed_actions: ActionKind[];
  allow_foreground_input: boolean;
  restore_foreground_after_input: boolean;
  allow_display_capture: boolean;
  allow_clipboard_read: boolean;
  allow_clipboard_write: boolean;
  ttl_seconds: number;
}

export interface OpenSessionOutput { session_id: string; expires_at: string }
export interface ApplicationSummary { app_ref: string; name: string; application_id: string; foreground: boolean }
export interface ScreenRect { x: number; y: number; width: number; height: number }
export interface ScreenshotPoint { x: number; y: number }
export interface PixelSize { width: number; height: number }
export interface ScreenshotMapping { screen_bounds: ScreenRect; pixel_size: PixelSize }
export interface WindowSummary { window_ref: string; app_ref: string; title: string; screen_bounds: ScreenRect; minimized: boolean; visible: boolean; foreground: boolean }
export interface DisplaySummary { display_ref: string; screen_bounds: ScreenRect; primary: boolean }

export interface ScreenshotArtifact {
  artifact_ref: string;
  path: string;
  mime_type: string;
  mapping: ScreenshotMapping;
  byte_length: number;
  sha256: string;
}

export interface AccessibilityRangeValue {
  value: number;
  minimum: number;
  maximum: number;
  small_change: number | null;
  large_change: number | null;
  read_only: boolean;
}

export interface AccessibilityElement {
  element_ref: string;
  parent_ref: string | null;
  role: string;
  name: string;
  value: string | null;
  range_value: AccessibilityRangeValue | null;
  screen_bounds: ScreenRect | null;
  enabled: boolean;
  focused: boolean;
  selected: boolean | null;
  checked: boolean | null;
  expanded: boolean | null;
  actions: string[];
}

export interface ObservationTruncation { reason: TruncationReason; emitted_elements: number }
export interface WindowObservation {
  observation_id: string;
  window_ref: string;
  captured_at: string;
  window_screen_bounds: ScreenRect;
  screenshot: ScreenshotArtifact | null;
  elements: AccessibilityElement[];
  elements_complete: boolean;
  elements_truncation: ObservationTruncation | null;
}
export interface DisplayObservation {
  observation_id: string;
  display_ref: string;
  captured_at: string;
  display_screen_bounds: ScreenRect;
  screenshot: ScreenshotArtifact;
}

export interface ClipboardSnapshot {
  clipboard_ref: string;
  text: SensitiveText | null;
  byte_length: number;
}

export interface ClipboardWriteInput {
  session_id: string;
  clipboard_ref: string;
  text: SensitiveText;
}

export interface ClipboardWriteOutput {
  clipboard_ref: string;
  delivery_mode: DeliveryMode;
  dispatched: boolean;
  effect_status: ActionEffectStatus;
  evidence_kind: ActionEvidenceKind;
}

export interface ActionOutput {
  delivery_mode: DeliveryMode;
  dispatched: boolean;
  effect_status: ActionEffectStatus;
  evidence_kind: ActionEvidenceKind;
  foreground_restoration: ForegroundRestorationStatus;
  observation_invalidated: boolean;
}
export interface VerificationOutput { matched: boolean; evidence: string }
export interface MutationReconciliation {
  request_id: string;
  runtime_epoch: string;
  worker_epoch: string | null;
  admission: MutationAdmission;
  disposition: MutationDisposition;
  dispatched: boolean | null;
  effect_status: ActionEffectStatus | null;
  evidence_kind: ActionEvidenceKind | null;
  error_code: ErrorCode | null;
  fresh_observation_required: boolean;
}

export type Action =
  | { kind: "focus_window" }
  | { kind: "set_window_bounds"; bounds: ScreenRect }
  | { kind: "set_window_minimized"; minimized: boolean }
  | { kind: "terminate_application" }
  | { kind: "focus_element"; element_ref: string }
  | { kind: "invoke_element"; element_ref: string }
  | { kind: "click_point"; point: ScreenshotPoint; button: PointerButton; count: number; delivery?: DeliveryPreference }
  | { kind: "press_point"; point: ScreenshotPoint; button: PointerButton; duration_ms: number }
  | { kind: "set_value"; element_ref: string; value: SensitiveText }
  | { kind: "set_range_value"; element_ref: string; value: number }
  | { kind: "toggle_element"; element_ref: string }
  | { kind: "select_element"; element_ref: string }
  | { kind: "set_expanded"; element_ref: string; expanded: boolean }
  | { kind: "move_pointer"; point: ScreenshotPoint; duration_ms: number }
  | { kind: "type_text"; text: SensitiveText }
  | { kind: "press_keys"; keys: string[] }
  | { kind: "scroll"; element_ref: string | null; delta_x: number; delta_y: number }
  | { kind: "drag"; from: ScreenshotPoint; to: ScreenshotPoint; duration_ms: number };

export type StatePredicate =
  | { kind: "window_title_contains"; text: string }
  | { kind: "element_exists"; role: string | null; name: string | null }
  | { kind: "element_state"; role: string | null; name: string | null; state: ElementState; value: boolean }
  | { kind: "bounds_contained"; inner: ScreenRect; outer: ScreenRect };

export interface ObserveDisplayInput { session_id: string; display_ref: string }
export interface ObserveWindowInput { session_id: string; window_ref: string; include_screenshot: boolean; accessibility: AccessibilityMode }
export interface PerformActionInput { session_id: string; window_ref: string; observation_id: string; action: Action }
export interface VerifyStateInput { session_id: string; window_ref: string; predicate: StatePredicate }

export class ActionRequest { readonly requestId: string; toString(): string }
export class LaunchRequest { readonly requestId: string; toString(): string }
export class ClipboardRequest { readonly requestId: string; toString(): string }
export class MutationIndeterminateError extends Error { readonly request: ActionRequest }
export class LaunchIndeterminateError extends Error { readonly request: LaunchRequest }
export class ClipboardIndeterminateError extends Error { readonly request: ClipboardRequest }

export class Client {
  constructor(config: Config);
  toString(): "Client([PRIVATE TRANSPORT])";
  getCapabilities(timeoutMs?: number): Promise<DriverCapabilities>;
  getPermissionStatus(timeoutMs?: number): Promise<PermissionStatus>;
  discoverApplications(timeoutMs?: number): Promise<DiscoverApplicationsOutput>;
  discoverLaunchableApplications(timeoutMs?: number): Promise<DiscoverLaunchableApplicationsOutput>;
  launchApplication(input: LaunchApplicationInput, timeoutMs?: number): Promise<{ output: ApplicationLaunchOutput; request: LaunchRequest }>;
  reconcileLaunch(request: LaunchRequest, timeoutMs: number): Promise<ApplicationLaunchOutput>;
  openSession(manifest: CapabilityManifest, timeoutMs?: number): Promise<OpenSessionOutput>;
  closeSession(sessionId: string, timeoutMs?: number): Promise<void>;
  listApps(sessionId: string, timeoutMs?: number): Promise<ApplicationSummary[]>;
  listWindows(sessionId: string, appRef?: string | null, timeoutMs?: number): Promise<WindowSummary[]>;
  listDisplays(sessionId: string, timeoutMs?: number): Promise<DisplaySummary[]>;
  observeDisplay(input: ObserveDisplayInput, timeoutMs?: number): Promise<DisplayObservation>;
  observeWindow(input: ObserveWindowInput, timeoutMs?: number): Promise<WindowObservation>;
  readClipboard(sessionId: string, timeoutMs?: number): Promise<ClipboardSnapshot>;
  writeClipboard(input: ClipboardWriteInput, timeoutMs?: number): Promise<{ output: ClipboardWriteOutput; request: ClipboardRequest }>;
  reconcileClipboard(request: ClipboardRequest, timeoutMs: number): Promise<ClipboardWriteOutput>;
  performAction(input: PerformActionInput, timeoutMs?: number): Promise<{ output: ActionOutput; request: ActionRequest }>;
  reconcileAction(request: ActionRequest, timeoutMs: number): Promise<ActionOutput>;
  verifyState(input: VerifyStateInput, timeoutMs?: number): Promise<VerificationOutput>;
  reconcileMutation(requestId: string, waitMs?: number, timeoutMs?: number): Promise<MutationReconciliation>;
}

export function selectApplication(applications: DiscoveredApplication[], selector: string): DiscoveredApplication;
export function selectWindow(windows: WindowSummary[], selector: string): WindowSummary;
