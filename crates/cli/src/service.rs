//! Host-bound lifecycle for the internal CLI adapter service.

use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Subcommand};
use nexus_cua_mcp::{
    McpHttpConfig, McpHttpReady, McpHttpResponseMode, McpServeConfig, McpServeError,
    current_unix_ms, serve_http_until,
};
use nexus_cua_protocol::AuthorizationToken;
use nexus_cua_provider::{
    AuthenticatedIntegrationStore, RegisteredAdapterProfile, RegisteredAuthorityProvider,
    RegistrationError, RegistrationStore, SecretStoreError, default_state_root,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, timeout};
use uuid::Uuid;
use zeroize::Zeroizing;

const SERVICE_SCHEMA_VERSION: u32 = 1;
const STOP_SCHEMA_VERSION: u32 = 1;
const STATUS_SCHEMA_VERSION: u32 = 1;
const SERVICE_ENDPOINT: &str = "/cli";
const REGISTRATION_POLL: Duration = Duration::from_millis(250);
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(125);
const MAX_HTTP_RESPONSE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 32 * 1024;

/// Internal CLI adapter service command group.
#[derive(Debug, Args)]
pub(crate) struct ServiceArgs {
    /// Service lifecycle operation.
    #[command(subcommand)]
    pub(crate) command: ServiceCommand,
}

/// Closed service lifecycle operations.
#[derive(Debug, Subcommand)]
pub(crate) enum ServiceCommand {
    /// Run one registered CLI adapter in the foreground until stopped or revoked.
    Start(ServiceStartArgs),
    /// Request bounded graceful shutdown of one registered CLI adapter.
    Stop(ServiceStopArgs),
    /// Inspect authenticated service state without exposing its capability.
    Status(ServiceStatusArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ServiceStartArgs {
    /// Non-secret CLI registration created by `authority apply`.
    #[arg(long)]
    registration: String,
    /// Emit compact JSON when the listener becomes ready.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ServiceStopArgs {
    /// Non-secret CLI registration owning the service.
    #[arg(long)]
    registration: String,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ServiceStatusArgs {
    /// Non-secret CLI registration owning the service.
    #[arg(long)]
    registration: String,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Error)]
pub(crate) enum ServiceError {
    #[error("CLI service input is invalid: {0}")]
    Invalid(String),
    #[error("CLI service is already running or its authenticated state is still live")]
    AlreadyRunning,
    #[error("CLI service is not running")]
    NotRunning,
    #[error("CLI service did not stop within the five-second bound")]
    StopTimeout,
    #[error("CLI service state no longer matches its registration")]
    StateDrift,
    #[error("CLI service returned an invalid HTTP or MCP response")]
    InvalidResponse,
    #[error("CLI service response exceeds the 20 MiB bound")]
    ResponseTooLarge,
    #[error("CLI service returned HTTP status {0}")]
    HttpStatus(u16),
    #[error("CLI service I/O failed")]
    Io(#[from] std::io::Error),
    #[error("CLI service JSON failed")]
    Json(#[from] serde_json::Error),
    #[error("registered CLI authority failed")]
    Provider(#[from] RegistrationError),
    #[error(transparent)]
    Mcp(#[from] McpServeError),
    #[error("CLI service clock is unavailable")]
    Clock(#[from] nexus_cua_mcp::AuthorityError),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceRecord {
    schema_version: u32,
    registration_id: String,
    instance_id: String,
    product_version: String,
    started_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    address: SocketAddr,
    endpoint: String,
    stop_ref: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StopRequest {
    schema_version: u32,
    registration_id: String,
    instance_id: String,
    requested_at_unix_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ServiceStatus {
    schema_version: u32,
    state: &'static str,
    registration_id: String,
    instance_id: Option<String>,
    product_version: &'static str,
    endpoint: Option<String>,
    started_at_unix_ms: Option<u64>,
    expires_at_unix_ms: Option<u64>,
}

pub(crate) async fn run(args: ServiceArgs) -> Result<(), ServiceError> {
    match args.command {
        ServiceCommand::Start(args) => start(&args).await,
        ServiceCommand::Stop(args) => stop(&args).await,
        ServiceCommand::Status(args) => status(&args).await,
    }
}

async fn start(args: &ServiceStartArgs) -> Result<(), ServiceError> {
    let state_root = default_state_root()?;
    let registrations = Arc::new(RegistrationStore::system(&state_root)?);
    let authority_ttl_seconds = validate_cli_registration(&registrations, &args.registration)?;
    let provider = Arc::new(RegisteredAuthorityProvider::new(Arc::clone(&registrations)));
    let authorization = provider.cli_authorization(&args.registration)?;
    let states = Arc::new(AuthenticatedIntegrationStore::system(&state_root)?);
    let state_ref = service_ref(&args.registration);
    clear_stale_service(&states, &state_ref).await?;

    let instance_id = format!("service_{}", Uuid::new_v4().simple());
    let stop_ref = format!("svstop_{}", Uuid::new_v4().simple());
    let started_at_unix_ms = current_unix_ms()?;
    let lease_duration = Duration::from_secs(u64::from(authority_ttl_seconds));
    let expires_at_unix_ms =
        started_at_unix_ms.saturating_add(u64::from(authority_ttl_seconds).saturating_mul(1_000));
    let lease_deadline = Instant::now() + lease_duration;
    let registration_id = args.registration.clone();
    let ready_states = Arc::clone(&states);
    let ready_state_ref = state_ref.clone();
    let ready_instance_id = instance_id.clone();
    let ready_stop_ref = stop_ref.clone();
    let compact = args.json;
    let on_ready = move |ready: McpHttpReady| {
        let record = ServiceRecord {
            schema_version: SERVICE_SCHEMA_VERSION,
            registration_id: registration_id.clone(),
            instance_id: ready_instance_id,
            product_version: env!("CARGO_PKG_VERSION").to_owned(),
            started_at_unix_ms,
            expires_at_unix_ms,
            address: ready.address,
            endpoint: ready.endpoint,
            stop_ref: ready_stop_ref,
        };
        install_state(&ready_states, &ready_state_ref, &record).map_err(|_| {
            McpServeError::InvalidConfiguration(
                "authenticated CLI service state could not be published".to_owned(),
            )
        })?;
        write_output(
            &service_status("running", &registration_id, Some(&record)),
            compact,
        )
        .map_err(|_| {
            McpServeError::InvalidConfiguration(
                "CLI service readiness output could not be written".to_owned(),
            )
        })?;
        std::io::stdout().flush().map_err(McpServeError::Http)
    };

    let monitor_states = Arc::clone(&states);
    let monitor_registrations = Arc::clone(&registrations);
    let monitor_registration = args.registration.clone();
    let monitor_stop_ref = stop_ref.clone();
    let shutdown = async move {
        let mut checks = interval(REGISTRATION_POLL);
        checks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let lease_expiry = tokio::time::sleep_until(lease_deadline);
        tokio::pin!(lease_expiry);
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                () = &mut lease_expiry => break,
                _ = checks.tick() => {
                    if monitor_states.load(&monitor_stop_ref).is_ok()
                        || monitor_registrations.inspect(&monitor_registration).is_err()
                    {
                        break;
                    }
                }
            }
        }
    };

    let mut http = McpHttpConfig::new(authorization);
    http.bind = IpAddr::V4(Ipv4Addr::LOCALHOST);
    http.port = 0;
    SERVICE_ENDPOINT.clone_into(&mut http.endpoint);
    http.response_mode = McpHttpResponseMode::Json;
    let config = McpServeConfig::provider(
        args.registration.clone(),
        provider,
        state_root.join("cli-artifacts").join(&args.registration),
    );
    let result = serve_http_until(config, http, on_ready, shutdown).await;
    let cleanup = cleanup_service_state(&states, &state_ref, &stop_ref);
    result?;
    cleanup
}

async fn stop(args: &ServiceStopArgs) -> Result<(), ServiceError> {
    let result = stop_registered(&args.registration).await?;
    write_output(&result, args.json)
}

pub(crate) async fn stop_registered(registration_id: &str) -> Result<Value, ServiceError> {
    let state_root = default_state_root()?;
    let registrations = Arc::new(RegistrationStore::system(&state_root)?);
    validate_cli_registration(&registrations, registration_id)?;
    let provider = RegisteredAuthorityProvider::new(registrations);
    let _authorization = provider.cli_authorization(registration_id)?;
    let states = AuthenticatedIntegrationStore::system(&state_root)?;
    let state_ref = service_ref(registration_id);
    let record = load_service(&states, &state_ref)?.ok_or(ServiceError::NotRunning)?;
    validate_service(&record, registration_id)?;
    let request = StopRequest {
        schema_version: STOP_SCHEMA_VERSION,
        registration_id: registration_id.to_owned(),
        instance_id: record.instance_id.clone(),
        requested_at_unix_ms: current_unix_ms()?,
    };
    match load_stop(&states, &record.stop_ref)? {
        Some(existing) if valid_stop(&existing, &record) => {}
        Some(_) => return Err(ServiceError::StateDrift),
        None => install_state(&states, &record.stop_ref, &request)?,
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if load_service(&states, &state_ref)?.is_none() {
            return serde_json::to_value(service_status("stopped", registration_id, None))
                .map_err(Into::into);
        }
        sleep(Duration::from_millis(50)).await;
    }
    Err(ServiceError::StopTimeout)
}

pub(crate) async fn call_tool(
    registration_id: &str,
    tool: &str,
    arguments: Value,
) -> Result<Value, ServiceError> {
    if tool.is_empty()
        || tool.len() > 64
        || !tool
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        || !arguments.is_object()
    {
        return Err(ServiceError::Invalid(
            "tool name or arguments are outside the closed CLI binding".to_owned(),
        ));
    }
    let (record, authorization) = service_connection(registration_id)?;
    call_http_tool(&record, &authorization, tool, arguments).await
}

pub(crate) async fn list_tools(registration_id: &str) -> Result<Value, ServiceError> {
    let (record, authorization) = service_connection(registration_id)?;
    call_http_request(
        &record,
        &authorization,
        "tools/list",
        None,
        json!({"_meta": protocol_metadata()}),
    )
    .await
}

fn service_connection(
    registration_id: &str,
) -> Result<(ServiceRecord, AuthorizationToken), ServiceError> {
    let state_root = default_state_root()?;
    let registrations = Arc::new(RegistrationStore::system(&state_root)?);
    validate_cli_registration(&registrations, registration_id)?;
    let provider = RegisteredAuthorityProvider::new(registrations);
    let authorization = provider.cli_authorization(registration_id)?;
    let states = AuthenticatedIntegrationStore::system(&state_root)?;
    let record =
        load_service(&states, &service_ref(registration_id))?.ok_or(ServiceError::NotRunning)?;
    validate_service(&record, registration_id)?;
    Ok((record, authorization))
}

pub(crate) async fn wait_for_registration_shutdown(
    registration_id: &str,
) -> Result<(), ServiceError> {
    let state_root = default_state_root()?;
    let states = AuthenticatedIntegrationStore::system(&state_root)?;
    let state_ref = service_ref(registration_id);
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        let Some(record) = load_service(&states, &state_ref)? else {
            return Ok(());
        };
        validate_service(&record, registration_id)?;
        let listening = timeout(PROBE_TIMEOUT, TcpStream::connect(record.address))
            .await
            .is_ok_and(|result| result.is_ok());
        if !listening {
            cleanup_service_state(&states, &state_ref, &record.stop_ref)?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ServiceError::StopTimeout);
        }
        sleep(Duration::from_millis(50)).await;
    }
}

async fn call_http_tool(
    record: &ServiceRecord,
    authorization: &AuthorizationToken,
    tool: &str,
    arguments: Value,
) -> Result<Value, ServiceError> {
    call_http_request(
        record,
        authorization,
        "tools/call",
        Some(tool),
        json!({
            "_meta": protocol_metadata(),
            "name": tool,
            "arguments": arguments
        }),
    )
    .await
}

async fn call_http_request(
    record: &ServiceRecord,
    authorization: &AuthorizationToken,
    method: &str,
    name: Option<&str>,
    params: Value,
) -> Result<Value, ServiceError> {
    let request_id = format!("cli_{}", Uuid::new_v4().simple());
    let body = Zeroizing::new(serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": params
    }))?);
    let name_header = name
        .map(|value| format!("Mcp-Name: {value}\r\n"))
        .unwrap_or_default();
    let mut request = Zeroizing::new(format!(
        "POST {SERVICE_ENDPOINT} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nAccept: application/json\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: {method}\r\n{name_header}Connection: close\r\nContent-Length: {}\r\n\r\n",
        record.address,
        authorization.expose(),
        body.len()
    ));
    request.push_str(std::str::from_utf8(&body).map_err(|_| ServiceError::InvalidResponse)?);

    let response = timeout(CALL_TIMEOUT, async {
        let mut stream = TcpStream::connect(record.address).await?;
        stream.write_all(request.as_bytes()).await?;
        stream.shutdown().await?;
        let mut response = Zeroizing::new(Vec::new());
        stream
            .take(MAX_HTTP_RESPONSE_BYTES.saturating_add(1))
            .read_to_end(&mut response)
            .await?;
        Ok::<_, std::io::Error>(response)
    })
    .await
    .map_err(|_| ServiceError::InvalidResponse)??;
    if response.len() as u64 > MAX_HTTP_RESPONSE_BYTES {
        return Err(ServiceError::ResponseTooLarge);
    }
    decode_http_response(&response, &request_id)
}

fn protocol_metadata() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
            "name": "nexuscua-cli",
            "version": env!("CARGO_PKG_VERSION")
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn decode_http_response(bytes: &[u8], request_id: &str) -> Result<Value, ServiceError> {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or(ServiceError::InvalidResponse)?;
    if split > MAX_HTTP_HEADER_BYTES {
        return Err(ServiceError::InvalidResponse);
    }
    let headers =
        std::str::from_utf8(&bytes[..split]).map_err(|_| ServiceError::InvalidResponse)?;
    let body = &bytes[split + 4..];
    let mut lines = headers.split("\r\n");
    let status = lines.next().ok_or(ServiceError::InvalidResponse)?;
    let status_code = status
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(ServiceError::InvalidResponse)?;
    if status_code != 200 {
        return Err(ServiceError::HttpStatus(status_code));
    }
    let mut content_length = None;
    let mut content_type_valid = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(ServiceError::InvalidResponse)?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(ServiceError::InvalidResponse);
            }
            content_length = value.parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type_valid = value
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(ServiceError::InvalidResponse);
        }
    }
    if !content_type_valid || content_length != Some(body.len()) {
        return Err(ServiceError::InvalidResponse);
    }
    let value: Value = serde_json::from_slice(body)?;
    if value.get("jsonrpc") != Some(&Value::String("2.0".to_owned()))
        || value.get("id") != Some(&Value::String(request_id.to_owned()))
        || value.get("error").is_some()
    {
        return Err(ServiceError::InvalidResponse);
    }
    value
        .get("result")
        .cloned()
        .ok_or(ServiceError::InvalidResponse)
}

async fn status(args: &ServiceStatusArgs) -> Result<(), ServiceError> {
    let state_root = default_state_root()?;
    let registrations = RegistrationStore::system(&state_root)?;
    let states = AuthenticatedIntegrationStore::system(&state_root)?;
    let Some(record) = load_service(&states, &service_ref(&args.registration))? else {
        write_output(
            &service_status("stopped", &args.registration, None),
            args.json,
        )?;
        return Ok(());
    };
    validate_service(&record, &args.registration)?;
    let registration_active = match registrations.inspect(&args.registration) {
        Ok(summary) => {
            validate_cli_summary(&summary)?;
            true
        }
        Err(error) if is_missing_registration(&error) => false,
        Err(error) => return Err(error.into()),
    };
    let listening = timeout(PROBE_TIMEOUT, TcpStream::connect(record.address))
        .await
        .is_ok_and(|result| result.is_ok());
    let state = if registration_active && listening {
        "running"
    } else {
        "stopping_or_stale"
    };
    write_output(
        &service_status(state, &args.registration, Some(&record)),
        args.json,
    )
}

fn validate_cli_registration(
    registrations: &RegistrationStore,
    registration_id: &str,
) -> Result<u32, ServiceError> {
    let summary = registrations.inspect(registration_id)?;
    validate_cli_summary(&summary)?;
    Ok(summary.authority_ttl_seconds)
}

fn validate_cli_summary(
    summary: &nexus_cua_provider::RegistrationSummary,
) -> Result<(), ServiceError> {
    if summary.product_version != env!("CARGO_PKG_VERSION")
        || summary.adapter_profile != RegisteredAdapterProfile::Cli
    {
        return Err(ServiceError::Invalid(
            "registration is not bound to this CLI adapter version".to_owned(),
        ));
    }
    Ok(())
}

async fn clear_stale_service(
    states: &AuthenticatedIntegrationStore,
    state_ref: &str,
) -> Result<(), ServiceError> {
    let Some(record) = load_service(states, state_ref)? else {
        return Ok(());
    };
    validate_service(&record, &record.registration_id)?;
    if timeout(PROBE_TIMEOUT, TcpStream::connect(record.address))
        .await
        .is_ok_and(|result| result.is_ok())
    {
        return Err(ServiceError::AlreadyRunning);
    }
    cleanup_service_state(states, state_ref, &record.stop_ref)
}

fn cleanup_service_state(
    states: &AuthenticatedIntegrationStore,
    state_ref: &str,
    stop_ref: &str,
) -> Result<(), ServiceError> {
    states.remove(state_ref)?;
    states.remove(stop_ref)?;
    Ok(())
}

fn load_service(
    states: &AuthenticatedIntegrationStore,
    state_ref: &str,
) -> Result<Option<ServiceRecord>, ServiceError> {
    match states.load(state_ref) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(Into::into),
        Err(RegistrationError::NotFound) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn load_stop(
    states: &AuthenticatedIntegrationStore,
    stop_ref: &str,
) -> Result<Option<StopRequest>, ServiceError> {
    match states.load(stop_ref) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(Into::into),
        Err(RegistrationError::NotFound) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn validate_service(record: &ServiceRecord, registration_id: &str) -> Result<(), ServiceError> {
    if record.schema_version != SERVICE_SCHEMA_VERSION
        || record.registration_id != registration_id
        || record.product_version != env!("CARGO_PKG_VERSION")
        || record.expires_at_unix_ms <= record.started_at_unix_ms
        || record
            .expires_at_unix_ms
            .saturating_sub(record.started_at_unix_ms)
            > 60 * 60 * 1_000
        || !record.instance_id.starts_with("service_")
        || !record.stop_ref.starts_with("svstop_")
        || !record.address.ip().is_loopback()
        || record.address.port() == 0
        || record.endpoint != format!("http://{}{SERVICE_ENDPOINT}", record.address)
    {
        return Err(ServiceError::StateDrift);
    }
    Ok(())
}

fn valid_stop(request: &StopRequest, record: &ServiceRecord) -> bool {
    request.schema_version == STOP_SCHEMA_VERSION
        && request.registration_id == record.registration_id
        && request.instance_id == record.instance_id
}

fn is_missing_registration(error: &RegistrationError) -> bool {
    matches!(
        error,
        RegistrationError::NotFound | RegistrationError::Secret(SecretStoreError::NotFound)
    )
}

fn service_status(
    state: &'static str,
    registration_id: &str,
    record: Option<&ServiceRecord>,
) -> ServiceStatus {
    ServiceStatus {
        schema_version: STATUS_SCHEMA_VERSION,
        state,
        registration_id: registration_id.to_owned(),
        instance_id: record.map(|value| value.instance_id.clone()),
        product_version: env!("CARGO_PKG_VERSION"),
        endpoint: record.map(|value| value.endpoint.clone()),
        started_at_unix_ms: record.map(|value| value.started_at_unix_ms),
        expires_at_unix_ms: record.map(|value| value.expires_at_unix_ms),
    }
}

fn service_ref(registration_id: &str) -> String {
    let value = hex::encode(Sha256::digest(registration_id.as_bytes()));
    format!("clisvc_{}", &value[..32])
}

fn install_state(
    states: &AuthenticatedIntegrationStore,
    state_ref: &str,
    value: &impl Serialize,
) -> Result<(), ServiceError> {
    let bytes = Zeroizing::new(serde_json::to_vec(value)?);
    states.install(state_ref, &bytes)?;
    Ok(())
}

fn write_output(value: &impl Serialize, compact: bool) -> Result<(), ServiceError> {
    if compact {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;

    fn record() -> ServiceRecord {
        ServiceRecord {
            schema_version: SERVICE_SCHEMA_VERSION,
            registration_id: "registration_0123456789abcdef".to_owned(),
            instance_id: "service_0123456789abcdef".to_owned(),
            product_version: env!("CARGO_PKG_VERSION").to_owned(),
            started_at_unix_ms: 1_000,
            expires_at_unix_ms: 61_000,
            address: "127.0.0.1:43123".parse().expect("loopback address"),
            endpoint: "http://127.0.0.1:43123/cli".to_owned(),
            stop_ref: "svstop_0123456789abcdef".to_owned(),
        }
    }

    #[test]
    fn service_record_requires_exact_loopback_endpoint() {
        let mut value = record();
        validate_service(&value, &value.registration_id).expect("valid record");
        value.endpoint = "http://example.com/cli".to_owned();
        assert!(matches!(
            validate_service(&value, &value.registration_id),
            Err(ServiceError::StateDrift)
        ));
    }

    #[test]
    fn stop_request_is_bound_to_one_service_instance() {
        let value = record();
        let mut request = StopRequest {
            schema_version: STOP_SCHEMA_VERSION,
            registration_id: value.registration_id.clone(),
            instance_id: value.instance_id.clone(),
            requested_at_unix_ms: 1_001,
        };
        assert!(valid_stop(&request, &value));
        request.instance_id = "service_other".to_owned();
        assert!(!valid_stop(&request, &value));
    }

    #[test]
    fn service_reference_does_not_embed_registration_identity() {
        let registration = "registration_0123456789abcdef";
        let value = service_ref(registration);
        assert!(value.starts_with("clisvc_"));
        assert!(!value.contains(registration));
        assert_eq!(value.len(), 39);
    }

    #[tokio::test]
    async fn bounded_http_client_authenticates_and_decodes_one_json_tool_result() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept CLI client");
            let mut request = Vec::new();
            stream
                .read_to_end(&mut request)
                .await
                .expect("read CLI request");
            let request_text = std::str::from_utf8(&request).expect("HTTP request text");
            assert!(
                request_text.contains("Authorization: Bearer aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            );
            assert!(request_text.contains("Mcp-Name: list_apps"));
            let body = request_text.split_once("\r\n\r\n").expect("request body").1;
            let request_json: Value = serde_json::from_str(body).expect("request JSON");
            let response_body = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": request_json["id"],
                "result": {
                    "content": [],
                    "structuredContent": [{"application_id": "fixture"}],
                    "isError": false
                }
            }))
            .expect("response JSON");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("write headers");
            stream
                .write_all(&response_body)
                .await
                .expect("write response");
        });
        let mut service = record();
        service.address = address;
        service.endpoint = format!("http://{address}/cli");
        let result = call_http_tool(
            &service,
            &AuthorizationToken::new("a".repeat(32)),
            "list_apps",
            json!({}),
        )
        .await
        .expect("tool result");
        assert_eq!(result["structuredContent"][0]["application_id"], "fixture");
        server.await.expect("fixture server");
    }

    #[tokio::test]
    async fn bounded_http_client_lists_authoritative_tool_schemas_without_name_header() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept CLI client");
            let mut request = Vec::new();
            stream
                .read_to_end(&mut request)
                .await
                .expect("read CLI request");
            let request_text = std::str::from_utf8(&request).expect("HTTP request text");
            assert!(request_text.contains("Mcp-Method: tools/list"));
            assert!(!request_text.contains("Mcp-Name:"));
            let body = request_text.split_once("\r\n\r\n").expect("request body").1;
            let request_json: Value = serde_json::from_str(body).expect("request JSON");
            assert_eq!(request_json["method"], "tools/list");
            let response_body = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": request_json["id"],
                "result": {
                    "tools": [{
                        "name": "nexuscua_observe_window",
                        "inputSchema": {"type": "object"}
                    }]
                }
            }))
            .expect("response JSON");
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response_body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("write headers");
            stream
                .write_all(&response_body)
                .await
                .expect("write response");
        });
        let mut service = record();
        service.address = address;
        service.endpoint = format!("http://{address}/cli");
        let result = call_http_request(
            &service,
            &AuthorizationToken::new("a".repeat(32)),
            "tools/list",
            None,
            json!({"_meta": protocol_metadata()}),
        )
        .await
        .expect("tool catalogue");
        assert_eq!(result["tools"][0]["name"], "nexuscua_observe_window");
        server.await.expect("fixture server");
    }

    #[test]
    fn http_decoder_rejects_chunked_or_mismatched_responses() {
        let chunked = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
        assert!(matches!(
            decode_http_response(chunked, "cli_request"),
            Err(ServiceError::InvalidResponse)
        ));
        let wrong_id_body = br#"{"jsonrpc":"2.0","id":"other","result":{}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            wrong_id_body.len(),
            std::str::from_utf8(wrong_id_body).expect("response body")
        );
        assert!(matches!(
            decode_http_response(response.as_bytes(), "cli_request"),
            Err(ServiceError::InvalidResponse)
        ));
    }
}
