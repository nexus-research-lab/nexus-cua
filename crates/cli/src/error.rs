//! CLI error boundary.

use thiserror::Error;

/// Human-readable terminal failure without sensitive command payloads.
#[derive(Debug, Error)]
pub enum CliError {
    /// An operation returned failure; its complete response is on stdout.
    #[error(transparent)]
    Operation(#[from] crate::outcome::OperationFailure),
    /// Agent-facing CLI binding failed closed.
    #[error(transparent)]
    Agent(#[from] crate::agent::AgentError),
    /// CLI arguments form an invalid configuration.
    #[error("invalid configuration: {0}")]
    InvalidConfiguration(String),
    /// Local filesystem or terminal I/O failed.
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A JSON diagnostic or command payload is invalid.
    #[error("JSON processing failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Native platform driver setup failed.
    #[error("native driver setup failed: {0}")]
    Driver(#[from] nexus_cua_runtime::DriverError),
    /// Runtime setup rejected the local configuration.
    #[error("runtime setup failed: {0:?}")]
    Runtime(nexus_cua_protocol::CuaError),
    /// Immutable runtime policy setup failed.
    #[error(transparent)]
    Policy(#[from] nexus_cua_runtime::PolicyLoadError),
    /// Public MCP adapter setup or transport failed.
    #[error(transparent)]
    Mcp(#[from] nexus_cua_mcp::McpServeError),
    /// Registered authority provider setup or authentication failed.
    #[error(transparent)]
    Provider(#[from] nexus_cua_provider::RegistrationError),
    /// Host integration planning, application, or removal failed closed.
    #[error(transparent)]
    Integration(#[from] crate::integration::IntegrationError),
    /// Human authority planning, application, inspection, or revocation failed closed.
    #[error(transparent)]
    Authority(#[from] crate::authority::AuthorityCliError),
    /// Registered internal CLI service lifecycle failed closed.
    #[error(transparent)]
    Service(#[from] crate::service::ServiceError),
    /// Private local IPC failed.
    #[error(transparent)]
    Transport(#[from] nexus_cua_transport::TransportError),
    /// Structured terminal logging could not be installed.
    #[error("logging setup failed: {0}")]
    Logging(String),
}

impl CliError {
    pub(crate) fn report(&self) -> crate::outcome::FailureReport {
        use crate::agent::AgentError;
        use crate::outcome::{FailureCode as Code, FailureReport};
        let code = match self {
            Self::Operation(error) | Self::Agent(AgentError::Operation(error)) => {
                return FailureReport::new(Code::OperationFailed, error.mutation_status);
            }
            Self::Runtime(error) => {
                return FailureReport::new(Code::OperationFailed, Some(error.mutation_status));
            }
            Self::Json(error) | Self::Agent(AgentError::Json(error)) => json_code(error),
            Self::InvalidConfiguration(_) | Self::Agent(AgentError::Invalid(_)) => {
                Code::InvalidInput
            }
            Self::Provider(_) | Self::Policy(_) => Code::AuthorityFailed,
            Self::Service(error) | Self::Agent(AgentError::Service(error)) => service_code(error),
            Self::Authority(error) => authority_code(error),
            Self::Integration(error) => integration_code(error),
            Self::Transport(_) | Self::Io(_) | Self::Agent(AgentError::Io(_)) => {
                Code::CommunicationFailed
            }
            Self::Mcp(error) => return mcp_report(error),
            Self::Driver(_) | Self::Logging(_) => Code::InternalError,
        };
        FailureReport::new(code, None)
    }
}

fn json_code(error: &serde_json::Error) -> crate::outcome::FailureCode {
    if error.is_io() {
        crate::outcome::FailureCode::CommunicationFailed
    } else {
        crate::outcome::FailureCode::InvalidInput
    }
}

fn mcp_report(error: &nexus_cua_mcp::McpServeError) -> crate::outcome::FailureReport {
    use crate::outcome::{FailureCode as Code, FailureReport};
    use nexus_cua_mcp::McpServeError as Error;
    let code = match error {
        Error::InvalidConfiguration(_) => Code::InvalidInput,
        Error::Authority(_) | Error::Policy(_) | Error::Bootstrap(_) => Code::AuthorityFailed,
        Error::Transport(_) | Error::Http(_) => Code::CommunicationFailed,
        Error::Driver(_) => Code::InternalError,
        Error::Runtime(error) => {
            return FailureReport::new(Code::OperationFailed, Some(error.mutation_status));
        }
    };
    FailureReport::new(code, None)
}

fn service_code(error: &crate::service::ServiceError) -> crate::outcome::FailureCode {
    use crate::outcome::FailureCode as Code;
    use crate::service::ServiceError as Error;
    match error {
        Error::Invalid(_) => Code::InvalidInput,
        Error::NotRunning => Code::ServiceUnavailable,
        Error::AlreadyRunning | Error::StateDrift => Code::StateConflict,
        Error::Provider(_) | Error::HttpStatus(401 | 403) => Code::AuthorityFailed,
        Error::StopTimeout
        | Error::InvalidResponse
        | Error::ResponseTooLarge
        | Error::HttpStatus(_)
        | Error::Io(_)
        | Error::Json(_) => Code::CommunicationFailed,
        Error::Mcp(_) | Error::Clock(_) => Code::InternalError,
    }
}

fn authority_code(error: &crate::authority::AuthorityCliError) -> crate::outcome::FailureCode {
    use crate::authority::AuthorityCliError as Error;
    use crate::outcome::FailureCode as Code;
    match error {
        Error::Invalid(_) | Error::Json(_) => Code::InvalidInput,
        Error::StalePlan | Error::StateDrift | Error::RollbackIncomplete => Code::StateConflict,
        Error::ConfirmationRequired
        | Error::Provider(_)
        | Error::Policy(_)
        | Error::HostIdentity(_) => Code::AuthorityFailed,
        Error::Io(_) => Code::CommunicationFailed,
        Error::Service(error) => service_code(error),
    }
}

fn integration_code(error: &crate::integration::IntegrationError) -> crate::outcome::FailureCode {
    use crate::integration::IntegrationError as Error;
    use crate::outcome::FailureCode as Code;
    match error {
        Error::Invalid(_) | Error::Json(_) => Code::InvalidInput,
        Error::StalePlan
        | Error::AliasConflict
        | Error::SkillConflict
        | Error::StateDrift
        | Error::RollbackIncomplete => Code::StateConflict,
        Error::ConfirmationRequired
        | Error::Provider(_)
        | Error::Authority(_)
        | Error::HostIdentity(_) => Code::AuthorityFailed,
        Error::HostNotFound => Code::ServiceUnavailable,
        Error::HostCommand | Error::HostOutput | Error::Io(_) => Code::CommunicationFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_cua_protocol::{CuaError, ErrorCode, MutationStatus};

    #[test]
    fn communication_failure_does_not_claim_a_mutation_was_rejected() {
        let error = CliError::Agent(crate::agent::AgentError::Service(
            crate::service::ServiceError::InvalidResponse,
        ));
        let value = serde_json::to_value(error.report()).unwrap();
        assert_eq!(value["exit_code"], 6);
        assert!(value["mutation_status"].is_null());
    }

    #[test]
    fn runtime_disposition_survives_error_boundary_but_messages_do_not() {
        for status in [MutationStatus::NotDispatched, MutationStatus::Indeterminate] {
            let error = CliError::Runtime(CuaError {
                code: ErrorCode::DeadlineExceeded,
                message: "sensitive-input".to_owned(),
                retryable: true,
                recovery_action: Some("sensitive-path".to_owned()),
                mutation_status: status,
            });
            let value = serde_json::to_value(error.report()).unwrap();
            assert_eq!(value["exit_code"], 7);
            assert_eq!(
                value["mutation_status"],
                serde_json::to_value(status).unwrap()
            );
            assert!(!value.to_string().contains("sensitive"));
        }
    }

    #[test]
    fn nested_authority_and_service_failures_have_distinct_exit_codes() {
        let authority =
            CliError::Authority(crate::authority::AuthorityCliError::ConfirmationRequired);
        let stopped = CliError::Agent(crate::agent::AgentError::Service(
            crate::service::ServiceError::NotRunning,
        ));
        let conflict = CliError::Integration(crate::integration::IntegrationError::StateDrift);
        assert_eq!(authority.report().exit_code(), 3);
        assert_eq!(stopped.report().exit_code(), 5);
        assert_eq!(conflict.report().exit_code(), 4);
    }
}
