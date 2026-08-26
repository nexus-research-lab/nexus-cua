//! Controlled accessibility-provider fault validation.

use std::error::Error;
use std::time::Duration;

use clap::{Args, ValueEnum};
use nexus_cua_protocol::{
    AccessibilityMode, Action, ActionKind, CapabilityManifest, Command, CommandResult, CuaError,
    ErrorCode, MutationStatus, OpenSessionInput, PerformActionInput, PermissionMode, RequestId,
    SessionId, SessionInput, WindowSummary,
};
use serde_json::json;
use uuid::Uuid;

use super::{
    Cli, Client, find_actionable, list_windows, observe, perform, select_application,
    select_window, unexpected,
};

const PROVIDER_TIMEOUT_MS: u32 = 3_000;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum FaultMode {
    /// Schedule a fixture hang, then prove action preflight and observation fail closed.
    Preflight,
    /// Hang inside an invoked mutation and reconcile its indeterminate result.
    Dispatch,
}

#[derive(Debug, Args)]
pub(super) struct FaultArgs {
    /// Fault boundary to validate against a freshly started fixture.
    #[arg(value_enum)]
    mode: FaultMode,
    /// Fixture arm delay configured by NEXUS_CUA_FIXTURE_FAULT_ARM_MS.
    #[arg(long, default_value_t = 3_000, value_parser = clap::value_parser!(u64).range(500..=10_000))]
    arm_delay_ms: u64,
}

pub(super) async fn validate(
    cli: &Cli,
    client: &mut Client,
    args: &FaultArgs,
) -> Result<(), Box<dyn Error>> {
    let target = open_target(cli, client).await?;
    let result = match args.mode {
        FaultMode::Preflight => validate_preflight(client, &target, args.arm_delay_ms).await,
        FaultMode::Dispatch => validate_dispatch(client, &target).await,
    };
    let close = client
        .send(Command::CloseSession(SessionInput {
            session_id: target.session_id,
        }))
        .await;
    result?;
    close?;
    Ok(())
}

struct FaultTarget {
    session_id: SessionId,
    window: WindowSummary,
}

async fn open_target(cli: &Cli, client: &mut Client) -> Result<FaultTarget, Box<dyn Error>> {
    let capabilities = match client.send(Command::GetCapabilities).await? {
        CommandResult::Capabilities(value) => value,
        other => return Err(unexpected("fault capabilities", &other)),
    };
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("fault application discovery", &other)),
    };
    let application = select_application(&discovered, &cli.application_match)?;
    let session = match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::Bounded,
                application_refs: vec![application.discovery_ref.clone()],
                allowed_actions: capabilities.actions,
                allow_foreground_input: false,
                ttl_seconds: 60,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => value,
        other => return Err(unexpected("fault session", &other)),
    };
    let window = select_window(
        list_windows(client, &session.session_id).await?,
        &cli.window_title_prefix,
    )?;
    Ok(FaultTarget {
        session_id: session.session_id,
        window,
    })
}

async fn validate_preflight(
    client: &mut Client,
    target: &FaultTarget,
    arm_delay_ms: u64,
) -> Result<(), Box<dyn Error>> {
    let arm_observation = observe(
        client,
        &target.session_id,
        &target.window,
        false,
        AccessibilityMode::Full,
    )
    .await?;
    let arm_element = find_actionable(&arm_observation, "Hang Before Mutation", "invoke")?
        .element_ref
        .clone();
    perform(
        client,
        &target.session_id,
        &target.window,
        &arm_observation,
        Action::InvokeElement {
            element_ref: arm_element,
        },
        ActionKind::InvokeElement,
    )
    .await?;

    let fresh = observe(
        client,
        &target.session_id,
        &target.window,
        false,
        AccessibilityMode::Full,
    )
    .await?;
    let increment = find_actionable(&fresh, "Increment Counter", "invoke")?
        .element_ref
        .clone();
    tokio::time::sleep(Duration::from_millis(arm_delay_ms.saturating_add(150))).await;

    let action_error = expect_error(
        client
            .send_outcome_with(
                request_id("fault_preflight_action"),
                PROVIDER_TIMEOUT_MS,
                Command::PerformAction(PerformActionInput {
                    session_id: target.session_id.clone(),
                    window_ref: target.window.window_ref.clone(),
                    observation_id: fresh.observation_id,
                    action: Action::InvokeElement {
                        element_ref: increment,
                    },
                }),
            )
            .await?,
        "hung action preflight",
    )?;
    require_error(
        &action_error,
        ErrorCode::TargetUnresponsive,
        MutationStatus::NotDispatched,
        "hung action preflight",
    )?;

    let observation_error = expect_error(
        client
            .send_outcome_with(
                request_id("fault_preflight_observation"),
                PROVIDER_TIMEOUT_MS,
                Command::ObserveWindow(nexus_cua_protocol::ObserveWindowInput {
                    session_id: target.session_id.clone(),
                    window_ref: target.window.window_ref.clone(),
                    include_screenshot: false,
                    accessibility: AccessibilityMode::Full,
                }),
            )
            .await?,
        "hung observation",
    )?;
    require_error(
        &observation_error,
        ErrorCode::TargetUnresponsive,
        MutationStatus::NotApplicable,
        "hung observation",
    )?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "fixture_contract": "nexus.cua.fixture.v1",
            "fault_contract": "nexus.cua.fault.v1",
            "status": "passed",
            "mode": "preflight",
            "scenario_groups": {
                "hung_accessibility_observation": "passed",
                "hung_accessibility_preflight": "passed"
            },
            "action_error": action_error,
            "observation_error": observation_error
        }))?
    );
    Ok(())
}

async fn validate_dispatch(
    client: &mut Client,
    target: &FaultTarget,
) -> Result<(), Box<dyn Error>> {
    let observation = observe(
        client,
        &target.session_id,
        &target.window,
        false,
        AccessibilityMode::Full,
    )
    .await?;
    let hang = find_actionable(&observation, "Hang During Invoke", "invoke")?
        .element_ref
        .clone();
    let command = Command::PerformAction(PerformActionInput {
        session_id: target.session_id.clone(),
        window_ref: target.window.window_ref.clone(),
        observation_id: observation.observation_id,
        action: Action::InvokeElement { element_ref: hang },
    });
    let request_id = request_id("fault_dispatch_action");

    let deadline_error = expect_error(
        client
            .send_outcome_with(request_id.clone(), 100, command.clone())
            .await?,
        "dispatched action deadline",
    )?;
    require_error(
        &deadline_error,
        ErrorCode::DeadlineExceeded,
        MutationStatus::Indeterminate,
        "dispatched action deadline",
    )?;

    let reconciled_error = expect_error(
        client
            .send_outcome_with(request_id.clone(), PROVIDER_TIMEOUT_MS, command)
            .await?,
        "same-request dispatched action reconciliation",
    )?;
    require_error(
        &reconciled_error,
        ErrorCode::TargetUnresponsive,
        MutationStatus::Indeterminate,
        "same-request dispatched action reconciliation",
    )?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "fixture_contract": "nexus.cua.fixture.v1",
            "fault_contract": "nexus.cua.fault.v1",
            "status": "passed",
            "mode": "dispatch",
            "scenario_groups": {
                "hung_accessibility_after_dispatch": "passed",
                "same_request_mutation_reconciliation": "passed"
            },
            "request_id": request_id,
            "deadline_error": deadline_error,
            "reconciled_error": reconciled_error
        }))?
    );
    Ok(())
}

fn request_id(label: &str) -> RequestId {
    RequestId::new(format!(
        "native_harness_{label}_{}_{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ))
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
