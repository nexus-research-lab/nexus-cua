//! Closed request and response envelopes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    Action, ApplicationSummary, AuthorizationToken, CuaError, DeliveryMode, DriverCapabilities,
    ListWindowsInput, ObservationId, OpenSessionInput, OpenSessionOutput, PermissionStatus,
    RequestId, SessionId, SessionInput, StatePredicate, WindowObservation, WindowRef,
    WindowSummary,
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
#[serde(tag = "operation", content = "input", rename_all = "snake_case")]
pub enum Command {
    /// Read driver and protocol capabilities.
    GetCapabilities,
    /// Read current system permission state.
    GetPermissionStatus,
    /// Open one isolated authorized session.
    OpenSession(OpenSessionInput),
    /// Close a session and delete its transient artifacts.
    CloseSession(SessionInput),
    /// List allowed running applications.
    ListApps(SessionInput),
    /// List allowed top-level windows.
    ListWindows(ListWindowsInput),
    /// Capture one exact window.
    ObserveWindow(crate::ObserveWindowInput),
    /// Execute one mutation bound to a fresh observation.
    PerformAction(PerformActionInput),
    /// Evaluate a deterministic predicate against fresh state.
    VerifyState(VerifyStateInput),
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

/// Successful result for one command.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(tag = "result_type", content = "data", rename_all = "snake_case")]
pub enum CommandResult {
    /// Driver capability response.
    Capabilities(DriverCapabilities),
    /// Operating-system permission response.
    PermissionStatus(PermissionStatus),
    /// Session creation response.
    SessionOpened(OpenSessionOutput),
    /// Empty successful result.
    Acknowledged,
    /// Application enumeration response.
    Apps(Vec<ApplicationSummary>),
    /// Window enumeration response.
    Windows(Vec<WindowSummary>),
    /// Immutable observation response.
    WindowObserved(Box<WindowObservation>),
    /// Mutation response.
    ActionPerformed(ActionOutput),
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
#[serde(tag = "status", rename_all = "snake_case")]
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
