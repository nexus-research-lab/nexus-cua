//! Stable, recoverable public errors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable machine-readable failure category.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request used a different protocol version.
    ProtocolMismatch,
    /// Transport token is missing or invalid.
    Unauthorized,
    /// Request body or operation input is invalid.
    InvalidRequest,
    /// A bounded native actor queue could not admit the request.
    Busy,
    /// The caller deadline elapsed; an admitted side effect may still finish.
    DeadlineExceeded,
    /// Session does not exist, expired, or is already closed.
    SessionUnavailable,
    /// Discovery reference expired or no longer identifies the same process.
    StaleDiscovery,
    /// Capability manifest does not authorize the operation.
    CapabilityDenied,
    /// An opaque reference is unknown in the selected session.
    ReferenceNotFound,
    /// Observation expired, was invalidated, or no longer matches platform state.
    StaleObservation,
    /// Required operating-system permission is missing.
    PermissionRequired,
    /// Driver cannot perform the operation on this platform or target.
    Unsupported,
    /// Target requires foreground input not authorized by the manifest.
    ForegroundRequired,
    /// Target application or window disappeared.
    TargetUnavailable,
    /// Native target stopped answering within the bounded provider timeout.
    TargetUnresponsive,
    /// Platform driver failed without exposing sensitive implementation details.
    DriverFailure,
    /// Internal invariant failed.
    Internal,
}

/// Mutation disposition attached to every error response.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationStatus {
    /// The failed request was not a mutation.
    #[default]
    NotApplicable,
    /// The runtime proved that no target mutation was dispatched.
    NotDispatched,
    /// Dispatch may have occurred; only same-request reconciliation is safe.
    Indeterminate,
}

/// Public error envelope without platform handles or sensitive payloads.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CuaError {
    /// Stable failure category.
    pub code: ErrorCode,
    /// Concise user-safe explanation.
    pub message: String,
    /// Whether retrying after a fresh observation or state change may succeed.
    pub retryable: bool,
    /// Stable recovery hint for agent or product routing.
    pub recovery_action: Option<String>,
    /// Whether a failed request could have reached the mutation target.
    pub mutation_status: MutationStatus,
}

impl CuaError {
    /// Attaches the mutation disposition known at the failure boundary.
    #[must_use]
    pub fn with_mutation_status(mut self, status: MutationStatus) -> Self {
        self.mutation_status = status;
        self
    }
}
