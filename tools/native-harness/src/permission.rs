//! Staged operating-system permission and protected-target validation.

use std::error::Error;
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use nexus_cua_protocol::{
    AccessibilityMode, Action, CapabilityManifest, Command, CommandResult, CuaError, ErrorCode,
    MutationStatus, ObservationId, OpenSessionInput, PerformActionInput, PermissionMode,
    PermissionState, PermissionStatus, Platform, PointerButton, SessionId, SessionInput, WindowRef,
    WindowSummary,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    Cli, Client, element_screenshot_point, list_windows, observe, select_application,
    select_window, unexpected,
};

#[derive(Debug, Args)]
pub(super) struct PermissionArgs {
    #[command(subcommand)]
    command: PermissionCommand,
}

#[derive(Debug, Subcommand)]
enum PermissionCommand {
    /// Validate one deliberately denied OS permission profile.
    Denied {
        #[arg(value_enum)]
        permission: PermissionKind,
    },
    /// Preserve live refs before an operator revokes one permission.
    RevokePrepare(RevocationArgs),
    /// Validate the next affected operation after an in-place revocation.
    RevokeVerify(RevocationArgs),
    /// Validate explicit failure against an operator-provisioned elevated target.
    Protected,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum PermissionKind {
    ScreenCapture,
    Accessibility,
    InputControl,
}

#[derive(Debug, Args)]
struct RevocationArgs {
    #[arg(value_enum)]
    permission: PermissionKind,
    /// Probe state shared while the same sidecar remains running.
    #[arg(long)]
    state_file: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RevocationState {
    contract_version: String,
    permission: PermissionKind,
    session_id: SessionId,
    window_ref: WindowRef,
    observation_id: ObservationId,
}

pub(super) async fn validate(
    cli: &Cli,
    client: &mut Client,
    args: &PermissionArgs,
) -> Result<(), Box<dyn Error>> {
    match &args.command {
        PermissionCommand::Denied { permission } => denied(cli, client, *permission).await,
        PermissionCommand::RevokePrepare(args) => revoke_prepare(cli, client, args).await,
        PermissionCommand::RevokeVerify(args) => revoke_verify(client, args).await,
        PermissionCommand::Protected => protected(cli, client).await,
    }
}

async fn denied(
    cli: &Cli,
    client: &mut Client,
    permission: PermissionKind,
) -> Result<(), Box<dyn Error>> {
    let (platform, status) = platform_and_permissions(client).await?;
    if platform == Platform::Windows {
        require_state(&status, permission, PermissionState::NotApplicable)?;
        print_report(json!({
            "status": "not_applicable",
            "mode": "denied",
            "permission": permission,
            "scenario_groups": {
                "permission_denied": "not_applicable",
                "permission_revoked": "not_applicable"
            },
            "permissions": status
        }))?;
        return Ok(());
    }
    if permission_state(&status, permission) == PermissionState::Granted {
        return Err(format!(
            "{permission:?} is granted; run this probe under the deliberately denied TCC profile"
        )
        .into());
    }

    let error = match permission {
        PermissionKind::ScreenCapture => expect_error(
            client.send_outcome(Command::DiscoverApplications).await?,
            "screen-capture denial discovery",
        )?,
        PermissionKind::Accessibility => {
            let target = open_target(cli, client, 120).await?;
            let outcome = client
                .send_outcome(Command::ObserveWindow(
                    nexus_cua_protocol::ObserveWindowInput {
                        session_id: target.session_id.clone(),
                        window_ref: target.window.window_ref.clone(),
                        include_screenshot: false,
                        accessibility: AccessibilityMode::Full,
                    },
                ))
                .await?;
            let close = close_target(client, target).await;
            let error = expect_error(outcome, "accessibility denial observation")?;
            close?;
            error
        }
        PermissionKind::InputControl => {
            let target = open_target(cli, client, 120).await?;
            let observation = observe(
                client,
                &target.session_id,
                &target.window,
                false,
                AccessibilityMode::Disabled,
            )
            .await?;
            let outcome = client
                .send_outcome(Command::PerformAction(PerformActionInput {
                    session_id: target.session_id.clone(),
                    window_ref: target.window.window_ref.clone(),
                    observation_id: observation.observation_id,
                    action: Action::FocusWindow,
                }))
                .await?;
            let close = close_target(client, target).await;
            let error = expect_error(outcome, "input-control denial action")?;
            close?;
            error
        }
    };
    let expected_mutation = if matches!(permission, PermissionKind::InputControl) {
        MutationStatus::NotDispatched
    } else {
        MutationStatus::NotApplicable
    };
    require_error(
        &error,
        ErrorCode::PermissionRequired,
        expected_mutation,
        "permission denial",
    )?;
    print_report(json!({
        "status": "passed",
        "mode": "denied",
        "permission": permission,
        "scenario_groups": { "permission_denied": "passed" },
        "permissions": status,
        "error": error
    }))
}

async fn revoke_prepare(
    cli: &Cli,
    client: &mut Client,
    args: &RevocationArgs,
) -> Result<(), Box<dyn Error>> {
    let (platform, status) = platform_and_permissions(client).await?;
    if platform != Platform::Macos {
        return Err("permission revocation is not an OS permission concept on Windows".into());
    }
    require_state(&status, args.permission, PermissionState::Granted)?;
    let target = open_target(cli, client, 600).await?;
    let observation = observe(
        client,
        &target.session_id,
        &target.window,
        matches!(args.permission, PermissionKind::ScreenCapture),
        if matches!(args.permission, PermissionKind::Accessibility) {
            AccessibilityMode::Full
        } else {
            AccessibilityMode::Disabled
        },
    )
    .await?;
    let state = RevocationState {
        contract_version: "nexus.cua.permission-revocation.v1".to_owned(),
        permission: args.permission,
        session_id: target.session_id,
        window_ref: target.window.window_ref,
        observation_id: observation.observation_id,
    };
    if let Some(parent) = args.state_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.state_file, serde_json::to_vec_pretty(&state)?)?;
    print_report(json!({
        "status": "prepared",
        "mode": "revoke_prepare",
        "permission": args.permission,
        "state_file": args.state_file,
        "operator_action": "revoke the selected permission without restarting the sidecar, then run revoke-verify"
    }))
}

async fn revoke_verify(client: &mut Client, args: &RevocationArgs) -> Result<(), Box<dyn Error>> {
    let state: RevocationState = serde_json::from_slice(&std::fs::read(&args.state_file)?)?;
    if state.contract_version != "nexus.cua.permission-revocation.v1"
        || state.permission != args.permission
    {
        return Err("permission revocation state does not match this probe".into());
    }
    let (_, status) = platform_and_permissions(client).await?;
    if permission_state(&status, args.permission) == PermissionState::Granted {
        return Err("selected permission is still granted; revocation was not observed".into());
    }
    let outcome = match args.permission {
        PermissionKind::ScreenCapture => {
            client
                .send_outcome(Command::ObserveWindow(
                    nexus_cua_protocol::ObserveWindowInput {
                        session_id: state.session_id.clone(),
                        window_ref: state.window_ref.clone(),
                        include_screenshot: true,
                        accessibility: AccessibilityMode::Disabled,
                    },
                ))
                .await?
        }
        PermissionKind::Accessibility => {
            client
                .send_outcome(Command::ObserveWindow(
                    nexus_cua_protocol::ObserveWindowInput {
                        session_id: state.session_id.clone(),
                        window_ref: state.window_ref.clone(),
                        include_screenshot: false,
                        accessibility: AccessibilityMode::Full,
                    },
                ))
                .await?
        }
        PermissionKind::InputControl => {
            client
                .send_outcome(Command::PerformAction(PerformActionInput {
                    session_id: state.session_id.clone(),
                    window_ref: state.window_ref.clone(),
                    observation_id: state.observation_id,
                    action: Action::FocusWindow,
                }))
                .await?
        }
    };
    let error = expect_error(outcome, "permission revocation operation")?;
    let expected_mutation = if matches!(args.permission, PermissionKind::InputControl) {
        MutationStatus::NotDispatched
    } else {
        MutationStatus::NotApplicable
    };
    require_error(
        &error,
        ErrorCode::PermissionRequired,
        expected_mutation,
        "permission revocation",
    )?;
    client
        .send(Command::CloseSession(SessionInput {
            session_id: state.session_id,
        }))
        .await?;
    print_report(json!({
        "status": "passed",
        "mode": "revoke_verify",
        "permission": args.permission,
        "scenario_groups": { "permission_revoked": "passed" },
        "permissions": status,
        "error": error
    }))
}

async fn protected(cli: &Cli, client: &mut Client) -> Result<(), Box<dyn Error>> {
    let (platform, permissions) = platform_and_permissions(client).await?;
    if platform != Platform::Windows {
        print_report(json!({
            "status": "not_applicable",
            "mode": "protected",
            "scenario_groups": { "elevated_or_protected_target": "not_applicable" }
        }))?;
        return Ok(());
    }
    let target = open_target(cli, client, 120).await?;
    let observed = client
        .send_outcome(Command::ObserveWindow(
            nexus_cua_protocol::ObserveWindowInput {
                session_id: target.session_id.clone(),
                window_ref: target.window.window_ref.clone(),
                include_screenshot: true,
                accessibility: AccessibilityMode::Full,
            },
        ))
        .await?;
    let (error, mutation_attempted) = match observed {
        Err(error) => (error, false),
        Ok(CommandResult::WindowObserved(observation)) => {
            let point = element_screenshot_point(&observation, "Increment Counter")?;
            (
                expect_error(
                    client
                        .send_outcome(Command::PerformAction(PerformActionInput {
                            session_id: target.session_id.clone(),
                            window_ref: target.window.window_ref.clone(),
                            observation_id: observation.observation_id,
                            action: Action::ClickPoint {
                                point,
                                button: PointerButton::Left,
                                count: 1,
                            },
                        }))
                        .await?,
                    "protected-target foreground action",
                )?,
                true,
            )
        }
        Ok(other) => return Err(unexpected("protected-target observation", &other)),
    };
    if !matches!(
        error.code,
        ErrorCode::PermissionRequired
            | ErrorCode::ForegroundRequired
            | ErrorCode::TargetUnavailable
            | ErrorCode::TargetUnresponsive
            | ErrorCode::Unsupported
    ) {
        return Err(format!(
            "protected target returned non-explicit failure {:?}",
            error.code
        )
        .into());
    }
    if mutation_attempted && error.mutation_status == MutationStatus::NotApplicable {
        return Err("protected-target mutation failure omitted mutation disposition".into());
    }
    close_target(client, target).await?;
    print_report(json!({
        "status": "passed",
        "mode": "protected",
        "scenario_groups": { "elevated_or_protected_target": "passed" },
        "permissions": permissions,
        "error": error
    }))
}

struct Target {
    session_id: SessionId,
    window: WindowSummary,
}

async fn open_target(
    cli: &Cli,
    client: &mut Client,
    ttl_seconds: u32,
) -> Result<Target, Box<dyn Error>> {
    let capabilities = match client.send(Command::GetCapabilities).await? {
        CommandResult::Capabilities(value) => value,
        other => return Err(unexpected("permission capabilities", &other)),
    };
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("permission discovery", &other)),
    };
    let application = select_application(&discovered, &cli.application_match)?;
    let session = match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::Bounded,
                application_refs: vec![application.discovery_ref.clone()],
                allowed_actions: capabilities.actions,
                allow_foreground_input: true,
                ttl_seconds,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => value,
        other => return Err(unexpected("permission session", &other)),
    };
    let window = select_window(
        list_windows(client, &session.session_id).await?,
        &cli.window_title_prefix,
    )?;
    Ok(Target {
        session_id: session.session_id,
        window,
    })
}

async fn close_target(client: &mut Client, target: Target) -> Result<(), Box<dyn Error>> {
    client
        .send(Command::CloseSession(SessionInput {
            session_id: target.session_id,
        }))
        .await?;
    Ok(())
}

async fn platform_and_permissions(
    client: &mut Client,
) -> Result<(Platform, PermissionStatus), Box<dyn Error>> {
    let platform = match client.send(Command::GetCapabilities).await? {
        CommandResult::Capabilities(value) => value.platform,
        other => return Err(unexpected("permission capabilities", &other)),
    };
    let status = match client.send(Command::GetPermissionStatus).await? {
        CommandResult::PermissionStatus(value) => value,
        other => return Err(unexpected("permission status", &other)),
    };
    Ok((platform, status))
}

fn permission_state(status: &PermissionStatus, permission: PermissionKind) -> PermissionState {
    match permission {
        PermissionKind::ScreenCapture => status.screen_capture,
        PermissionKind::Accessibility => status.accessibility,
        PermissionKind::InputControl => status.input_control,
    }
}

fn require_state(
    status: &PermissionStatus,
    permission: PermissionKind,
    expected: PermissionState,
) -> Result<(), Box<dyn Error>> {
    let actual = permission_state(status, permission);
    if actual != expected {
        return Err(format!("{permission:?} reported {actual:?}, expected {expected:?}").into());
    }
    Ok(())
}

fn expect_error(
    outcome: Result<CommandResult, CuaError>,
    scenario: &str,
) -> Result<CuaError, Box<dyn Error>> {
    match outcome {
        Err(error) => Ok(error),
        Ok(result) => Err(format!("{scenario} unexpectedly succeeded: {result:?}").into()),
    }
}

fn require_error(
    error: &CuaError,
    code: ErrorCode,
    mutation_status: MutationStatus,
    scenario: &str,
) -> Result<(), Box<dyn Error>> {
    if error.code != code || error.mutation_status != mutation_status {
        return Err(format!(
            "{scenario} returned {:?}/{:?}, expected {code:?}/{mutation_status:?}",
            error.code, error.mutation_status
        )
        .into());
    }
    Ok(())
}

fn print_report(value: serde_json::Value) -> Result<(), Box<dyn Error>> {
    let mut value = value;
    value["fixture_contract"] = json!("nexus.cua.fixture.v1");
    value["permission_contract"] = json!("nexus.cua.permission.v1");
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
