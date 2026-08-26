//! Internal errors projected to stable public failures.

use nexus_cua_protocol::{CuaError, ErrorCode, MutationStatus};
use thiserror::Error;

/// Stable driver failure category hidden behind the public error model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverErrorKind {
    /// A bounded native actor queue cannot admit more work.
    Busy,
    /// Required operating-system permission is missing.
    PermissionRequired,
    /// Target or action is not supported.
    Unsupported,
    /// Target disappeared or can no longer be addressed.
    TargetUnavailable,
    /// Native target stopped answering within a bounded provider timeout.
    TargetUnresponsive,
    /// Action requires foreground delivery.
    ForegroundRequired,
    /// Observation no longer matches platform state.
    StaleObservation,
    /// Platform operation failed.
    Platform,
}

/// Driver error whose message is safe for a local product surface.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct DriverError {
    /// Stable category.
    pub kind: DriverErrorKind,
    /// Non-sensitive explanation.
    pub message: String,
    /// Whether fresh state or a later retry may succeed.
    pub retryable: bool,
    /// Stable recovery hint.
    pub recovery_action: Option<String>,
    /// Mutation disposition known by the native driver.
    pub mutation_status: MutationStatus,
}

impl DriverError {
    /// Constructs a driver error without platform handles or user content.
    pub fn new(kind: DriverErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable: false,
            recovery_action: None,
            mutation_status: MutationStatus::NotApplicable,
        }
    }

    /// Marks the failure as retryable after the supplied recovery action.
    #[must_use]
    pub fn retryable(mut self, recovery_action: impl Into<String>) -> Self {
        self.retryable = true;
        self.recovery_action = Some(recovery_action.into());
        self
    }

    /// Marks an action failure as known to have happened before dispatch.
    #[must_use]
    pub fn mutation_not_dispatched(mut self) -> Self {
        self.mutation_status = MutationStatus::NotDispatched;
        self
    }

    /// Marks an action failure as potentially occurring after dispatch.
    #[must_use]
    pub fn mutation_indeterminate(mut self) -> Self {
        self.mutation_status = MutationStatus::Indeterminate;
        self
    }
}

impl From<DriverError> for CuaError {
    fn from(value: DriverError) -> Self {
        let code = match value.kind {
            DriverErrorKind::Busy => ErrorCode::Busy,
            DriverErrorKind::PermissionRequired => ErrorCode::PermissionRequired,
            DriverErrorKind::Unsupported => ErrorCode::Unsupported,
            DriverErrorKind::TargetUnavailable => ErrorCode::TargetUnavailable,
            DriverErrorKind::TargetUnresponsive => ErrorCode::TargetUnresponsive,
            DriverErrorKind::ForegroundRequired => ErrorCode::ForegroundRequired,
            DriverErrorKind::StaleObservation => ErrorCode::StaleObservation,
            DriverErrorKind::Platform => ErrorCode::DriverFailure,
        };
        Self {
            code,
            message: value.message,
            retryable: value.retryable,
            recovery_action: value.recovery_action,
            mutation_status: value.mutation_status,
        }
    }
}

/// Constructs one public runtime error.
pub(crate) fn public_error(
    code: ErrorCode,
    message: impl Into<String>,
    retryable: bool,
    recovery_action: Option<&str>,
) -> CuaError {
    CuaError {
        code,
        message: message.into(),
        retryable,
        recovery_action: recovery_action.map(str::to_owned),
        mutation_status: MutationStatus::NotApplicable,
    }
}
