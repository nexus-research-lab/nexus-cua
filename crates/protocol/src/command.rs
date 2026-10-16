//! Closed request and response envelopes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Action, ActionEffectStatus, ActionEvidenceKind, ApplicationLaunchOutput, ApplicationSummary,
    AuthorizationToken, ClipboardSnapshot, ClipboardWriteOutput, CuaError, DeliveryMode,
    DiscoverApplicationsOutput, DiscoverLaunchableApplicationsOutput, DisplayObservation,
    DisplaySummary, DriverCapabilities, ErrorCode, ForegroundRestorationStatus,
    LaunchApplicationInput, ListWindowsInput, ObservationId, OpenSessionInput, OpenSessionOutput,
    PermissionStatus, ReadClipboardInput, RequestId, SessionId, SessionInput, StatePredicate,
    WindowObservation, WindowRef, WindowSummary, WriteClipboardInput,
};

/// Authenticated request sent over a private local transport.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    /// Must equal [`crate::PROTOCOL_VERSION`].
    pub protocol_version: String,
    /// Client-generated request identity.
    pub request_id: RequestId,
    /// End-to-end caller deadline in milliseconds, bounded by the service.
    pub timeout_ms: u32,
    /// Host-issued transport token.
    pub authorization: AuthorizationToken,
    /// Exact requested operation.
    pub command: Command,
}

/// Exact public operation. Unknown variants or fields are rejected.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(
    tag = "operation",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Command {
    /// Read driver and protocol capabilities.
    GetCapabilities,
    /// Read current system permission state.
    GetPermissionStatus,
    /// Discover running applications for an authenticated policy host.
    DiscoverApplications,
    /// Discover installed applications through bounded native registration sources.
    DiscoverLaunchableApplications,
    /// Launch one freshly discovered installed application identity.
    LaunchApplication(LaunchApplicationInput),
    /// Open one isolated authorized session.
    OpenSession(OpenSessionInput),
    /// Close a session and delete its transient artifacts.
    CloseSession(SessionInput),
    /// List allowed running applications.
    ListApps(SessionInput),
    /// List allowed top-level windows.
    ListWindows(ListWindowsInput),
    /// List exact physical displays under separate read-only authority.
    ListDisplays(SessionInput),
    /// Capture one exact window.
    ObserveWindow(crate::ObserveWindowInput),
    /// Capture one exact physical display for desktop overview.
    ObserveDisplay(crate::ObserveDisplayInput),
    /// Read bounded plain text under separate clipboard authority.
    ReadClipboard(ReadClipboardInput),
    /// Replace plain text only when a fresh clipboard guard still matches.
    WriteClipboard(WriteClipboardInput),
    /// Execute one mutation bound to a fresh observation.
    PerformAction(PerformActionInput),
    /// Query one prior mutation identity without dispatching it again.
    ReconcileMutation(ReconcileMutationInput),
    /// Evaluate a deterministic predicate against fresh state.
    VerifyState(VerifyStateInput),
}

/// Read-only reconciliation query for one prior mutation identity.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileMutationInput {
    /// Original host-generated mutation identity.
    pub request_id: RequestId,
    /// Maximum time to wait for an already-admitted mutation to finish.
    pub wait_ms: u32,
}

/// Mutation input bound to exact session, window, and observation identities.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PerformActionInput {
    /// Runtime-issued session identity.
    pub session_id: SessionId,
    /// Session-scoped target window.
    pub window_ref: WindowRef,
    /// Fresh observation guarding the action.
    pub observation_id: ObservationId,
    /// Authorized action.
    pub action: Action,
}

/// Deterministic verification input.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyStateInput {
    /// Runtime-issued session identity.
    pub session_id: SessionId,
    /// Session-scoped target window.
    pub window_ref: WindowRef,
    /// Predicate evaluated against fresh state.
    pub predicate: StatePredicate,
}

/// Successful mutation result.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionOutput {
    /// Platform delivery route actually used.
    pub delivery_mode: DeliveryMode,
    /// Whether the platform reported successful dispatch.
    pub dispatched: bool,
    /// Truthful effect classification for the direct requested mutation.
    pub effect_status: ActionEffectStatus,
    /// Evidence class supporting `effect_status`.
    pub evidence_kind: ActionEvidenceKind,
    /// Truthful outcome of any host-authorized foreground restoration attempt.
    pub foreground_restoration: ForegroundRestorationStatus,
    /// Whether callers must observe again before another mutation.
    pub observation_invalidated: bool,
}

/// Deterministic verification result.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationOutput {
    /// Whether the predicate matched.
    pub matched: bool,
    /// Concise non-sensitive evidence.
    pub evidence: String,
}

/// Whether a mutation identity is known to the current runtime epoch.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationAdmission {
    /// No retained ledger entry exists in this runtime epoch.
    Unknown,
    /// The dispatcher admitted the original command in this runtime epoch.
    Admitted,
}

/// Read-only disposition of one retained mutation identity.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationDisposition {
    /// No retained ledger entry exists in this runtime epoch.
    Unknown,
    /// The identity belongs to a read-only command, not a mutation.
    NotMutation,
    /// The mutation remains admitted and may still complete.
    Pending,
    /// The mutation completed successfully.
    Succeeded,
    /// The runtime proved that the target mutation was not dispatched.
    NotDispatched,
    /// Dispatch may have occurred and the direct effect cannot be proven here.
    Indeterminate,
}

/// Bounded, read-only evidence retained for mutation reconciliation.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MutationReconciliation {
    /// Original host-generated mutation identity.
    pub request_id: RequestId,
    /// Runtime generation that answered this query.
    pub runtime_epoch: String,
    /// Remote worker generation, or `null` for an embedded/local runtime.
    pub worker_epoch: Option<String>,
    /// Whether this runtime epoch admitted the original mutation.
    pub admission: MutationAdmission,
    /// Current closed mutation disposition.
    pub disposition: MutationDisposition,
    /// Whether the native provider accepted delivery, when known.
    pub dispatched: Option<bool>,
    /// Direct-effect status retained from a successful mutation, when available.
    pub effect_status: Option<ActionEffectStatus>,
    /// Evidence class retained from a successful mutation, when available.
    pub evidence_kind: Option<ActionEvidenceKind>,
    /// Stable terminal error category, when reconciliation found a failure.
    pub error_code: Option<ErrorCode>,
    /// Whether the caller must establish a fresh observation before another mutation.
    pub fresh_observation_required: bool,
}

/// Successful result for one command.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(
    tag = "result_type",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CommandResult {
    /// Driver capability response.
    Capabilities(DriverCapabilities),
    /// Operating-system permission response.
    PermissionStatus(PermissionStatus),
    /// Short-lived running-application discovery response.
    ApplicationsDiscovered(DiscoverApplicationsOutput),
    /// Short-lived installed-application discovery response.
    LaunchableApplicationsDiscovered(DiscoverLaunchableApplicationsOutput),
    /// Newly launched running process generation.
    ApplicationLaunched(ApplicationLaunchOutput),
    /// Session creation response.
    SessionOpened(OpenSessionOutput),
    /// Empty successful result.
    Acknowledged,
    /// Application enumeration response.
    Apps(Vec<ApplicationSummary>),
    /// Window enumeration response.
    Windows(Vec<WindowSummary>),
    /// Display enumeration response.
    Displays(Vec<DisplaySummary>),
    /// Immutable observation response.
    WindowObserved(Box<WindowObservation>),
    /// Immutable read-only display observation response.
    DisplayObserved(Box<DisplayObservation>),
    /// Transient bounded clipboard text and a fresh write guard.
    ClipboardRead(ClipboardSnapshot),
    /// Guarded clipboard mutation response.
    ClipboardWritten(ClipboardWriteOutput),
    /// Mutation response.
    ActionPerformed(ActionOutput),
    /// Read-only mutation-ledger response.
    MutationReconciled(MutationReconciliation),
    /// Verification response.
    StateVerified(VerificationOutput),
}

/// Response paired with one request identity.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEnvelope {
    /// Stable protocol identifier.
    pub protocol_version: String,
    /// Echo of the request identity.
    pub request_id: RequestId,
    /// Success or public error outcome.
    pub outcome: ResponseOutcome,
}

/// Closed success/error outcome.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseOutcome {
    /// Successful command result.
    Success {
        /// Command-specific response data.
        result: CommandResult,
    },
    /// Stable public failure.
    Error {
        /// Stable public error without sensitive driver details.
        error: CuaError,
    },
}
