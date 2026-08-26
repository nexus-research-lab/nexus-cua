//! Two-stage sidecar restart probe for hardware-runner orchestration.

use std::error::Error;
use std::path::PathBuf;

use clap::Args;
use nexus_cua_protocol::{
    AccessibilityMode, CapabilityManifest, Command, CommandResult, ErrorCode, ListWindowsInput,
    OpenSessionInput, PermissionMode, SessionId,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Cli, Client, observe, select_application, select_window, unexpected};

#[derive(Debug, Args)]
pub(super) struct RestartArgs {
    /// Probe state shared across the stop/start boundary; contains no transport token.
    #[arg(long)]
    state_file: PathBuf,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RestartState {
    contract_version: String,
    session_id: String,
    artifact_path: String,
}

pub(super) async fn prepare(
    cli: &Cli,
    client: &mut Client,
    args: &RestartArgs,
) -> Result<(), Box<dyn Error>> {
    let capabilities = match client.send(Command::GetCapabilities).await? {
        CommandResult::Capabilities(value) => value,
        other => return Err(unexpected("capabilities", &other)),
    };
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("application discovery", &other)),
    };
    let application = select_application(&discovered, &cli.application_match)?;
    let session = match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::Bounded,
                application_refs: vec![application.discovery_ref.clone()],
                allowed_actions: capabilities.actions,
                allow_foreground_input: true,
                ttl_seconds: 180,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => value,
        other => return Err(unexpected("restart probe session", &other)),
    };
    let window = select_window(
        match client
            .send(Command::ListWindows(ListWindowsInput {
                session_id: session.session_id.clone(),
                app_ref: None,
            }))
            .await?
        {
            CommandResult::Windows(value) => value,
            other => return Err(unexpected("restart probe window list", &other)),
        },
        &cli.window_title_prefix,
    )?;
    let observation = observe(
        client,
        &session.session_id,
        &window,
        true,
        AccessibilityMode::Disabled,
    )
    .await?;
    let artifact_path = observation
        .screenshot
        .as_ref()
        .ok_or("restart probe observation omitted its screenshot")?
        .path
        .clone();
    let state = RestartState {
        contract_version: "nexus.cua.restart.v1".to_owned(),
        session_id: session.session_id.to_string(),
        artifact_path,
    };
    if let Some(parent) = args.state_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.state_file, serde_json::to_vec_pretty(&state)?)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "fixture_contract": "nexus.cua.fixture.v1",
            "restart_contract": state.contract_version,
            "status": "prepared",
            "state_file": args.state_file,
            "live_session": true,
            "live_artifact": std::path::Path::new(&state.artifact_path).exists(),
        }))?
    );
    Ok(())
}

pub(super) async fn verify(client: &mut Client, args: &RestartArgs) -> Result<(), Box<dyn Error>> {
    let state: RestartState = serde_json::from_slice(&std::fs::read(&args.state_file)?)?;
    if state.contract_version != "nexus.cua.restart.v1" {
        return Err("restart probe state has an unsupported contract version".into());
    }
    match client
        .send_outcome(Command::ListWindows(ListWindowsInput {
            session_id: SessionId::new(state.session_id),
            app_ref: None,
        }))
        .await?
    {
        Err(error) if error.code == ErrorCode::SessionUnavailable => {}
        Err(error) => {
            return Err(format!(
                "post-restart session returned {:?}, expected session_unavailable",
                error.code
            )
            .into());
        }
        Ok(_) => return Err("pre-restart session remained usable after sidecar restart".into()),
    }
    if std::path::Path::new(&state.artifact_path).exists() {
        return Err("sidecar restart retained the prepared transient artifact".into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "fixture_contract": "nexus.cua.fixture.v1",
            "restart_contract": state.contract_version,
            "status": "passed",
            "scenario_groups": { "sidecar_restart": "passed" },
            "old_session": "session_unavailable",
            "artifact_cleanup": "passed",
        }))?
    );
    Ok(())
}
