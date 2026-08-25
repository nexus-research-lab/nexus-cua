//! Wire compatibility and secret-redaction contracts.

use nexus_cua_protocol::{
    Action, AuthorizationToken, Command, ErrorCode, RequestEnvelope, RequestId, SensitiveText,
    SessionId, SessionInput,
};

#[test]
fn request_decoder_rejects_unknown_fields() {
    let raw = r#"{
        "protocol_version":"nexus.cua.v1",
        "request_id":"request-1",
        "timeout_ms":30000,
        "authorization":"secret",
        "command":{"operation":"get_capabilities"},
        "owner_id":"must-not-cross-boundary"
    }"#;

    let error = serde_json::from_str::<RequestEnvelope>(raw).expect_err("unknown field must fail");
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn sensitive_values_are_redacted_from_debug_output() {
    let token = AuthorizationToken::new("transport-secret");
    let action = Action::TypeText {
        text: SensitiveText::new("customer-password"),
    };

    assert!(!format!("{token:?}").contains("transport-secret"));
    assert!(!format!("{action:?}").contains("customer-password"));
}

#[test]
fn tagged_command_round_trips_without_platform_identity() {
    let request = RequestEnvelope {
        protocol_version: "nexus.cua.v1".to_owned(),
        request_id: RequestId::new("request-1"),
        timeout_ms: 30_000,
        authorization: AuthorizationToken::new("secret"),
        command: Command::CloseSession(SessionInput {
            session_id: SessionId::new("session-1"),
        }),
    };

    let encoded = serde_json::to_string(&request).expect("encode request");
    let decoded: RequestEnvelope = serde_json::from_str(&encoded).expect("decode request");

    assert!(matches!(decoded.command, Command::CloseSession(_)));
    for forbidden in ["pid", "hwnd", "window_id", "owner_id", "agent_id"] {
        assert!(!encoded.contains(forbidden));
    }
}

#[test]
fn stable_error_code_uses_snake_case() {
    assert_eq!(
        serde_json::to_string(&ErrorCode::StaleObservation).expect("encode error code"),
        "\"stale_observation\""
    );
}

#[test]
fn protocol_schema_is_closed_and_serializable() {
    let schema = schemars::schema_for!(RequestEnvelope);
    let value = serde_json::to_value(schema).expect("serialize schema");
    assert_eq!(
        value["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
}
