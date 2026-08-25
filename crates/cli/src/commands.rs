//! CLI commands and orchestration.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use nexus_cua_protocol::{
    AuthorizationToken, Command, PROTOCOL_VERSION, RequestEnvelope, RequestId,
};
use nexus_cua_runtime::{Runtime, RuntimeConfig};
use nexus_cua_transport::{Dispatcher, ServerConfig};
use schemars::schema_for;
use serde_json::json;
use uuid::Uuid;

use crate::diagnostics::LogFormat;
use crate::error::CliError;
use crate::paths::ServicePaths;

/// Nexus CUA native desktop service and diagnostics.
#[derive(Debug, Parser)]
#[command(name = "nexus-cua", version, about)]
pub struct Cli {
    /// Log filter such as `info` or `nexus_cua=debug`.
    #[arg(
        long,
        global = true,
        env = "NEXUS_CUA_LOG_LEVEL",
        default_value = "info"
    )]
    pub log_level: String,
    /// Human-readable or machine-readable service logs.
    #[arg(
        long,
        global = true,
        env = "NEXUS_CUA_LOG_FORMAT",
        value_enum,
        default_value = "pretty"
    )]
    pub log_format: LogFormat,
    /// Operation to execute.
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Run the authenticated private local service.
    Serve(ServeArgs),
    /// Inspect native capabilities and OS permission state.
    Doctor(DoctorArgs),
    /// Print the closed protocol JSON schemas.
    Schema,
    /// Send one command to a running local service.
    Request(RequestArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Create isolated development state below this directory.
    #[arg(long, conflicts_with_all = ["endpoint", "token_file", "artifact_root"])]
    dev_root: Option<PathBuf>,
    /// Unix socket path or Windows named-pipe path.
    #[arg(long, requires_all = ["token_file", "artifact_root"])]
    endpoint: Option<String>,
    /// Existing private file containing the shared transport token.
    #[arg(long, requires_all = ["endpoint", "artifact_root"])]
    token_file: Option<PathBuf>,
    /// Private transient screenshot directory.
    #[arg(long, requires_all = ["endpoint", "token_file"])]
    artifact_root: Option<PathBuf>,
    /// Maximum encoded request or response size.
    #[arg(long, default_value_t = 1024 * 1024)]
    max_frame_bytes: usize,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Emit a single JSON object instead of pretty JSON.
    #[arg(long)]
    compact: bool,
}

#[derive(Debug, Args)]
struct RequestArgs {
    /// Unix socket path or Windows named-pipe path.
    #[arg(long)]
    endpoint: String,
    /// Private file containing the shared transport token.
    #[arg(long)]
    token_file: PathBuf,
    /// JSON file containing one tagged protocol Command.
    #[arg(long)]
    command_file: PathBuf,
    /// Stable retry identity; generated when omitted.
    #[arg(long)]
    request_id: Option<String>,
    /// Maximum encoded request or response size.
    #[arg(long, default_value_t = 1024 * 1024)]
    max_frame_bytes: usize,
    /// End-to-end command deadline in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u32,
}

pub async fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        CliCommand::Serve(args) => serve(args).await,
        CliCommand::Doctor(args) => doctor(args).await,
        CliCommand::Schema => print_schema(),
        CliCommand::Request(args) => send_request(args).await,
    }
}

async fn serve(args: ServeArgs) -> Result<(), CliError> {
    let max_frame_bytes = args.max_frame_bytes;
    let paths = resolve_service_paths(args)?;
    let token = paths.prepare()?;
    let driver = nexus_cua_platform::system_driver()?;
    let runtime = Arc::new(
        Runtime::new(driver, RuntimeConfig::new(&paths.artifact_root))
            .map_err(CliError::Runtime)?,
    );
    let mut server_config = ServerConfig::new(paths.endpoint.clone());
    server_config.max_frame_bytes = max_frame_bytes;
    let dispatcher = Arc::new(Dispatcher::new(
        runtime,
        &token,
        server_config.max_inflight_requests,
        server_config.max_completed_requests,
        server_config.max_request_timeout_ms,
    )?);
    eprintln!("Nexus CUA endpoint: {}", paths.endpoint);
    eprintln!("Nexus CUA token file: {}", paths.token_file.display());
    nexus_cua_transport::serve_until(dispatcher, server_config, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

fn resolve_service_paths(args: ServeArgs) -> Result<ServicePaths, CliError> {
    match (
        args.dev_root,
        args.endpoint,
        args.token_file,
        args.artifact_root,
    ) {
        (Some(root), None, None, None) => ServicePaths::development(&root),
        (None, Some(endpoint), Some(token_file), Some(artifact_root)) => {
            ServicePaths::explicit(endpoint, token_file, artifact_root)
        }
        _ => Err(CliError::InvalidConfiguration(
            "serve requires --dev-root or all of --endpoint, --token-file, and --artifact-root"
                .to_owned(),
        )),
    }
}

async fn doctor(args: DoctorArgs) -> Result<(), CliError> {
    let driver = nexus_cua_platform::system_driver()?;
    let capabilities = driver.capabilities().await?;
    let permissions = driver.permission_status().await?;
    let value = json!({
        "protocol_version": PROTOCOL_VERSION,
        "capabilities": capabilities,
        "permissions": permissions,
    });
    if args.compact {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

fn print_schema() -> Result<(), CliError> {
    let value = json!({
        "protocol_version": PROTOCOL_VERSION,
        "request": schema_for!(RequestEnvelope),
        "response": schema_for!(nexus_cua_protocol::ResponseEnvelope),
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

async fn send_request(args: RequestArgs) -> Result<(), CliError> {
    let token = read_token(&args.token_file)?;
    let command: Command = serde_json::from_slice(&std::fs::read(&args.command_file)?)?;
    let request = RequestEnvelope {
        protocol_version: PROTOCOL_VERSION.to_owned(),
        request_id: RequestId::new(
            args.request_id
                .unwrap_or_else(|| format!("request_{}", Uuid::new_v4().simple())),
        ),
        timeout_ms: args.timeout_ms,
        authorization: token,
        command,
    };
    let endpoint = nexus_cua_transport::LocalEndpoint::new(args.endpoint)?;
    let response = nexus_cua_transport::request(&endpoint, &request, args.max_frame_bytes).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn read_token(path: &PathBuf) -> Result<AuthorizationToken, CliError> {
    let value = std::fs::read_to_string(path)?;
    let value = value.trim();
    if value.len() < 32 {
        return Err(CliError::InvalidConfiguration(
            "token file must contain at least 32 non-whitespace bytes".to_owned(),
        ));
    }
    Ok(AuthorizationToken::new(value))
}
