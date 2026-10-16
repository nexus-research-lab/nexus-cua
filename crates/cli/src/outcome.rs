//! Stable, payload-free process failure contract.

use nexus_cua_protocol::MutationStatus;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// A failed operation whose full response remains on stdout.
#[derive(Debug, Error)]
#[error("operation failed; inspect the structured response")]
pub(crate) struct OperationFailure {
    pub(crate) mutation_status: Option<MutationStatus>,
}

/// Check an MCP tool result after writing it, without retaining its contents.
pub(crate) fn check_tool_result(value: &Value) -> Result<(), OperationFailure> {
    if value.get("content").is_some_and(Value::is_array)
        && (value.get("isError") == Some(&Value::Bool(false)) || value.get("isError").is_none())
    {
        return Ok(());
    }
    Err(OperationFailure {
        mutation_status: value
            .get("structuredContent")
            .and_then(|value| value.get("mutation_status"))
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
    })
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureCode {
    InvalidInput,
    AuthorityFailed,
    StateConflict,
    ServiceUnavailable,
    CommunicationFailed,
    OperationFailed,
    InternalError,
}

impl FailureCode {
    pub(crate) const fn exit_code(self) -> u8 {
        match self {
            Self::InvalidInput => 2,
            Self::AuthorityFailed => 3,
            Self::StateConflict => 4,
            Self::ServiceUnavailable => 5,
            Self::CommunicationFailed => 6,
            Self::OperationFailed => 7,
            Self::InternalError => 1,
        }
    }
}

/// Null disposition means no proof is available, never permission to replay.
#[derive(Debug, Serialize)]
pub(crate) struct FailureReport {
    schema: &'static str,
    code: FailureCode,
    exit_code: u8,
    mutation_status: Option<MutationStatus>,
    recovery: &'static str,
}

impl FailureReport {
    pub(crate) const fn new(code: FailureCode, status: Option<MutationStatus>) -> Self {
        Self {
            schema: "nexus.cua.cli-error.v1",
            code,
            exit_code: code.exit_code(),
            mutation_status: status,
            recovery: match status {
                None | Some(MutationStatus::Indeterminate) => {
                    "For a submitted mutation, reconcile the same request_id; do not resubmit."
                }
                Some(MutationStatus::NotDispatched | MutationStatus::NotApplicable) => {
                    "Inspect the response and current authority/state before another operation."
                }
            },
        }
    }

    pub(crate) const fn exit_code(&self) -> u8 {
        self.exit_code
    }

    pub(crate) fn emit(&self) {
        // Only closed fields are serialized. Never render source error strings.
        if let Ok(json) = serde_json::to_string(self) {
            eprintln!("{json}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failed_tool_retains_disposition_without_copying_sensitive_content() {
        let value = json!({"isError": true, "content": [], "structuredContent": {
            "mutation_status": "indeterminate", "message": "secret-input"
        }});
        let failure = check_tool_result(&value).unwrap_err();
        let report = FailureReport::new(FailureCode::OperationFailed, failure.mutation_status);
        let encoded = serde_json::to_string(&report).unwrap();
        assert_eq!(report.exit_code(), 7);
        assert!(encoded.contains("indeterminate"));
        assert!(encoded.contains("same request_id"));
        assert!(!encoded.contains("secret-input"));
    }

    #[test]
    fn malformed_or_missing_disposition_never_claims_not_dispatched() {
        for value in [
            json!({}),
            json!({"isError": true}),
            json!({
                "isError": true, "structuredContent": {"mutation_status": "invented"}
            }),
        ] {
            assert!(
                check_tool_result(&value)
                    .unwrap_err()
                    .mutation_status
                    .is_none()
            );
        }
        assert!(check_tool_result(&json!({"content": []})).is_ok());
        assert!(check_tool_result(&json!({"isError": false, "content": []})).is_ok());
    }
}
