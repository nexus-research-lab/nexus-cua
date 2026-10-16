//! Agent-facing CLI binding over the authenticated internal service.

use std::io::{Read as _, Write as _};

use clap::{Args, Subcommand, ValueEnum};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use zeroize::Zeroizing;

const MAX_INPUT_BYTES: u64 = 256 * 1024;
const CATALOGUE: &[u8] = include_bytes!("../../../skills/catalogue.json");

#[derive(Debug, Error)]
pub(crate) enum AgentError {
    #[error(transparent)]
    Operation(#[from] crate::outcome::OperationFailure),
    #[error("Agent CLI input is invalid: {0}")]
    Invalid(String),
    #[error("Agent CLI JSON input failed")]
    Json(#[from] serde_json::Error),
    #[error("Agent CLI I/O failed")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Service(#[from] crate::service::ServiceError),
}

#[derive(Debug, Args)]
pub(crate) struct SessionArgs {
    /// Session lifecycle operation.
    #[command(subcommand)]
    pub(crate) command: SessionCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SessionCommand {
    /// Attach to the already-running registered service and read its authority/session state.
    Open(RegistrationArgs),
    /// List exact current tool argument schemas from the running service.
    Catalogue(RegistrationArgs),
    /// Gracefully close the registered service and its runtime session.
    Close(RegistrationArgs),
    /// Read the current authority/session state.
    Status(RegistrationArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RegistrationArgs {
    /// Non-secret CLI registration selected by the trusted host.
    #[arg(long)]
    registration: String,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AppArgs {
    /// Application operation.
    #[command(subcommand)]
    pub(crate) command: AppCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AppCommand {
    /// List running applications inside the host-authorized session.
    List(RegistrationArgs),
    /// List host-authorized installed applications using opaque launch discovery.
    ListLaunchable(RegistrationArgs),
    /// Cold-launch one exact stable identity; paths and arguments are never accepted.
    Launch(AppLaunchArgs),
    /// Cooperatively terminate the exact generation owning a freshly observed window.
    Terminate(AppTerminateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AppLaunchArgs {
    #[command(flatten)]
    output: RegistrationArgs,
    /// Exact stable identity returned by `app list-launchable`.
    #[arg(long)]
    application_id: String,
    /// Stable provider-namespaced mutation identity.
    #[arg(long)]
    request_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct AppTerminateArgs {
    #[command(flatten)]
    output: RegistrationArgs,
    /// Opaque target returned by `window list`.
    #[arg(long)]
    window_ref: String,
    /// Fresh guard returned by `window observe`.
    #[arg(long)]
    observation_id: String,
    /// Stable provider-namespaced mutation identity.
    #[arg(long)]
    request_id: String,
}

#[derive(Debug, Args)]
pub(crate) struct WindowArgs {
    /// Window operation.
    #[command(subcommand)]
    pub(crate) command: WindowCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WindowCommand {
    /// List exact top-level windows, optionally filtered by opaque app ref.
    List(WindowListArgs),
    /// Capture one exact window and optional transient screenshot.
    Observe(WindowObserveArgs),
    /// Crop one current screenshot observation using normalized coordinates.
    Zoom(WindowZoomArgs),
}

#[derive(Debug, Args)]
pub(crate) struct WindowListArgs {
    #[command(flatten)]
    output: RegistrationArgs,
    /// Optional opaque application reference returned by `app list`.
    #[arg(long)]
    app_ref: Option<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AccessibilityArg {
    Disabled,
    Interactive,
    Full,
}

impl AccessibilityArg {
    const fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Interactive => "interactive",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct WindowObserveArgs {
    #[command(flatten)]
    output: RegistrationArgs,
    /// Opaque target returned by `window list`.
    #[arg(long)]
    window_ref: String,
    /// Include the bounded transient PNG content block.
    #[arg(long, default_value_t = false)]
    include_screenshot: bool,
    /// Bounded semantic-tree detail.
    #[arg(long, value_enum, default_value_t = AccessibilityArg::Interactive)]
    accessibility: AccessibilityArg,
}

#[derive(Debug, Args)]
pub(crate) struct WindowZoomArgs {
    #[command(flatten)]
    output: RegistrationArgs,
    #[arg(long)]
    window_ref: String,
    #[arg(long)]
    observation_id: String,
    #[arg(long)]
    left: u16,
    #[arg(long)]
    top: u16,
    #[arg(long)]
    right: u16,
    #[arg(long)]
    bottom: u16,
}

#[derive(Debug, Args)]
pub(crate) struct DisplayArgs {
    /// Physical-display read operation.
    #[command(subcommand)]
    pub(crate) command: DisplayCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum DisplayCommand {
    /// List exact physical displays under separate read-only authority.
    List(RegistrationArgs),
    /// Observe one opaque physical display target.
    Observe(DisplayObserveArgs),
}

#[derive(Debug, Args)]
pub(crate) struct DisplayObserveArgs {
    #[command(flatten)]
    output: RegistrationArgs,
    #[arg(long)]
    display_ref: String,
}

#[derive(Debug, Args)]
pub(crate) struct ClipboardArgs {
    /// Clipboard operation.
    #[command(subcommand)]
    pub(crate) command: ClipboardCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ClipboardCommand {
    /// Read bounded plain text and a fresh opaque write guard.
    Read(RegistrationArgs),
}

/// Generic closed mutation binding. Arguments are bounded JSON read only from stdin.
#[derive(Debug, Args)]
pub(crate) struct ActionArgs {
    /// Canonical mutation leaf, such as `click_normalized` or `type_text`.
    operation: String,
    #[command(flatten)]
    output: RegistrationArgs,
    /// Read one bounded JSON object from stdin; no input file path is accepted.
    #[arg(long)]
    input_stdin: bool,
}

/// Deterministic state verification using bounded JSON read only from stdin.
#[derive(Debug, Args)]
pub(crate) struct VerifyArgs {
    #[command(flatten)]
    output: RegistrationArgs,
    /// Read `{window_ref, predicate}` from bounded stdin.
    #[arg(long)]
    input_stdin: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalogue {
    schema_version: u32,
    product_version: String,
    mcp_alias: String,
    tools: Vec<CatalogueTool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueTool {
    leaf: String,
    kind: String,
    vision: bool,
}

pub(crate) async fn run_session(args: SessionArgs) -> Result<(), AgentError> {
    match args.command {
        SessionCommand::Open(args) | SessionCommand::Status(args) => {
            call_and_write(&args, "get_status", json!({})).await
        }
        SessionCommand::Catalogue(args) => {
            let value = crate::service::list_tools(&args.registration).await?;
            write_output(&value, args.json)
        }
        SessionCommand::Close(args) => {
            let value = crate::service::stop_registered(&args.registration).await?;
            write_output(&value, args.json)
        }
    }
}

pub(crate) async fn run_app(args: AppArgs) -> Result<(), AgentError> {
    match args.command {
        AppCommand::List(args) => call_and_write(&args, "list_apps", json!({})).await,
        AppCommand::ListLaunchable(args) => {
            call_and_write(&args, "list_launchable_apps", json!({})).await
        }
        AppCommand::Launch(args) => {
            call_and_write(
                &args.output,
                "launch_application",
                json!({
                    "application_id": args.application_id,
                    "request_id": args.request_id,
                }),
            )
            .await
        }
        AppCommand::Terminate(args) => {
            call_and_write(
                &args.output,
                "terminate_application",
                json!({
                    "window_ref": args.window_ref,
                    "observation_id": args.observation_id,
                    "request_id": args.request_id,
                }),
            )
            .await
        }
    }
}

pub(crate) async fn run_window(args: WindowArgs) -> Result<(), AgentError> {
    match args.command {
        WindowCommand::List(args) => {
            call_and_write(
                &args.output,
                "list_windows",
                json!({"app_ref": args.app_ref}),
            )
            .await
        }
        WindowCommand::Observe(args) => {
            call_and_write(
                &args.output,
                "observe_window",
                json!({
                    "window_ref": args.window_ref,
                    "include_screenshot": args.include_screenshot,
                    "accessibility": args.accessibility.name(),
                }),
            )
            .await
        }
        WindowCommand::Zoom(args) => {
            call_and_write(
                &args.output,
                "zoom",
                json!({
                    "window_ref": args.window_ref,
                    "observation_id": args.observation_id,
                    "left": args.left,
                    "top": args.top,
                    "right": args.right,
                    "bottom": args.bottom,
                }),
            )
            .await
        }
    }
}

pub(crate) async fn run_display(args: DisplayArgs) -> Result<(), AgentError> {
    match args.command {
        DisplayCommand::List(args) => call_and_write(&args, "list_displays", json!({})).await,
        DisplayCommand::Observe(args) => {
            call_and_write(
                &args.output,
                "observe_display",
                json!({"display_ref": args.display_ref}),
            )
            .await
        }
    }
}

pub(crate) async fn run_clipboard(args: ClipboardArgs) -> Result<(), AgentError> {
    match args.command {
        ClipboardCommand::Read(args) => call_and_write(&args, "read_clipboard", json!({})).await,
    }
}

pub(crate) async fn run_action(args: ActionArgs) -> Result<(), AgentError> {
    validate_catalogue_operation(&args.operation, "mutation")?;
    let input = read_json_stdin(args.input_stdin)?;
    call_and_write(&args.output, &args.operation, input).await
}

pub(crate) async fn run_verify(args: VerifyArgs) -> Result<(), AgentError> {
    validate_catalogue_operation("verify_state", "read")?;
    let input = read_json_stdin(args.input_stdin)?;
    call_and_write(&args.output, "verify_state", input).await
}

pub(crate) async fn reconcile_registered(
    registration_id: &str,
    request_id: &str,
    wait_ms: u32,
) -> Result<Value, AgentError> {
    validate_catalogue_operation("reconcile_mutation", "reconcile")?;
    crate::service::call_tool(
        registration_id,
        "reconcile_mutation",
        json!({"request_id": request_id, "wait_ms": wait_ms}),
    )
    .await
    .map_err(Into::into)
}

async fn call_and_write(
    args: &RegistrationArgs,
    tool: &str,
    input: Value,
) -> Result<(), AgentError> {
    let value = crate::service::call_tool(&args.registration, tool, input).await?;
    write_output(&value, args.json)?;
    crate::outcome::check_tool_result(&value).map_err(Into::into)
}

fn read_json_stdin(enabled: bool) -> Result<Value, AgentError> {
    if !enabled {
        return Err(AgentError::Invalid(
            "--input-stdin is required; Agent commands never read an input file".to_owned(),
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    std::io::stdin()
        .take(MAX_INPUT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(AgentError::Invalid(
            "stdin must contain one JSON object no larger than 256 KiB".to_owned(),
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object() {
        return Err(AgentError::Invalid(
            "stdin must contain one JSON object".to_owned(),
        ));
    }
    Ok(value)
}

fn validate_catalogue_operation(operation: &str, expected_kind: &str) -> Result<(), AgentError> {
    let catalogue: Catalogue = serde_json::from_slice(CATALOGUE)?;
    if catalogue.schema_version != 1
        || catalogue.product_version != env!("CARGO_PKG_VERSION")
        || catalogue.mcp_alias != "nexuscua"
        || !catalogue.tools.iter().any(|tool| {
            tool.leaf == operation
                && tool.kind == expected_kind
                && (tool.vision || tool.leaf == "perform_action")
        })
    {
        return Err(AgentError::Invalid(format!(
            "{operation} is not a canonical {expected_kind} operation"
        )));
    }
    Ok(())
}

fn write_output(value: &Value, compact: bool) -> Result<(), AgentError> {
    let mut stdout = std::io::stdout().lock();
    if compact {
        serde_json::to_writer(&mut stdout, value)?;
    } else {
        serde_json::to_writer_pretty(&mut stdout, value)?;
    }
    stdout.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogue_operation_has_exactly_one_cli_route() {
        let catalogue: Catalogue = serde_json::from_slice(CATALOGUE).expect("catalogue");
        for tool in catalogue.tools {
            let routed = match tool.kind.as_str() {
                "mutation" => validate_catalogue_operation(&tool.leaf, "mutation").is_ok(),
                "reconcile" => tool.leaf == "reconcile_mutation",
                "read" => matches!(
                    tool.leaf.as_str(),
                    "get_status"
                        | "list_apps"
                        | "list_launchable_apps"
                        | "list_windows"
                        | "list_displays"
                        | "observe_display"
                        | "observe_window"
                        | "read_clipboard"
                        | "zoom"
                        | "verify_state"
                ),
                _ => false,
            };
            assert!(routed, "missing CLI route for {}", tool.leaf);
        }
    }

    #[test]
    fn generic_action_rejects_read_and_unknown_leaves() {
        assert!(validate_catalogue_operation("click_normalized", "mutation").is_ok());
        assert!(validate_catalogue_operation("list_apps", "mutation").is_err());
        assert!(validate_catalogue_operation("shell", "mutation").is_err());
    }
}
