//! CLI commands and orchestration.

use std::io::Read as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Parser, Subcommand, ValueEnum};
use nexus_cua_mcp::{
    DEFAULT_MAX_IMAGE_BYTES, DEFAULT_MAX_OBSERVATION_AGE_SECONDS, McpHttpConfig,
    McpHttpResponseMode, McpServeConfig, ToolProfile,
};
use nexus_cua_protocol::{
    Command, PROTOCOL_VERSION, ReconcileMutationInput, RequestEnvelope, RequestId,
};
use nexus_cua_provider::{
    McpRegisteredTransport, RegisteredAdapterProfile, RegisteredAuthorityProvider,
    RegistrationStore, default_state_root,
};
use nexus_cua_runtime::{Runtime, RuntimeConfig, RuntimePolicyStack};
use nexus_cua_transport::{Dispatcher, ServerConfig};
use schemars::schema_for;
use serde_json::json;
use uuid::Uuid;

use crate::diagnostics::LogFormat;
use crate::error::CliError;
use crate::paths::{ServicePaths, read_private_token};

/// Nexus Computer Use Runtime service and diagnostics.
#[derive(Debug, Parser)]
#[command(name = "nexuscua", version, about)]
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
    /// Print the installed product and protocol version.
    Version(VersionArgs),
    /// Run the authenticated private local service.
    Serve(ServeArgs),
    /// Inspect native capabilities and OS permission state.
    Doctor(DoctorArgs),
    /// Print the closed protocol JSON schemas.
    Schema(SchemaArgs),
    /// Send one command to a running local service.
    Request(RequestArgs),
    /// Inspect retained mutation outcomes without redispatching them.
    Mutation(MutationArgs),
    /// Run or inspect the model-facing MCP adapter.
    Mcp(McpArgs),
    /// Inspect, plan, apply, or remove one host integration.
    Integration(crate::integration::IntegrationArgs),
    /// Plan, apply, inspect, or revoke one human-approved CLI authority.
    Authority(crate::authority::AuthorityArgs),
    /// Start, stop, or inspect one registered internal CLI adapter service.
    Service(crate::service::ServiceArgs),
    /// Attach, inspect, or close one registered CLI runtime session.
    Session(crate::agent::SessionArgs),
    /// Discover, launch, or terminate an authorized application.
    App(crate::agent::AppArgs),
    /// List, observe, or zoom an authorized window.
    Window(crate::agent::WindowArgs),
    /// List or observe an authorized physical display.
    Display(crate::agent::DisplayArgs),
    /// Read the clipboard under separate authority.
    Clipboard(crate::agent::ClipboardArgs),
    /// Execute one canonical mutation using bounded JSON from stdin.
    Action(crate::agent::ActionArgs),
    /// Evaluate one deterministic predicate using bounded JSON from stdin.
    Verify(crate::agent::VerifyArgs),
}

#[derive(Debug, Args)]
struct McpArgs {
    /// MCP adapter operation.
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    /// Launch from one authenticated, host-bound registration.
    Launch(McpLaunchArgs),
    /// Serve one host-authorized MCP adapter.
    Serve(Box<McpServeArgs>),
}

#[derive(Debug, Args)]
struct McpLaunchArgs {
    /// Non-secret registration selected by the trusted host integration.
    #[arg(long)]
    registration: String,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum McpTransport {
    /// Local process transport over standard input/output.
    Stdio,
    /// Current stateless Streamable HTTP transport.
    Http,
}

#[derive(Debug, Args)]
struct McpServeArgs {
    /// MCP transport. SSE is not a separate configuration value.
    #[arg(long, value_enum, default_value_t = McpTransport::Stdio)]
    transport: McpTransport,
    /// Unsigned source-build authority fixture; never a production trust root.
    #[arg(long, requires = "allow_unsigned_development_authority")]
    development_authority_file: PathBuf,
    /// Explicitly accept the unsigned fixture for isolated source dogfood only.
    #[arg(long, requires = "development_authority_file", default_value_t = false)]
    allow_unsigned_development_authority: bool,
    /// Private directory for transient screenshot artifacts.
    #[arg(long)]
    artifact_root: PathBuf,
    /// Largest PNG returned as one MCP image block.
    #[arg(long, default_value_t = DEFAULT_MAX_IMAGE_BYTES)]
    max_image_bytes: u64,
    /// Maximum model think time before a guarded observation must be refreshed.
    #[arg(long, default_value_t = DEFAULT_MAX_OBSERVATION_AGE_SECONDS)]
    max_observation_age_seconds: u64,
    /// Optional administrator-owned immutable runtime policy JSON.
    #[arg(long)]
    managed_policy_file: Option<PathBuf>,
    /// Optional user-owned immutable runtime policy JSON.
    #[arg(long)]
    policy_file: Option<PathBuf>,
    /// Model-facing schema surface. `vision` hides the generic action union.
    #[arg(long, value_enum, default_value_t = ToolProfile::Full)]
    tool_profile: ToolProfile,
    /// Private Bearer token file required by the HTTP transport.
    #[arg(long)]
    http_token_file: Option<PathBuf>,
    /// HTTP bind address; only loopback is accepted in this product stage.
    #[arg(long)]
    bind: Option<IpAddr>,
    /// HTTP listener port. Zero asks the OS for an available port.
    #[arg(long)]
    port: Option<u16>,
    /// Single Streamable HTTP endpoint path.
    #[arg(long)]
    endpoint: Option<String>,
    /// Maximum streamed HTTP POST body in bytes.
    #[arg(long)]
    max_http_body_bytes: Option<usize>,
    /// Maximum concurrently admitted authenticated HTTP requests.
    #[arg(long)]
    max_http_inflight_requests: Option<usize>,
    /// Maximum TCP connections accepted by this adapter process.
    #[arg(long)]
    max_http_connections: Option<usize>,
    /// Close a connection after this many seconds without read/write progress.
    #[arg(long)]
    http_io_idle_timeout_seconds: Option<u64>,
    /// Absolute maximum lifetime of one HTTP connection in seconds.
    #[arg(long)]
    http_connection_lifetime_seconds: Option<u64>,
    /// Maximum authenticated request duration including streamed body seconds.
    #[arg(long)]
    http_request_timeout_seconds: Option<u64>,
    /// Streamable HTTP response representation; SSE remains request-scoped.
    #[arg(long, value_enum)]
    http_response_mode: Option<McpHttpResponseMode>,
}

#[derive(Debug, Args)]
struct VersionArgs {
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
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
    /// Optional administrator-owned immutable runtime policy JSON.
    #[arg(long)]
    managed_policy_file: Option<PathBuf>,
    /// Optional user-owned immutable runtime policy JSON.
    #[arg(long)]
    policy_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Emit a single JSON object instead of pretty JSON.
    #[arg(long)]
    compact: bool,
}

#[derive(Debug, Args)]
struct SchemaArgs {
    /// Write deterministic request/response schema files below this directory.
    #[arg(long)]
    output_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct RequestArgs {
    /// Unix socket path or Windows named-pipe path.
    #[arg(long, env = "NEXUS_CUA_ENDPOINT")]
    endpoint: String,
    /// Private file containing the shared transport token.
    #[arg(long, env = "NEXUS_CUA_TOKEN_FILE")]
    token_file: PathBuf,
    /// JSON file containing one tagged protocol Command.
    #[arg(
        long,
        conflicts_with = "command_stdin",
        required_unless_present = "command_stdin"
    )]
    command_file: Option<PathBuf>,
    /// Read one tagged protocol Command from bounded standard input.
    #[arg(long, conflicts_with = "command_file")]
    command_stdin: bool,
    /// Stable retry identity; generated when omitted.
    #[arg(long)]
    request_id: Option<String>,
    /// Maximum encoded request or response size.
    #[arg(long, default_value_t = 1024 * 1024)]
    max_frame_bytes: usize,
    /// End-to-end command deadline in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    timeout_ms: u32,
    /// Emit one compact JSON response object.
    #[arg(long)]
    compact: bool,
}

#[derive(Debug, Args)]
struct MutationArgs {
    /// Mutation-ledger operation.
    #[command(subcommand)]
    command: MutationCommand,
}

#[derive(Debug, Subcommand)]
enum MutationCommand {
    /// Query one prior mutation identity without dispatching it again.
    Reconcile(ReconcileArgs),
}

#[derive(Debug, Args)]
struct ReconcileArgs {
    /// Non-secret CLI registration for the Agent-facing service path.
    #[arg(long, conflicts_with_all = ["endpoint", "token_file"])]
    registration: Option<String>,
    /// Unix socket path or Windows named-pipe path.
    #[arg(long, env = "NEXUS_CUA_ENDPOINT", requires = "token_file")]
    endpoint: Option<String>,
    /// Private file containing the shared transport token.
    #[arg(long, env = "NEXUS_CUA_TOKEN_FILE", requires = "endpoint")]
    token_file: Option<PathBuf>,
    /// Original stable host-generated mutation identity.
    #[arg(long)]
    request_id: String,
    /// Maximum wait for an already-admitted mutation, in milliseconds.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=120_000))]
    wait_ms: u32,
    /// Maximum encoded request or response size.
    #[arg(long, default_value_t = 1024 * 1024)]
    max_frame_bytes: usize,
    /// Emit one compact JSON response object.
    #[arg(long)]
    compact: bool,
}

const MAX_COMMAND_BYTES: u64 = 1024 * 1024;

pub async fn run(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        CliCommand::Version(args) => version(&args),
        CliCommand::Serve(args) => serve(args).await,
        CliCommand::Doctor(args) => doctor(args).await,
        CliCommand::Schema(args) => write_or_print_schema(args),
        CliCommand::Request(args) => send_request(args).await,
        CliCommand::Mutation(args) => run_mutation(args).await,
        CliCommand::Mcp(args) => run_mcp(args).await,
        CliCommand::Integration(args) => crate::integration::run(args).await.map_err(Into::into),
        CliCommand::Authority(args) => crate::authority::run(args).await.map_err(Into::into),
        CliCommand::Service(args) => crate::service::run(args).await.map_err(Into::into),
        CliCommand::Session(args) => crate::agent::run_session(args).await.map_err(Into::into),
        CliCommand::App(args) => crate::agent::run_app(args).await.map_err(Into::into),
        CliCommand::Window(args) => crate::agent::run_window(args).await.map_err(Into::into),
        CliCommand::Display(args) => crate::agent::run_display(args).await.map_err(Into::into),
        CliCommand::Clipboard(args) => crate::agent::run_clipboard(args).await.map_err(Into::into),
        CliCommand::Action(args) => crate::agent::run_action(args).await.map_err(Into::into),
        CliCommand::Verify(args) => crate::agent::run_verify(args).await.map_err(Into::into),
    }
}

async fn run_mutation(args: MutationArgs) -> Result<(), CliError> {
    match args.command {
        MutationCommand::Reconcile(args) => reconcile_mutation(args).await,
    }
}

async fn reconcile_mutation(args: ReconcileArgs) -> Result<(), CliError> {
    if let Some(registration) = args.registration.as_deref() {
        let result =
            crate::agent::reconcile_registered(registration, &args.request_id, args.wait_ms)
                .await?;
        write_response(&result, args.compact)?;
        return crate::outcome::check_tool_result(&result).map_err(Into::into);
    }
    let endpoint = args.endpoint.as_deref().ok_or_else(|| {
        CliError::InvalidConfiguration(
            "mutation reconcile requires --registration or --endpoint with --token-file".to_owned(),
        )
    })?;
    let token_file = args.token_file.as_deref().ok_or_else(|| {
        CliError::InvalidConfiguration(
            "mutation reconcile requires --registration or --endpoint with --token-file".to_owned(),
        )
    })?;
    let token = read_private_token(token_file)?;
    let target_request_id = RequestId::new(args.request_id);
    let request = RequestEnvelope {
        protocol_version: PROTOCOL_VERSION.to_owned(),
        request_id: RequestId::new(format!("reconcile_{}", Uuid::new_v4().simple())),
        timeout_ms: 120_000,
        authorization: token,
        command: Command::ReconcileMutation(ReconcileMutationInput {
            request_id: target_request_id,
            wait_ms: args.wait_ms,
        }),
    };
    let endpoint = nexus_cua_transport::LocalEndpoint::new(endpoint)?;
    let response = nexus_cua_transport::request(&endpoint, &request, args.max_frame_bytes).await?;
    write_response(&response, args.compact)?;
    check_response(response.outcome)
}

async fn run_mcp(args: McpArgs) -> Result<(), CliError> {
    match args.command {
        McpCommand::Launch(args) => launch_registered_mcp(args).await,
        McpCommand::Serve(args) => {
            let has_http_options = has_http_options(&args);
            if !args.allow_unsigned_development_authority {
                return Err(CliError::InvalidConfiguration(
                    "MCP serve requires an authenticated provider; unsigned fixtures require --allow-unsigned-development-authority"
                        .to_owned(),
                ));
            }
            let mut config = McpServeConfig::unsigned_development(
                args.development_authority_file,
                args.artifact_root,
            );
            config.max_image_bytes = args.max_image_bytes;
            config.max_observation_age_seconds = args.max_observation_age_seconds;
            config.managed_policy_file = args.managed_policy_file;
            config.policy_file = args.policy_file;
            config.tool_profile = args.tool_profile;
            match args.transport {
                McpTransport::Stdio => {
                    if has_http_options {
                        return Err(CliError::InvalidConfiguration(
                            "HTTP-specific options are valid only with --transport http".to_owned(),
                        ));
                    }
                    nexus_cua_mcp::serve_stdio(config).await?;
                }
                McpTransport::Http => {
                    let token_file = args.http_token_file.as_deref().ok_or_else(|| {
                        CliError::InvalidConfiguration(
                            "--transport http requires --http-token-file".to_owned(),
                        )
                    })?;
                    let token = read_private_token(token_file)?;
                    let mut http = McpHttpConfig::new(token);
                    if let Some(value) = args.bind {
                        http.bind = value;
                    }
                    if let Some(value) = args.port {
                        http.port = value;
                    }
                    if let Some(value) = args.endpoint {
                        http.endpoint = value;
                    }
                    if let Some(value) = args.max_http_body_bytes {
                        http.max_request_body_bytes = value;
                    }
                    if let Some(value) = args.max_http_inflight_requests {
                        http.max_inflight_requests = value;
                    }
                    if let Some(value) = args.max_http_connections {
                        http.max_connections = value;
                    }
                    if let Some(value) = args.http_io_idle_timeout_seconds {
                        http.io_idle_timeout = std::time::Duration::from_secs(value);
                    }
                    if let Some(value) = args.http_connection_lifetime_seconds {
                        http.connection_lifetime = std::time::Duration::from_secs(value);
                    }
                    if let Some(value) = args.http_request_timeout_seconds {
                        http.request_timeout = std::time::Duration::from_secs(value);
                    }
                    if let Some(value) = args.http_response_mode {
                        http.response_mode = value;
                    }
                    nexus_cua_mcp::serve_http(config, http).await?;
                }
            }
            Ok(())
        }
    }
}

fn has_http_options(args: &McpServeArgs) -> bool {
    args.http_token_file.is_some()
        || args.bind.is_some()
        || args.port.is_some()
        || args.endpoint.is_some()
        || args.max_http_body_bytes.is_some()
        || args.max_http_inflight_requests.is_some()
        || args.max_http_connections.is_some()
        || args.http_io_idle_timeout_seconds.is_some()
        || args.http_connection_lifetime_seconds.is_some()
        || args.http_request_timeout_seconds.is_some()
        || args.http_response_mode.is_some()
}

fn version(args: &VersionArgs) -> Result<(), CliError> {
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&json!({
                "command": "nexuscua",
                "product": "Nexus Computer Use Runtime",
                "product_version": env!("CARGO_PKG_VERSION"),
                "protocol_version": PROTOCOL_VERSION,
            }))?
        );
    } else {
        println!("nexuscua {}", env!("CARGO_PKG_VERSION"));
    }
    Ok(())
}

async fn launch_registered_mcp(args: McpLaunchArgs) -> Result<(), CliError> {
    let state_root = default_state_root()?;
    let registrations = Arc::new(RegistrationStore::system(&state_root)?);
    let summary = registrations.inspect(&args.registration)?;
    if summary.product_version != env!("CARGO_PKG_VERSION") {
        return Err(CliError::InvalidConfiguration(
            "registration product version does not match this nexuscua binary".to_owned(),
        ));
    }
    let RegisteredAdapterProfile::Mcp {
        transport,
        tool_profile,
    } = summary.adapter_profile
    else {
        return Err(CliError::InvalidConfiguration(
            "registration is bound to the CLI adapter, not MCP".to_owned(),
        ));
    };
    let tool_profile = match tool_profile.as_str() {
        "full" => ToolProfile::Full,
        "vision" => ToolProfile::Vision,
        _ => {
            return Err(CliError::InvalidConfiguration(
                "registration contains an unsupported MCP tool profile".to_owned(),
            ));
        }
    };
    let provider = Arc::new(RegisteredAuthorityProvider::new(Arc::clone(&registrations)));
    let http_authorization = if transport == McpRegisteredTransport::StreamableHttp {
        Some(provider.http_authorization(&args.registration)?)
    } else {
        None
    };
    let mut config =
        McpServeConfig::provider(args.registration, provider, state_root.join("artifacts"));
    config.tool_profile = tool_profile;
    match transport {
        McpRegisteredTransport::Stdio => nexus_cua_mcp::serve_stdio(config).await?,
        McpRegisteredTransport::StreamableHttp => {
            let authorization = http_authorization.ok_or_else(|| {
                CliError::InvalidConfiguration(
                    "registered HTTP authorization is unavailable".to_owned(),
                )
            })?;
            nexus_cua_mcp::serve_http(config, McpHttpConfig::new(authorization)).await?;
        }
    }
    Ok(())
}

async fn serve(args: ServeArgs) -> Result<(), CliError> {
    let max_frame_bytes = args.max_frame_bytes;
    let policy = RuntimePolicyStack::from_files(
        args.managed_policy_file.as_deref(),
        args.policy_file.as_deref(),
    )?;
    let paths = resolve_service_paths(&args)?;
    let token = paths.prepare()?;
    let driver = nexus_cua_platform::system_driver()?;
    let mut runtime_config = RuntimeConfig::new(&paths.artifact_root);
    runtime_config.policy = policy;
    let runtime = Arc::new(Runtime::new(driver, runtime_config).map_err(CliError::Runtime)?);
    let mut server_config = ServerConfig::new(paths.endpoint.clone());
    server_config.max_frame_bytes = max_frame_bytes;
    let dispatcher = Arc::new(Dispatcher::new(
        Arc::clone(&runtime),
        &token,
        server_config.max_inflight_requests,
        server_config.max_completed_requests,
        server_config.completed_request_ttl,
        server_config.max_request_timeout_ms,
    )?);
    eprintln!("Nexus Computer Use endpoint: {}", paths.endpoint);
    eprintln!(
        "Nexus Computer Use token file: {}",
        paths.token_file.display()
    );
    let serve_result = nexus_cua_transport::serve_until(dispatcher, server_config, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await;
    runtime.shutdown().await;
    serve_result?;
    Ok(())
}

fn resolve_service_paths(args: &ServeArgs) -> Result<ServicePaths, CliError> {
    match (
        args.dev_root.as_deref(),
        args.endpoint.as_deref(),
        args.token_file.as_deref(),
        args.artifact_root.as_deref(),
    ) {
        (Some(root), None, None, None) => ServicePaths::development(root),
        (None, Some(endpoint), Some(token_file), Some(artifact_root)) => ServicePaths::explicit(
            endpoint.to_owned(),
            token_file.to_path_buf(),
            artifact_root.to_path_buf(),
        ),
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

fn write_or_print_schema(args: SchemaArgs) -> Result<(), CliError> {
    let request = schema_for!(RequestEnvelope);
    let response = schema_for!(nexus_cua_protocol::ResponseEnvelope);
    if let Some(output_dir) = args.output_dir {
        std::fs::create_dir_all(&output_dir)?;
        write_json(&output_dir.join("request.schema.json"), &request)?;
        write_json(&output_dir.join("response.schema.json"), &response)?;
    } else {
        let value = json!({
            "protocol_version": PROTOCOL_VERSION,
            "request": request,
            "response": response,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

fn write_json(path: &std::path::Path, value: &impl serde::Serialize) -> Result<(), CliError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)?;
    Ok(())
}

async fn send_request(args: RequestArgs) -> Result<(), CliError> {
    let token = read_private_token(&args.token_file)?;
    let command: Command = serde_json::from_slice(&read_command(&args)?)?;
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
    write_response(&response, args.compact)?;
    check_response(response.outcome)
}

fn write_response(value: &impl serde::Serialize, compact: bool) -> Result<(), CliError> {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    if compact {
        serde_json::to_writer(&mut stdout, value)?;
    } else {
        serde_json::to_writer_pretty(&mut stdout, value)?;
    }
    stdout.write_all(b"\n")?;
    Ok(())
}

fn check_response(outcome: nexus_cua_protocol::ResponseOutcome) -> Result<(), CliError> {
    match outcome {
        nexus_cua_protocol::ResponseOutcome::Success { .. } => Ok(()),
        nexus_cua_protocol::ResponseOutcome::Error { error } => Err(CliError::Runtime(error)),
    }
}

fn read_command(args: &RequestArgs) -> Result<Vec<u8>, CliError> {
    if args.command_stdin {
        return read_bounded(std::io::stdin());
    }
    let Some(path) = args.command_file.as_deref() else {
        return Err(CliError::InvalidConfiguration(
            "request requires --command-file or --command-stdin".to_owned(),
        ));
    };
    read_bounded_file(path)
}

fn read_bounded_file(path: &Path) -> Result<Vec<u8>, CliError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CliError::InvalidConfiguration(
            "command file must be a regular file, not a symlink".to_owned(),
        ));
    }
    if metadata.len() > MAX_COMMAND_BYTES {
        return Err(command_too_large());
    }
    read_bounded(std::fs::File::open(path)?)
}

fn read_bounded(reader: impl std::io::Read) -> Result<Vec<u8>, CliError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_COMMAND_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_COMMAND_BYTES {
        return Err(command_too_large());
    }
    Ok(bytes)
}

fn command_too_large() -> CliError {
    CliError::InvalidConfiguration("command JSON exceeds the 1 MiB limit".to_owned())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn bounded_command_reader_accepts_the_limit_and_rejects_one_more_byte() {
        let limit = usize::try_from(MAX_COMMAND_BYTES).expect("command limit fits usize");
        assert_eq!(
            read_bounded(Cursor::new(vec![b'x'; limit]))
                .expect("exact command limit")
                .len(),
            limit
        );
        let error = read_bounded(Cursor::new(vec![b'x'; limit + 1]))
            .expect_err("oversized command must fail");
        assert!(error.to_string().contains("1 MiB"));
    }

    #[test]
    fn command_file_rejects_symlinks() {
        let root = std::env::temp_dir().join(format!("nexus-cua-cli-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create test root");
        let command = root.join("command.json");
        std::fs::write(&command, br#"{"operation":"get_capabilities"}"#).expect("write command");
        assert!(
            !read_bounded_file(&command)
                .expect("regular command")
                .is_empty()
        );
        #[cfg(unix)]
        {
            let link = root.join("command-link.json");
            std::os::unix::fs::symlink(&command, &link).expect("create symlink");
            assert!(read_bounded_file(&link).is_err());
        }
        std::fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn mcp_unsigned_authority_requires_an_explicit_development_switch() {
        let without_switch = Cli::try_parse_from([
            "nexuscua",
            "mcp",
            "serve",
            "--development-authority-file",
            "authority.json",
            "--artifact-root",
            "artifacts",
        ]);
        assert!(without_switch.is_err());

        let with_switch = Cli::try_parse_from([
            "nexuscua",
            "mcp",
            "serve",
            "--development-authority-file",
            "authority.json",
            "--allow-unsigned-development-authority",
            "--artifact-root",
            "artifacts",
        ]);
        assert!(with_switch.is_ok());

        let removed_name = Cli::try_parse_from([
            "nexuscua",
            "mcp",
            "serve",
            "--authority-file",
            "authority.json",
            "--artifact-root",
            "artifacts",
        ]);
        assert!(removed_name.is_err());
    }

    #[test]
    fn stdio_can_detect_every_explicit_http_only_option() {
        let cli = Cli::try_parse_from([
            "nexuscua",
            "mcp",
            "serve",
            "--development-authority-file",
            "authority.json",
            "--allow-unsigned-development-authority",
            "--artifact-root",
            "artifacts",
            "--endpoint",
            "/mcp",
        ])
        .expect("parse explicit HTTP option");
        let CliCommand::Mcp(McpArgs {
            command: McpCommand::Serve(args),
        }) = cli.command
        else {
            panic!("expected MCP serve command");
        };
        assert!(has_http_options(&args));
    }

    #[test]
    fn version_is_a_first_class_public_command() {
        assert!(Cli::try_parse_from(["nexuscua", "version", "--json"]).is_ok());
    }

    #[test]
    fn registered_mcp_launch_exposes_only_a_non_secret_registration_id() {
        let launch = Cli::try_parse_from([
            "nexuscua",
            "mcp",
            "launch",
            "--registration",
            "registration_0123456789abcdef",
        ]);
        assert!(launch.is_ok());
        let secret_path = Cli::try_parse_from([
            "nexuscua",
            "mcp",
            "launch",
            "--registration",
            "registration_0123456789abcdef",
            "--authority-file",
            "authority.json",
        ]);
        assert!(secret_path.is_err());
    }

    #[test]
    fn authority_lifecycle_is_first_class_and_has_no_token_input() {
        let plan = Cli::try_parse_from([
            "nexuscua",
            "authority",
            "plan",
            "--interactive",
            "--host-executable",
            "/usr/bin/host",
            "--authority-policy-file",
            "authority.json",
            "--principal",
            "local-user",
            "--work-scope",
            "fixture-window",
        ]);
        assert!(plan.is_ok());
        let apply = Cli::try_parse_from([
            "nexuscua",
            "authority",
            "apply",
            "--plan",
            "authplan_0123456789abcdef",
            "--confirm",
        ]);
        assert!(apply.is_ok());
        let secret_input = Cli::try_parse_from([
            "nexuscua",
            "authority",
            "status",
            "--registration",
            "registration_0123456789abcdef",
            "--token-file",
            "token",
        ]);
        assert!(secret_input.is_err());
    }

    #[test]
    fn agent_binding_exposes_noun_actions_without_transport_secrets() {
        for command in [
            vec![
                "nexuscua",
                "service",
                "start",
                "--registration",
                "registration_0123456789abcdef",
            ],
            vec![
                "nexuscua",
                "session",
                "open",
                "--registration",
                "registration_0123456789abcdef",
            ],
            vec![
                "nexuscua",
                "session",
                "catalogue",
                "--registration",
                "registration_0123456789abcdef",
            ],
            vec![
                "nexuscua",
                "app",
                "list",
                "--registration",
                "registration_0123456789abcdef",
            ],
            vec![
                "nexuscua",
                "window",
                "list",
                "--registration",
                "registration_0123456789abcdef",
            ],
            vec![
                "nexuscua",
                "action",
                "click_normalized",
                "--registration",
                "registration_0123456789abcdef",
                "--input-stdin",
            ],
            vec![
                "nexuscua",
                "verify",
                "--registration",
                "registration_0123456789abcdef",
                "--input-stdin",
            ],
        ] {
            assert!(Cli::try_parse_from(command).is_ok());
        }
        assert!(
            Cli::try_parse_from([
                "nexuscua",
                "app",
                "list",
                "--registration",
                "registration_0123456789abcdef",
                "--token-file",
                "token",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "nexuscua",
                "window",
                "observe",
                "--registration",
                "registration_0123456789abcdef",
                "--window-ref",
                "window_ref",
                "--endpoint",
                "http://127.0.0.1:1/cli",
            ])
            .is_err()
        );
    }
}
