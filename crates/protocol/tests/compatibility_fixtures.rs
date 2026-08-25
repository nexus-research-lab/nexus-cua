//! Exhaustive compatibility fixture inventory for the development v1 protocol.

use std::collections::BTreeSet;

use nexus_cua_protocol::{Command, CommandResult, CuaError, PROTOCOL_VERSION, RequestEnvelope};

const REQUESTS: &str = include_str!("../../../fixtures/compatibility/nexus.cua.v1/requests.json");
const RESULTS: &str = include_str!("../../../fixtures/compatibility/nexus.cua.v1/results.json");
const ERRORS: &str = include_str!("../../../fixtures/compatibility/nexus.cua.v1/errors.json");
const UNKNOWN_FIELD: &str =
    include_str!("../../../fixtures/compatibility/nexus.cua.v1/invalid/unknown-request-field.json");
const UNKNOWN_COMMAND: &str = include_str!(
    "../../../fixtures/compatibility/nexus.cua.v1/invalid/unknown-command-variant.json"
);
const UNKNOWN_RESULT: &str = include_str!(
    "../../../fixtures/compatibility/nexus.cua.v1/invalid/unknown-result-variant.json"
);
const PROTOCOL_MISMATCH: &str =
    include_str!("../../../fixtures/compatibility/nexus.cua.v1/protocol-mismatch-request.json");

#[test]
fn request_fixtures_cover_every_command() {
    let requests: Vec<RequestEnvelope> = serde_json::from_str(REQUESTS).expect("decode requests");
    let actual = requests
        .iter()
        .map(|request| command_name(&request.command))
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([
        "close_session",
        "discover_applications",
        "get_capabilities",
        "get_permission_status",
        "list_apps",
        "list_windows",
        "observe_window",
        "open_session",
        "perform_action",
        "verify_state",
    ]);
    assert_eq!(actual, expected);
    assert!(
        requests
            .iter()
            .all(|request| request.protocol_version == PROTOCOL_VERSION)
    );
}

#[test]
fn result_fixtures_cover_every_success_variant() {
    let results: Vec<CommandResult> = serde_json::from_str(RESULTS).expect("decode results");
    let actual = results.iter().map(result_name).collect::<BTreeSet<_>>();
    let expected = BTreeSet::from([
        "acknowledged",
        "action_performed",
        "applications_discovered",
        "apps",
        "capabilities",
        "permission_status",
        "session_opened",
        "state_verified",
        "window_observed",
        "windows",
    ]);
    assert_eq!(actual, expected);
}

#[test]
fn error_fixtures_cover_every_stable_code() {
    let errors: Vec<CuaError> = serde_json::from_str(ERRORS).expect("decode errors");
    let actual = errors
        .iter()
        .map(|error| serde_json::to_string(&error.code).expect("encode code"))
        .collect::<BTreeSet<_>>();
    let expected = [
        "busy",
        "capability_denied",
        "deadline_exceeded",
        "driver_failure",
        "foreground_required",
        "internal",
        "invalid_request",
        "permission_required",
        "protocol_mismatch",
        "reference_not_found",
        "session_unavailable",
        "stale_discovery",
        "stale_observation",
        "target_unavailable",
        "unauthorized",
        "unsupported",
    ]
    .into_iter()
    .map(|code| format!("\"{code}\""))
    .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn invalid_fixtures_fail_closed_and_mismatch_remains_decodable() {
    assert!(serde_json::from_str::<RequestEnvelope>(UNKNOWN_FIELD).is_err());
    assert!(serde_json::from_str::<RequestEnvelope>(UNKNOWN_COMMAND).is_err());
    assert!(serde_json::from_str::<CommandResult>(UNKNOWN_RESULT).is_err());
    let mismatch: RequestEnvelope =
        serde_json::from_str(PROTOCOL_MISMATCH).expect("mismatch envelope remains readable");
    assert_ne!(mismatch.protocol_version, PROTOCOL_VERSION);
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::GetCapabilities => "get_capabilities",
        Command::GetPermissionStatus => "get_permission_status",
        Command::DiscoverApplications => "discover_applications",
        Command::OpenSession(_) => "open_session",
        Command::CloseSession(_) => "close_session",
        Command::ListApps(_) => "list_apps",
        Command::ListWindows(_) => "list_windows",
        Command::ObserveWindow(_) => "observe_window",
        Command::PerformAction(_) => "perform_action",
        Command::VerifyState(_) => "verify_state",
    }
}

fn result_name(result: &CommandResult) -> &'static str {
    match result {
        CommandResult::Capabilities(_) => "capabilities",
        CommandResult::PermissionStatus(_) => "permission_status",
        CommandResult::ApplicationsDiscovered(_) => "applications_discovered",
        CommandResult::SessionOpened(_) => "session_opened",
        CommandResult::Acknowledged => "acknowledged",
        CommandResult::Apps(_) => "apps",
        CommandResult::Windows(_) => "windows",
        CommandResult::WindowObserved(_) => "window_observed",
        CommandResult::ActionPerformed(_) => "action_performed",
        CommandResult::StateVerified(_) => "state_verified",
    }
}
