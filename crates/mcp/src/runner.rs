//! Shared MCP adapter bootstrap used by every public transport.

use std::future::Future;
use std::io::{self, IoSlice};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::serve::Listener;
use clap::ValueEnum;
use nexus_cua_protocol::AuthorizationToken;
use nexus_cua_runtime::{DriverError, Runtime, RuntimeConfig, RuntimePolicyStack};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ServiceExt, transport::stdio};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, Sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::{
    AuthorityChallenge, AuthorityContext, AuthorityError, AuthorityPolicy, AuthorityProvider,
    BootstrapError, NexusCuaMcp, ToolProfile, current_unix_ms,
};

/// Default maximum PNG returned in one MCP image block.
pub const DEFAULT_MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Hard ceiling for a single MCP image block.
pub const HARD_MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
/// Default lifetime of a guarded observation while a model is thinking.
pub const DEFAULT_MAX_OBSERVATION_AGE_SECONDS: u64 = 120;
/// Hard ceiling for a guarded observation lifetime.
pub const HARD_MAX_OBSERVATION_AGE_SECONDS: u64 = 300;
const DEFAULT_MAX_HTTP_BODY_BYTES: usize = 4 * 1024 * 1024;
const HARD_MAX_HTTP_BODY_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_HTTP_INFLIGHT_REQUESTS: usize = 64;
const HARD_MAX_HTTP_INFLIGHT_REQUESTS: usize = 1_024;
const DEFAULT_MAX_HTTP_CONNECTIONS: usize = 64;
const HARD_MAX_HTTP_CONNECTIONS: usize = 1_024;
const DEFAULT_HTTP_IO_IDLE_TIMEOUT_SECONDS: u64 = 15;
const HARD_HTTP_IO_IDLE_TIMEOUT_SECONDS: u64 = 120;
const DEFAULT_HTTP_CONNECTION_LIFETIME_SECONDS: u64 = 180;
const HARD_HTTP_CONNECTION_LIFETIME_SECONDS: u64 = 600;
const DEFAULT_HTTP_REQUEST_TIMEOUT_SECONDS: u64 = 120;
const HARD_HTTP_REQUEST_TIMEOUT_SECONDS: u64 = 180;
const MAX_HTTP_ENDPOINT_BYTES: usize = 128;

/// Streamable HTTP response representation for one request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum McpHttpResponseMode {
    /// Prefer one `application/json` response when the handler emits one terminal message.
    Json,
    /// Return the same response through one request-scoped SSE stream.
    RequestScopedSse,
}

/// Authenticated provider or explicit unsigned source-build authority fixture.
#[derive(Clone)]
pub enum AuthoritySource {
    /// Source-build-only unsigned file; never a production trust root.
    UnsignedDevelopmentFile(PathBuf),
    /// Registered provider reached through its authenticated protected channel.
    Provider {
        /// Non-secret registration selected by the trusted launcher.
        registration_id: String,
        /// Provider client that authenticates the protected channel.
        provider: Arc<dyn AuthorityProvider>,
    },
}

impl std::fmt::Debug for AuthoritySource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsignedDevelopmentFile(_) => {
                formatter.write_str("AuthoritySource::UnsignedDevelopmentFile([REDACTED])")
            }
            Self::Provider {
                registration_id, ..
            } => formatter
                .debug_struct("AuthoritySource::Provider")
                .field("registration_id", registration_id)
                .field("provider", &"[AUTHENTICATED CHANNEL]")
                .finish(),
        }
    }
}

/// Host-owned configuration shared by stdio and Streamable HTTP adapters.
#[derive(Clone, Debug)]
pub struct McpServeConfig {
    /// Authenticated authority source fixed before tool publication.
    pub authority: AuthoritySource,
    /// Private directory for transient screenshot artifacts.
    pub artifact_root: PathBuf,
    /// Largest PNG returned as one MCP image block.
    pub max_image_bytes: u64,
    /// Maximum age of an observation used to guard a mutation.
    pub max_observation_age_seconds: u64,
    /// Optional administrator-owned immutable runtime policy.
    pub managed_policy_file: Option<PathBuf>,
    /// Optional user-owned immutable runtime policy.
    pub policy_file: Option<PathBuf>,
    /// Immutable model-facing tool catalogue profile.
    pub tool_profile: ToolProfile,
}

impl McpServeConfig {
    /// Creates a source-build-only configuration using an unsigned fixture.
    pub fn unsigned_development(authority_file: PathBuf, artifact_root: PathBuf) -> Self {
        Self {
            authority: AuthoritySource::UnsignedDevelopmentFile(authority_file),
            artifact_root,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_observation_age_seconds: DEFAULT_MAX_OBSERVATION_AGE_SECONDS,
            managed_policy_file: None,
            policy_file: None,
            tool_profile: ToolProfile::Full,
        }
    }

    /// Creates a production configuration backed by an authenticated provider client.
    pub fn provider(
        registration_id: impl Into<String>,
        provider: Arc<dyn AuthorityProvider>,
        artifact_root: PathBuf,
    ) -> Self {
        Self {
            authority: AuthoritySource::Provider {
                registration_id: registration_id.into(),
                provider,
            },
            artifact_root,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_observation_age_seconds: DEFAULT_MAX_OBSERVATION_AGE_SECONDS,
            managed_policy_file: None,
            policy_file: None,
            tool_profile: ToolProfile::Full,
        }
    }

    fn validate(&self) -> Result<(), McpServeError> {
        if self.max_image_bytes == 0 || self.max_image_bytes > HARD_MAX_IMAGE_BYTES {
            return Err(McpServeError::InvalidConfiguration(
                "max-image-bytes must be between 1 and 33554432".to_owned(),
            ));
        }
        if self.max_observation_age_seconds == 0
            || self.max_observation_age_seconds > HARD_MAX_OBSERVATION_AGE_SECONDS
        {
            return Err(McpServeError::InvalidConfiguration(
                "max-observation-age-seconds must be between 1 and 300".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Loopback-only Streamable HTTP listener configuration.
#[derive(Clone, Debug)]
pub struct McpHttpConfig {
    /// Local bind address. M3 intentionally accepts loopback addresses only.
    pub bind: IpAddr,
    /// Listener port; zero asks the OS for an available port.
    pub port: u16,
    /// Single Streamable HTTP endpoint.
    pub endpoint: String,
    /// Host-owned Bearer capability checked before MCP dispatch.
    pub authorization: AuthorizationToken,
    /// Maximum streamed HTTP POST body.
    pub max_request_body_bytes: usize,
    /// Maximum concurrently admitted authenticated HTTP requests.
    pub max_inflight_requests: usize,
    /// Maximum TCP connections accepted by this adapter process.
    pub max_connections: usize,
    /// Maximum read/write inactivity on one accepted connection.
    pub io_idle_timeout: Duration,
    /// Absolute lifetime of one accepted connection.
    pub connection_lifetime: Duration,
    /// Maximum duration of one authenticated request including its streamed body.
    pub request_timeout: Duration,
    /// JSON or request-scoped SSE response representation.
    pub response_mode: McpHttpResponseMode,
}

/// Non-secret endpoint published only after the bounded HTTP listener is ready.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpHttpReady {
    /// Exact loopback socket selected by the operating system.
    pub address: SocketAddr,
    /// Complete current Streamable HTTP endpoint URL.
    pub endpoint: String,
}

impl McpHttpConfig {
    /// Creates a loopback, dynamic-port `/mcp` listener configuration.
    pub fn new(authorization: AuthorizationToken) -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
            endpoint: "/mcp".to_owned(),
            authorization,
            max_request_body_bytes: DEFAULT_MAX_HTTP_BODY_BYTES,
            max_inflight_requests: DEFAULT_MAX_HTTP_INFLIGHT_REQUESTS,
            max_connections: DEFAULT_MAX_HTTP_CONNECTIONS,
            io_idle_timeout: Duration::from_secs(DEFAULT_HTTP_IO_IDLE_TIMEOUT_SECONDS),
            connection_lifetime: Duration::from_secs(DEFAULT_HTTP_CONNECTION_LIFETIME_SECONDS),
            request_timeout: Duration::from_secs(DEFAULT_HTTP_REQUEST_TIMEOUT_SECONDS),
            response_mode: McpHttpResponseMode::Json,
        }
    }

    fn validate(&self) -> Result<(), McpServeError> {
        if !self.bind.is_loopback() {
            return Err(McpServeError::InvalidConfiguration(
                "HTTP bind must be loopback; remote bind requires a future explicit TLS profile"
                    .to_owned(),
            ));
        }
        if !valid_endpoint(&self.endpoint) {
            return Err(McpServeError::InvalidConfiguration(
                "HTTP endpoint must be one normalized absolute path of at most 128 bytes"
                    .to_owned(),
            ));
        }
        if self.authorization.expose().len() < 32 || self.authorization.expose().len() > 4 * 1024 {
            return Err(McpServeError::InvalidConfiguration(
                "HTTP authorization must contain 32 to 4096 bytes".to_owned(),
            ));
        }
        if self.max_request_body_bytes == 0
            || self.max_request_body_bytes > HARD_MAX_HTTP_BODY_BYTES
        {
            return Err(McpServeError::InvalidConfiguration(format!(
                "max HTTP body bytes must be between 1 and {HARD_MAX_HTTP_BODY_BYTES}"
            )));
        }
        if self.max_inflight_requests == 0
            || self.max_inflight_requests > HARD_MAX_HTTP_INFLIGHT_REQUESTS
        {
            return Err(McpServeError::InvalidConfiguration(format!(
                "max HTTP in-flight requests must be between 1 and {HARD_MAX_HTTP_INFLIGHT_REQUESTS}"
            )));
        }
        if self.max_connections == 0 || self.max_connections > HARD_MAX_HTTP_CONNECTIONS {
            return Err(McpServeError::InvalidConfiguration(format!(
                "max HTTP connections must be between 1 and {HARD_MAX_HTTP_CONNECTIONS}"
            )));
        }
        if self.max_inflight_requests > self.max_connections {
            return Err(McpServeError::InvalidConfiguration(
                "max HTTP in-flight requests cannot exceed max HTTP connections".to_owned(),
            ));
        }
        validate_duration_seconds(
            self.io_idle_timeout,
            HARD_HTTP_IO_IDLE_TIMEOUT_SECONDS,
            "HTTP I/O idle timeout",
        )?;
        validate_duration_seconds(
            self.connection_lifetime,
            HARD_HTTP_CONNECTION_LIFETIME_SECONDS,
            "HTTP connection lifetime",
        )?;
        validate_duration_seconds(
            self.request_timeout,
            HARD_HTTP_REQUEST_TIMEOUT_SECONDS,
            "HTTP request timeout",
        )?;
        if self.request_timeout > self.connection_lifetime {
            return Err(McpServeError::InvalidConfiguration(
                "HTTP request timeout cannot exceed connection lifetime".to_owned(),
            ));
        }
        Ok(())
    }
}

fn validate_duration_seconds(
    value: Duration,
    hard_max_seconds: u64,
    label: &str,
) -> Result<(), McpServeError> {
    if value < Duration::from_secs(1)
        || value > Duration::from_secs(hard_max_seconds)
        || value.subsec_nanos() != 0
    {
        return Err(McpServeError::InvalidConfiguration(format!(
            "{label} must be a whole number of seconds between 1 and {hard_max_seconds}"
        )));
    }
    Ok(())
}

#[derive(Clone)]
struct HttpRequestGuard {
    authorization: Arc<AuthorizationToken>,
    inflight: Arc<Semaphore>,
    request_timeout: Duration,
}

struct BoundedTcpListener {
    inner: TcpListener,
    permits: Arc<Semaphore>,
    io_idle_timeout: Duration,
    connection_lifetime: Duration,
    cancellation: CancellationToken,
}

struct BoundedHttpIo {
    inner: TcpStream,
    _permit: OwnedSemaphorePermit,
    io_idle_timeout: Duration,
    read_idle: Pin<Box<Sleep>>,
    write_idle: Pin<Box<Sleep>>,
    read_lifetime: Pin<Box<Sleep>>,
    write_lifetime: Pin<Box<Sleep>>,
    read_cancellation: Pin<Box<dyn Future<Output = ()> + Send>>,
    write_cancellation: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl BoundedTcpListener {
    fn new(inner: TcpListener, http: &McpHttpConfig, cancellation: CancellationToken) -> Self {
        Self {
            inner,
            permits: Arc::new(Semaphore::new(http.max_connections)),
            io_idle_timeout: http.io_idle_timeout,
            connection_lifetime: http.connection_lifetime,
            cancellation,
        }
    }
}

impl Listener for BoundedTcpListener {
    type Io = BoundedHttpIo;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .expect("HTTP connection semaphore is never closed");
        let (stream, address) = Listener::accept(&mut self.inner).await;
        (
            BoundedHttpIo::new(
                stream,
                permit,
                self.io_idle_timeout,
                self.connection_lifetime,
                self.cancellation.clone(),
            ),
            address,
        )
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

impl BoundedHttpIo {
    fn new(
        inner: TcpStream,
        permit: OwnedSemaphorePermit,
        io_idle_timeout: Duration,
        connection_lifetime: Duration,
        cancellation: CancellationToken,
    ) -> Self {
        let now = Instant::now();
        Self {
            inner,
            _permit: permit,
            io_idle_timeout,
            read_idle: Box::pin(tokio::time::sleep_until(now + io_idle_timeout)),
            write_idle: Box::pin(tokio::time::sleep_until(now + io_idle_timeout)),
            read_lifetime: Box::pin(tokio::time::sleep_until(now + connection_lifetime)),
            write_lifetime: Box::pin(tokio::time::sleep_until(now + connection_lifetime)),
            read_cancellation: Box::pin(cancellation.clone().cancelled_owned()),
            write_cancellation: Box::pin(cancellation.cancelled_owned()),
        }
    }

    fn poll_read_boundary(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.read_cancellation.as_mut().poll(context).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "MCP HTTP server is shutting down",
            ));
        }
        if self.read_lifetime.as_mut().poll(context).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP HTTP connection lifetime elapsed",
            ));
        }
        if self.read_idle.as_mut().poll(context).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP HTTP read idle timeout elapsed",
            ));
        }
        Ok(())
    }

    fn poll_write_boundary(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        if self.write_cancellation.as_mut().poll(context).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "MCP HTTP server is shutting down",
            ));
        }
        if self.write_lifetime.as_mut().poll(context).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP HTTP connection lifetime elapsed",
            ));
        }
        if self.write_idle.as_mut().poll(context).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP HTTP write idle timeout elapsed",
            ));
        }
        Ok(())
    }

    fn reset_read_idle(&mut self) {
        self.read_idle
            .as_mut()
            .reset(Instant::now() + self.io_idle_timeout);
    }

    fn reset_write_idle(&mut self) {
        self.write_idle
            .as_mut()
            .reset(Instant::now() + self.io_idle_timeout);
    }
}

impl AsyncRead for BoundedHttpIo {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_read_boundary(context) {
            return Poll::Ready(Err(error));
        }
        let filled = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(context, buffer);
        if matches!(result, Poll::Ready(Ok(()))) && buffer.filled().len() > filled {
            this.reset_read_idle();
        }
        result
    }
}

impl AsyncWrite for BoundedHttpIo {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_write_boundary(context) {
            return Poll::Ready(Err(error));
        }
        let result = Pin::new(&mut this.inner).poll_write(context, buffer);
        if matches!(result, Poll::Ready(Ok(written)) if written > 0) {
            this.reset_write_idle();
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_write_boundary(context) {
            return Poll::Ready(Err(error));
        }
        Pin::new(&mut this.inner).poll_flush(context)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        if let Err(error) = this.poll_write_boundary(context) {
            return Poll::Ready(Err(error));
        }
        let result = Pin::new(&mut this.inner).poll_write_vectored(context, buffers);
        if matches!(result, Poll::Ready(Ok(written)) if written > 0) {
            this.reset_write_idle();
        }
        result
    }
}

/// Serves one host-authorized MCP process over standard input/output.
///
/// # Errors
///
/// Returns bounded configuration, authority, platform, runtime, bootstrap, or
/// MCP transport failures without including sensitive policy contents.
pub async fn serve_stdio(config: McpServeConfig) -> Result<(), McpServeError> {
    let (runtime, server) = bootstrap_adapter(config).await?;
    let result = async {
        let service = server
            .clone()
            .serve(stdio())
            .await
            .map_err(|error| McpServeError::Transport(error.to_string()))?;
        service
            .waiting()
            .await
            .map_err(|error| McpServeError::Transport(error.to_string()))?;
        Ok(())
    }
    .await;
    server.close().await;
    runtime.shutdown().await;
    result
}

/// Serves current stateless Streamable HTTP on one loopback endpoint.
///
/// The listener authenticates before MCP body dispatch, rejects every browser
/// `Origin` by default, disables legacy MCP sessions, and exposes no old
/// HTTP+SSE endpoint/message pair. SSE remains only a response representation
/// inside this Streamable HTTP transport.
///
/// # Errors
///
/// Returns configuration, bootstrap, bind, listener, or MCP service failures.
pub async fn serve_http(config: McpServeConfig, http: McpHttpConfig) -> Result<(), McpServeError> {
    serve_http_until(
        config,
        http,
        |ready| {
            eprintln!(
                "{}",
                serde_json::json!({
                    "transport": "http",
                    "endpoint": ready.endpoint,
                })
            );
            Ok(())
        },
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await
}

/// Serves Streamable HTTP until a caller-owned shutdown future resolves.
///
/// The ready callback runs after bind and before request acceptance. A callback
/// failure closes the runtime without publishing a partially initialized
/// service.
///
/// # Errors
///
/// Returns configuration, bootstrap, bind, ready-publication, listener, or MCP
/// service failures.
pub async fn serve_http_until<F, R>(
    config: McpServeConfig,
    http: McpHttpConfig,
    on_ready: R,
    shutdown: F,
) -> Result<(), McpServeError>
where
    F: Future<Output = ()> + Send + 'static,
    R: FnOnce(McpHttpReady) -> Result<(), McpServeError>,
{
    http.validate()?;
    let (runtime, server) = bootstrap_adapter(config).await?;
    let server = server.require_current_http_protocol();
    let listener = match tokio::net::TcpListener::bind(SocketAddr::new(http.bind, http.port)).await
    {
        Ok(listener) => listener,
        Err(error) => {
            server.close().await;
            runtime.shutdown().await;
            return Err(McpServeError::Http(error));
        }
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            server.close().await;
            runtime.shutdown().await;
            return Err(McpServeError::Http(error));
        }
    };
    let cancellation = CancellationToken::new();
    let endpoint = http.endpoint.clone();
    let listener = BoundedTcpListener::new(listener, &http, cancellation.clone());
    let router = http_router(&server, http, address, &cancellation);
    if let Err(error) = on_ready(McpHttpReady {
        address,
        endpoint: format!("http://{address}{endpoint}"),
    }) {
        cancellation.cancel();
        server.close().await;
        runtime.shutdown().await;
        return Err(error);
    }
    let serve_result = axum::serve(listener, router)
        .with_graceful_shutdown({
            let cancellation = cancellation.clone();
            async move {
                shutdown.await;
                cancellation.cancel();
            }
        })
        .await
        .map_err(McpServeError::Http);
    cancellation.cancel();
    server.close().await;
    runtime.shutdown().await;
    serve_result
}

fn http_router(
    server: &NexusCuaMcp,
    http: McpHttpConfig,
    address: SocketAddr,
    cancellation: &CancellationToken,
) -> Router {
    let transport_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(http.response_mode == McpHttpResponseMode::Json)
        .with_stateless_protocol_metadata_required(true)
        .with_max_request_body_bytes(http.max_request_body_bytes)
        .with_allowed_hosts(allowed_loopback_hosts(address))
        .with_cancellation_token(cancellation.clone());
    let shared_server = server.clone();
    let service: StreamableHttpService<NexusCuaMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(shared_server.clone()),
            Arc::new(LocalSessionManager::default()),
            transport_config,
        );
    let guard = HttpRequestGuard {
        authorization: Arc::new(http.authorization),
        inflight: Arc::new(Semaphore::new(http.max_inflight_requests)),
        request_timeout: http.request_timeout,
    };
    Router::new()
        .nest_service(&http.endpoint, service)
        .layer(middleware::from_fn_with_state(guard, authorize_http))
}

async fn bootstrap_adapter(
    config: McpServeConfig,
) -> Result<(Arc<Runtime>, NexusCuaMcp), McpServeError> {
    config.validate()?;
    let driver = nexus_cua_platform::system_driver()?;
    let mut runtime_config = RuntimeConfig::new(config.artifact_root);
    runtime_config.max_observation_age = Duration::from_secs(config.max_observation_age_seconds);
    runtime_config.policy = RuntimePolicyStack::from_files(
        config.managed_policy_file.as_deref(),
        config.policy_file.as_deref(),
    )?;
    let runtime = Arc::new(Runtime::new(driver, runtime_config)?);
    let authority = match resolve_authority(&config.authority, runtime.epoch()).await {
        Ok(authority) => authority,
        Err(error) => {
            runtime.shutdown().await;
            return Err(error.into());
        }
    };
    let server = match NexusCuaMcp::bootstrap_with_authority_context(
        Arc::clone(&runtime),
        authority,
        config.max_image_bytes,
        config.tool_profile,
    )
    .await
    {
        Ok(server) => server,
        Err(error) => {
            runtime.shutdown().await;
            return Err(error.into());
        }
    };
    Ok((runtime, server))
}

async fn resolve_authority(
    source: &AuthoritySource,
    runtime_epoch: &str,
) -> Result<AuthorityContext, AuthorityError> {
    match source {
        AuthoritySource::UnsignedDevelopmentFile(path) => Ok(
            AuthorityContext::unsigned_development(AuthorityPolicy::load_development_file(path)?),
        ),
        AuthoritySource::Provider {
            registration_id,
            provider,
        } => {
            let challenge = AuthorityChallenge::new(registration_id.clone(), runtime_epoch, None)?;
            let grant = provider.acquire(challenge.clone()).await?;
            grant
                .resolve(&challenge, current_unix_ms()?)
                .map(|authority| authority.with_provider(Arc::clone(provider)))
        }
    }
}

async fn authorize_http(
    State(guard): State<HttpRequestGuard>,
    request: Request,
    next: Next,
) -> Response {
    if request.headers().contains_key(header::ORIGIN) {
        return (StatusCode::FORBIDDEN, "origin is not allowed").into_response();
    }
    if !bearer_matches(request.headers(), guard.authorization.as_ref()) {
        let mut response = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    let Ok(_permit) = Arc::clone(&guard.inflight).try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "request capacity is busy").into_response();
    };
    match timeout(guard.request_timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::REQUEST_TIMEOUT, "request deadline elapsed").into_response(),
    }
}

fn bearer_matches(headers: &axum::http::HeaderMap, expected: &AuthorizationToken) -> bool {
    let Some(candidate) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    let candidate = candidate.as_bytes();
    let expected = expected.expose().as_bytes();
    candidate.len() == expected.len() && bool::from(candidate.ct_eq(expected))
}

fn valid_endpoint(endpoint: &str) -> bool {
    !endpoint.is_empty()
        && endpoint.len() <= MAX_HTTP_ENDPOINT_BYTES
        && endpoint.starts_with('/')
        && endpoint != "/"
        && !endpoint.ends_with('/')
        && !endpoint.contains("//")
        && !endpoint.contains("..")
        && !endpoint.contains(['?', '#'])
        && endpoint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
}

fn allowed_loopback_hosts(address: SocketAddr) -> Vec<String> {
    let mut hosts = vec![address.ip().to_string(), address.to_string()];
    if address.is_ipv4() {
        hosts.push("localhost".to_owned());
        hosts.push(format!("localhost:{}", address.port()));
    } else {
        hosts.push("[::1]".to_owned());
        hosts.push(format!("[::1]:{}", address.port()));
    }
    hosts
}

/// Public MCP adapter startup failure.
#[derive(Debug, Error)]
pub enum McpServeError {
    /// A configured bound is outside the closed supported range.
    #[error("invalid MCP configuration: {0}")]
    InvalidConfiguration(String),
    /// Host authority could not be loaded or validated.
    #[error(transparent)]
    Authority(#[from] AuthorityError),
    /// Native platform driver setup failed.
    #[error("native platform driver setup failed: {0}")]
    Driver(#[from] DriverError),
    /// Runtime construction failed.
    #[error("runtime setup failed: {0:?}")]
    Runtime(nexus_cua_protocol::CuaError),
    /// Immutable runtime policy setup failed.
    #[error(transparent)]
    Policy(#[from] nexus_cua_runtime::PolicyLoadError),
    /// Authority could not be bound to a fresh runtime session.
    #[error(transparent)]
    Bootstrap(#[from] BootstrapError),
    /// MCP framing or process transport failed.
    #[error("MCP transport failed: {0}")]
    Transport(String),
    /// Streamable HTTP listener failed without exposing request contents.
    #[error("MCP HTTP listener failed: {0}")]
    Http(std::io::Error),
}

impl From<nexus_cua_protocol::CuaError> for McpServeError {
    fn from(error: nexus_cua_protocol::CuaError) -> Self {
        Self::Runtime(error)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::routing::post;
    use nexus_cua_protocol::{
        AccessibilityMode, ActionKind, ApplicationProvenance, CaptureMode, ClipboardOperation,
        DeliveryMode, DriverCapabilities, InputRoute, PermissionMode, PermissionState,
        PermissionStatus, Platform, ScreenRect,
    };
    use nexus_cua_runtime::{
        DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverErrorKind,
        DriverObservation, DriverVerification, DriverWindow,
    };
    use rmcp::model::{CallToolRequestParams, ClientRequest, Request};
    use rmcp::service::PeerRequestOptions;
    use serde_json::Value;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::sync::Notify;
    use tower::ServiceExt as _;

    use crate::{AuthorityGrant, AuthorityProviderError, MCP_SERVER_NAME};

    struct TestProvider;

    struct TransportDriver;

    #[derive(Default)]
    struct MutationGate {
        entered: Notify,
        release: Notify,
        dispatches: AtomicUsize,
        effects: AtomicUsize,
    }

    struct MutationDriver {
        gate: Arc<MutationGate>,
    }

    impl MutationDriver {
        fn application() -> DriverApplication {
            DriverApplication {
                key: "transport-app-key".to_owned(),
                process_generation: "transport-app-generation".to_owned(),
                identity: "bundle:dev.nexus.transport-fixture".to_owned(),
                name: "Transport Fixture".to_owned(),
                application_id: "dev.nexus.transport-fixture".to_owned(),
                foreground: true,
                provenance: ApplicationProvenance::Macos {
                    bundle_id: Some("dev.nexus.transport-fixture".to_owned()),
                    executable_path: None,
                    signing_team_id: None,
                    designated_requirement: None,
                },
            }
        }

        fn window() -> DriverWindow {
            DriverWindow {
                key: "transport-window-key".to_owned(),
                application: Self::application(),
                title: "Transport Fixture".to_owned(),
                screen_bounds: ScreenRect {
                    x: 20.0,
                    y: 30.0,
                    width: 640.0,
                    height: 480.0,
                },
                minimized: false,
                visible: true,
                foreground: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl DesktopDriver for TransportDriver {
        async fn capabilities(&self) -> Result<DriverCapabilities, DriverError> {
            Ok(DriverCapabilities {
                protocol_version: "nexus.cua.v1".to_owned(),
                runtime_version: "transport-test".to_owned(),
                platform: Platform::Macos,
                capture_modes: vec![CaptureMode::Display],
                accessibility_tree: false,
                input_routes: Vec::<InputRoute>::new(),
                application_lifecycle: Vec::new(),
                clipboard: Vec::<ClipboardOperation>::new(),
                actions: Vec::new(),
            })
        }

        async fn permission_status(&self) -> Result<PermissionStatus, DriverError> {
            Ok(PermissionStatus {
                screen_capture: PermissionState::Granted,
                accessibility: PermissionState::NotApplicable,
                input_control: PermissionState::NotApplicable,
            })
        }

        async fn list_applications(
            &self,
        ) -> Result<Vec<nexus_cua_runtime::DriverApplication>, DriverError> {
            Ok(Vec::new())
        }

        async fn list_windows(
            &self,
            _application_key: Option<&str>,
        ) -> Result<Vec<DriverWindow>, DriverError> {
            Ok(Vec::new())
        }

        async fn observe_window(
            &self,
            _window: &DriverWindow,
            _include_screenshot: bool,
            _accessibility: AccessibilityMode,
        ) -> Result<DriverObservation, DriverError> {
            Err(transport_driver_unused())
        }

        async fn observation_is_current(
            &self,
            _window: &DriverWindow,
            _fingerprint: &str,
        ) -> Result<bool, DriverError> {
            Err(transport_driver_unused())
        }

        async fn perform_action(
            &self,
            _window: &DriverWindow,
            _action: DriverAction,
            _allow_foreground: bool,
            _restore_foreground: bool,
        ) -> Result<DriverActionOutput, DriverError> {
            Err(transport_driver_unused().mutation_not_dispatched())
        }

        async fn verify_state(
            &self,
            _window: &DriverWindow,
            _predicate: &nexus_cua_protocol::StatePredicate,
        ) -> Result<DriverVerification, DriverError> {
            Err(transport_driver_unused())
        }
    }

    #[async_trait::async_trait]
    impl DesktopDriver for MutationDriver {
        async fn capabilities(&self) -> Result<DriverCapabilities, DriverError> {
            Ok(DriverCapabilities {
                protocol_version: "nexus.cua.v1".to_owned(),
                runtime_version: "transport-mutation-test".to_owned(),
                platform: Platform::Macos,
                capture_modes: vec![CaptureMode::Window],
                accessibility_tree: false,
                input_routes: vec![InputRoute::Foreground],
                application_lifecycle: Vec::new(),
                clipboard: Vec::new(),
                actions: vec![ActionKind::PressKeys],
            })
        }

        async fn permission_status(&self) -> Result<PermissionStatus, DriverError> {
            Ok(PermissionStatus {
                screen_capture: PermissionState::Granted,
                accessibility: PermissionState::NotApplicable,
                input_control: PermissionState::Granted,
            })
        }

        async fn list_applications(&self) -> Result<Vec<DriverApplication>, DriverError> {
            Ok(vec![Self::application()])
        }

        async fn list_windows(
            &self,
            _application_key: Option<&str>,
        ) -> Result<Vec<DriverWindow>, DriverError> {
            Ok(vec![Self::window()])
        }

        async fn observe_window(
            &self,
            window: &DriverWindow,
            _include_screenshot: bool,
            _accessibility: AccessibilityMode,
        ) -> Result<DriverObservation, DriverError> {
            Ok(DriverObservation {
                window_key: window.key.clone(),
                window_screen_bounds: window.screen_bounds,
                screenshot: None,
                screenshot_screen_bounds: None,
                elements: Vec::new(),
                elements_complete: true,
                elements_truncation: None,
                fingerprint: "transport-fixture-v1".to_owned(),
            })
        }

        async fn observation_is_current(
            &self,
            _window: &DriverWindow,
            fingerprint: &str,
        ) -> Result<bool, DriverError> {
            Ok(fingerprint == "transport-fixture-v1")
        }

        async fn perform_action(
            &self,
            _window: &DriverWindow,
            action: DriverAction,
            allow_foreground: bool,
            _restore_foreground: bool,
        ) -> Result<DriverActionOutput, DriverError> {
            assert!(matches!(action, DriverAction::PressKeys { .. }));
            assert!(allow_foreground);
            self.gate.dispatches.fetch_add(1, Ordering::SeqCst);
            self.gate.entered.notify_one();
            self.gate.release.notified().await;
            self.gate.effects.fetch_add(1, Ordering::SeqCst);
            Ok(DriverActionOutput::unverified(DeliveryMode::Foreground))
        }

        async fn verify_state(
            &self,
            _window: &DriverWindow,
            _predicate: &nexus_cua_protocol::StatePredicate,
        ) -> Result<DriverVerification, DriverError> {
            Ok(DriverVerification {
                matched: true,
                evidence: "transport fixture state".to_owned(),
            })
        }
    }

    fn transport_driver_unused() -> DriverError {
        DriverError::new(
            DriverErrorKind::Unsupported,
            "transport conformance does not invoke a desktop operation",
        )
    }

    #[async_trait::async_trait]
    impl AuthorityProvider for TestProvider {
        async fn acquire(
            &self,
            challenge: AuthorityChallenge,
        ) -> Result<AuthorityGrant, AuthorityProviderError> {
            let now = current_unix_ms().map_err(|_| AuthorityProviderError::InvalidResponse)?;
            Ok(AuthorityGrant {
                schema_version: 1,
                provider_id: "test-provider".to_owned(),
                registration_id: challenge.registration_id,
                adapter_nonce: challenge.adapter_nonce,
                runtime_epoch: challenge.runtime_epoch,
                worker_epoch: challenge.worker_epoch,
                principal: "test-principal".to_owned(),
                work_scope: "test-scope".to_owned(),
                request_namespace: "provider_namespace_012345".to_owned(),
                issued_at_unix_ms: now,
                expires_at_unix_ms: now + 60_000,
                authority: AuthorityPolicy {
                    schema_version: 1,
                    mode: PermissionMode::ReadOnly,
                    application_ids: Vec::new(),
                    allowed_actions: Vec::<ActionKind>::new(),
                    allow_foreground_input: false,
                    restore_foreground_after_input: false,
                    allow_display_capture: true,
                    allow_clipboard_read: false,
                    allow_clipboard_write: false,
                    allow_application_launch: false,
                    allow_application_launch_activation: false,
                    ttl_seconds: 60,
                },
            })
        }
    }

    async fn transport_server() -> (Arc<Runtime>, NexusCuaMcp, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nexus-cua-mcp-transport-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let runtime = Arc::new(
            Runtime::new(
                Arc::new(TransportDriver),
                RuntimeConfig::new(root.join("artifacts")),
            )
            .expect("transport test runtime"),
        );
        let authority = AuthorityContext::unsigned_development(AuthorityPolicy {
            schema_version: 1,
            mode: PermissionMode::ReadOnly,
            application_ids: Vec::new(),
            allowed_actions: Vec::new(),
            allow_foreground_input: false,
            restore_foreground_after_input: false,
            allow_display_capture: true,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            allow_application_launch: false,
            allow_application_launch_activation: false,
            ttl_seconds: 60,
        });
        let server = NexusCuaMcp::bootstrap_with_authority_context(
            Arc::clone(&runtime),
            authority,
            DEFAULT_MAX_IMAGE_BYTES,
            ToolProfile::Full,
        )
        .await
        .expect("transport test MCP server");
        (runtime, server, root)
    }

    async fn mutation_server() -> (Arc<Runtime>, NexusCuaMcp, Arc<MutationGate>, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nexus-cua-mcp-mutation-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let gate = Arc::new(MutationGate::default());
        let runtime = Arc::new(
            Runtime::new(
                Arc::new(MutationDriver {
                    gate: Arc::clone(&gate),
                }),
                RuntimeConfig::new(root.join("artifacts")),
            )
            .expect("mutation test runtime"),
        );
        let authority = AuthorityContext::unsigned_development(AuthorityPolicy {
            schema_version: 1,
            mode: PermissionMode::Bounded,
            application_ids: vec!["dev.nexus.transport-fixture".to_owned()],
            allowed_actions: vec![ActionKind::PressKeys],
            allow_foreground_input: true,
            restore_foreground_after_input: false,
            allow_display_capture: false,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            allow_application_launch: false,
            allow_application_launch_activation: false,
            ttl_seconds: 60,
        });
        let server = NexusCuaMcp::bootstrap_with_authority_context(
            Arc::clone(&runtime),
            authority,
            DEFAULT_MAX_IMAGE_BYTES,
            ToolProfile::Full,
        )
        .await
        .expect("mutation test MCP server");
        (runtime, server, gate, root)
    }

    fn json_object(value: Value) -> serde_json::Map<String, Value> {
        match value {
            Value::Object(object) => object,
            _ => panic!("JSON object"),
        }
    }

    fn tool_call_body(id: u64, name: &str, arguments: &Value) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "nexus-cua-conformance",
                        "version": "1"
                    },
                    "io.modelcontextprotocol/clientCapabilities": {}
                },
                "name": name,
                "arguments": arguments
            }
        })
    }

    fn press_keys_arguments(request_id: &str, window_ref: &str, observation_id: &str) -> Value {
        serde_json::json!({
            "request_id": request_id,
            "window_ref": window_ref,
            "observation_id": observation_id,
            "keys": ["enter"]
        })
    }

    fn client_tool_request(name: &str, arguments: Value) -> ClientRequest {
        ClientRequest::CallToolRequest(Request::new(
            CallToolRequestParams::new(name.to_owned()).with_arguments(json_object(arguments)),
        ))
    }

    fn assert_reconciliation_succeeded(value: &Value, request_id: &str) {
        assert_eq!(value["request_id"], request_id);
        assert_eq!(value["admission"], "admitted");
        assert_eq!(value["disposition"], "succeeded");
        assert_eq!(value["dispatched"], true);
        assert_eq!(value["fresh_observation_required"], true);
    }

    async fn stdio_observation(
        client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    ) -> (String, String) {
        let windows = client
            .call_tool(
                CallToolRequestParams::new("list_windows")
                    .with_arguments(json_object(serde_json::json!({}))),
            )
            .await
            .expect("stdio list_windows");
        let window_ref = windows
            .structured_content
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|windows| windows.first())
            .and_then(|window| window["window_ref"].as_str())
            .expect("stdio window_ref")
            .to_owned();
        let observation =
            client
                .call_tool(CallToolRequestParams::new("observe_window").with_arguments(
                    json_object(serde_json::json!({
                        "window_ref": window_ref,
                        "include_screenshot": false,
                        "accessibility": "disabled"
                    })),
                ))
                .await
                .expect("stdio observe_window");
        let observation_id = observation
            .structured_content
            .as_ref()
            .and_then(|value| value["observation_id"].as_str())
            .expect("stdio observation_id")
            .to_owned();
        (window_ref, observation_id)
    }

    async fn stdio_reconcile(
        client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
        request_id: &str,
    ) {
        let result = client
            .call_tool(
                CallToolRequestParams::new("reconcile_mutation").with_arguments(json_object(
                    serde_json::json!({"request_id": request_id, "wait_ms": 1000}),
                )),
            )
            .await
            .expect("stdio reconcile_mutation");
        assert_eq!(result.is_error, Some(false));
        assert_reconciliation_succeeded(
            result
                .structured_content
                .as_ref()
                .expect("stdio reconciliation body"),
            request_id,
        );
    }

    async fn decode_http_response(response: Response, mode: McpHttpResponseMode) -> Value {
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("bounded response body");
        assert_eq!(
            status,
            StatusCode::OK,
            "unexpected HTTP MCP response: {}",
            String::from_utf8_lossy(&bytes)
        );
        match mode {
            McpHttpResponseMode::Json => {
                assert!(content_type.starts_with("application/json"));
                serde_json::from_slice(&bytes).expect("JSON response")
            }
            McpHttpResponseMode::RequestScopedSse => {
                assert!(content_type.starts_with("text/event-stream"));
                let text = std::str::from_utf8(&bytes).expect("UTF-8 SSE response");
                let data = text
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .expect("request-scoped SSE data event");
                serde_json::from_str(data).expect("SSE JSON-RPC response")
            }
        }
    }

    fn current_http_request(method: &str, body: &Value) -> axum::http::Request<Body> {
        let mut builder = axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1:48123")
            .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(64)))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", method);
        if let Some(name) = body["params"]["name"].as_str() {
            builder = builder.header("Mcp-Name", name);
        }
        builder
            .body(Body::from(serde_json::to_vec(body).expect("request JSON")))
            .expect("HTTP request")
    }

    async fn http_tool(
        router: &Router,
        mode: McpHttpResponseMode,
        id: u64,
        name: &str,
        arguments: Value,
    ) -> Value {
        decode_http_response(
            router
                .clone()
                .oneshot(current_http_request(
                    "tools/call",
                    &tool_call_body(id, name, &arguments),
                ))
                .await
                .expect("HTTP tool response"),
            mode,
        )
        .await
    }

    async fn http_observation(
        router: &Router,
        mode: McpHttpResponseMode,
        id: u64,
    ) -> (String, String) {
        let windows = http_tool(router, mode, id, "list_windows", serde_json::json!({})).await;
        let window_ref = windows["result"]["structuredContent"]
            .as_array()
            .and_then(|windows| windows.first())
            .and_then(|window| window["window_ref"].as_str())
            .expect("HTTP window_ref")
            .to_owned();
        let observation = http_tool(
            router,
            mode,
            id + 1,
            "observe_window",
            serde_json::json!({
                "window_ref": window_ref,
                "include_screenshot": false,
                "accessibility": "disabled"
            }),
        )
        .await;
        let observation_id = observation["result"]["structuredContent"]["observation_id"]
            .as_str()
            .expect("HTTP observation_id")
            .to_owned();
        (window_ref, observation_id)
    }

    async fn http_reconcile(router: &Router, mode: McpHttpResponseMode, id: u64, request_id: &str) {
        let result = http_tool(
            router,
            mode,
            id,
            "reconcile_mutation",
            serde_json::json!({"request_id": request_id, "wait_ms": 1000}),
        )
        .await;
        assert_ne!(result["result"]["isError"], true);
        assert_reconciliation_succeeded(&result["result"]["structuredContent"], request_id);
    }

    async fn http_catalogue(mode: McpHttpResponseMode) -> Vec<Value> {
        let (runtime, server, root) = transport_server().await;
        let address: SocketAddr = "127.0.0.1:48123".parse().expect("test address");
        let cancellation = CancellationToken::new();
        let mut config = McpHttpConfig::new(AuthorizationToken::new("a".repeat(64)));
        config.response_mode = mode;
        let server = server.require_current_http_protocol();
        let router = http_router(&server, config, address, &cancellation);

        let initialize = current_http_request(
            "initialize",
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2026-07-28",
                    "capabilities": {},
                    "clientInfo": {"name": "nexus-cua-conformance", "version": "1"}
                }
            }),
        );
        let initialized = decode_http_response(
            router
                .clone()
                .oneshot(initialize)
                .await
                .expect("initialize response"),
            mode,
        )
        .await;
        assert_eq!(initialized["result"]["serverInfo"]["name"], MCP_SERVER_NAME);

        let list = current_http_request(
            "tools/list",
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientInfo": {
                            "name": "nexus-cua-conformance",
                            "version": "1"
                        },
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                }
            }),
        );
        let listed = decode_http_response(
            router
                .clone()
                .oneshot(list)
                .await
                .expect("tools/list response"),
            mode,
        )
        .await;
        let mut tools = listed["result"]["tools"]
            .as_array()
            .expect("tool list")
            .clone();
        tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));

        let legacy_get = router
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/mcp")
                    .header(header::HOST, "127.0.0.1:48123")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .expect("GET request"),
            )
            .await
            .expect("GET response");
        assert!(
            matches!(
                legacy_get.status(),
                StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_FOUND
            ),
            "legacy GET stream must not be available"
        );

        cancellation.cancel();
        server.close().await;
        runtime.shutdown().await;
        let _ = std::fs::remove_dir_all(root);
        tools
    }

    async fn stdio_catalogue() -> Vec<Value> {
        let (runtime, server, root) = transport_server().await;
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let serving = tokio::spawn({
            let server = server.clone();
            async move {
                let service = server
                    .serve(server_transport)
                    .await
                    .map_err(|error| error.to_string())?;
                service.waiting().await.map_err(|error| error.to_string())
            }
        });
        let client = ().serve(client_transport).await.expect("stdio MCP client");
        let mut tools = client
            .list_tools(None)
            .await
            .expect("stdio tools/list")
            .tools
            .into_iter()
            .map(|tool| serde_json::to_value(tool).expect("serialize stdio tool"))
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        client.cancel().await.expect("close stdio client");
        serving.await.expect("stdio task").expect("stdio server");
        server.close().await;
        runtime.shutdown().await;
        let _ = std::fs::remove_dir_all(root);
        tools
    }

    #[test]
    fn shared_transport_configuration_rejects_unbounded_images_and_guards() {
        let mut config =
            McpServeConfig::unsigned_development("authority.json".into(), "artifacts".into());
        config.max_image_bytes = HARD_MAX_IMAGE_BYTES + 1;
        assert!(config.validate().is_err());

        config.max_image_bytes = DEFAULT_MAX_IMAGE_BYTES;
        config.max_observation_age_seconds = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn http_configuration_is_loopback_bounded_and_has_one_endpoint() {
        let token = AuthorizationToken::new("t".repeat(64));
        let mut config = McpHttpConfig::new(token);
        assert!(config.validate().is_ok());
        config.bind = "0.0.0.0".parse().unwrap();
        assert!(config.validate().is_err());
        config.bind = IpAddr::V4(Ipv4Addr::LOCALHOST);
        config.endpoint = "/old/sse/".to_owned();
        assert!(config.validate().is_err());

        config.endpoint = "/mcp".to_owned();
        config.max_connections = config.max_inflight_requests - 1;
        assert!(config.validate().is_err());
        config.max_connections = DEFAULT_MAX_HTTP_CONNECTIONS;
        config.io_idle_timeout = Duration::from_millis(500);
        assert!(config.validate().is_err());
        config.io_idle_timeout = Duration::from_secs(DEFAULT_HTTP_IO_IDLE_TIMEOUT_SECONDS);
        config.request_timeout = Duration::from_secs(HARD_HTTP_REQUEST_TIMEOUT_SECONDS + 1);
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn stdio_and_both_http_representations_publish_the_same_catalogue() {
        let stdio = stdio_catalogue().await;
        let json = http_catalogue(McpHttpResponseMode::Json).await;
        let sse = http_catalogue(McpHttpResponseMode::RequestScopedSse).await;

        assert!(!stdio.is_empty());
        assert_eq!(stdio, json);
        assert_eq!(stdio, sse);
    }

    #[tokio::test]
    async fn stdio_timeout_and_cancellation_preserve_admitted_mutations_for_reconciliation() {
        let (runtime, server, gate, root) = mutation_server().await;
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let serving = tokio::spawn({
            let server = server.clone();
            async move {
                let service = server
                    .serve(server_transport)
                    .await
                    .map_err(|error| error.to_string())?;
                service.waiting().await.map_err(|error| error.to_string())
            }
        });
        let client = ().serve(client_transport).await.expect("stdio MCP client");

        let (window_ref, observation_id) = stdio_observation(&client).await;
        let timeout_request_id = "development_fixture:stdio_timeout";
        let timed = client
            .send_cancellable_request(
                client_tool_request(
                    "press_keys",
                    press_keys_arguments(timeout_request_id, &window_ref, &observation_id),
                ),
                PeerRequestOptions::with_timeout(Duration::from_millis(250)),
            )
            .await
            .expect("send timed stdio mutation");
        let timed = tokio::spawn(async move { timed.await_response().await });
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .expect("timed mutation admitted");
        let error = timed
            .await
            .expect("timed request task")
            .expect_err("client deadline must expire while native action is admitted");
        assert!(matches!(error, rmcp::service::ServiceError::Timeout { .. }));
        gate.release.notify_one();
        stdio_reconcile(&client, timeout_request_id).await;
        assert_eq!(gate.dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(gate.effects.load(Ordering::SeqCst), 1);

        let (window_ref, observation_id) =
            tokio::time::timeout(Duration::from_secs(5), stdio_observation(&client))
                .await
                .expect("stdio remains usable after request timeout");
        let cancelled_request_id = "development_fixture:stdio_cancelled";
        let cancelled = client
            .send_cancellable_request(
                client_tool_request(
                    "press_keys",
                    press_keys_arguments(cancelled_request_id, &window_ref, &observation_id),
                ),
                PeerRequestOptions::no_options(),
            )
            .await
            .expect("send cancellable stdio mutation");
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .expect("cancelled mutation admitted");
        cancelled
            .cancel(Some("transport conformance disconnect".to_owned()))
            .await
            .expect("cancel stdio wait");
        gate.release.notify_one();
        stdio_reconcile(&client, cancelled_request_id).await;

        let replay = client
            .call_tool(
                CallToolRequestParams::new("press_keys").with_arguments(json_object(
                    press_keys_arguments(cancelled_request_id, &window_ref, &observation_id),
                )),
            )
            .await
            .expect("replay completed stdio mutation");
        assert_ne!(replay.is_error, Some(true));
        assert_eq!(gate.dispatches.load(Ordering::SeqCst), 2);
        assert_eq!(gate.effects.load(Ordering::SeqCst), 2);

        client.cancel().await.expect("close stdio client");
        serving.await.expect("stdio task").expect("stdio server");
        server.close().await;
        runtime.shutdown().await;
        let _ = std::fs::remove_dir_all(root);
    }

    async fn exercise_http_mutation_interruptions(mode: McpHttpResponseMode) {
        let (runtime, server, gate, root) = mutation_server().await;
        let address: SocketAddr = "127.0.0.1:48123".parse().expect("test address");
        let cancellation = CancellationToken::new();
        let mut config = McpHttpConfig::new(AuthorizationToken::new("a".repeat(64)));
        config.response_mode = mode;
        let server = server.require_current_http_protocol();
        let router = http_router(&server, config, address, &cancellation);

        let (window_ref, observation_id) = http_observation(&router, mode, 10).await;
        let timeout_request_id = match mode {
            McpHttpResponseMode::Json => "development_fixture:http_json_timeout",
            McpHttpResponseMode::RequestScopedSse => "development_fixture:http_sse_timeout",
        };
        let timed_router = router.clone();
        let timed_request = current_http_request(
            "tools/call",
            &tool_call_body(
                12,
                "press_keys",
                &press_keys_arguments(timeout_request_id, &window_ref, &observation_id),
            ),
        );
        let timed = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_millis(250), async move {
                let response = timed_router
                    .oneshot(timed_request)
                    .await
                    .expect("timed HTTP response");
                to_bytes(response.into_body(), 1024 * 1024)
                    .await
                    .expect("timed HTTP body")
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .expect("timed HTTP mutation admitted");
        assert!(
            timed.await.expect("timed HTTP client task").is_err(),
            "HTTP client deadline must drop the admitted request wait"
        );
        gate.release.notify_one();
        http_reconcile(&router, mode, 13, timeout_request_id).await;
        assert_eq!(gate.dispatches.load(Ordering::SeqCst), 1);
        assert_eq!(gate.effects.load(Ordering::SeqCst), 1);

        let (window_ref, observation_id) = http_observation(&router, mode, 20).await;
        let disconnected_request_id = match mode {
            McpHttpResponseMode::Json => "development_fixture:http_json_disconnect",
            McpHttpResponseMode::RequestScopedSse => "development_fixture:http_sse_disconnect",
        };
        let disconnected_router = router.clone();
        let disconnected_request = current_http_request(
            "tools/call",
            &tool_call_body(
                22,
                "press_keys",
                &press_keys_arguments(disconnected_request_id, &window_ref, &observation_id),
            ),
        );
        let disconnected = tokio::spawn(async move {
            let response = disconnected_router
                .oneshot(disconnected_request)
                .await
                .expect("disconnected HTTP response");
            to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("disconnected HTTP body")
        });
        tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
            .await
            .expect("disconnected HTTP mutation admitted");
        disconnected.abort();
        assert!(
            disconnected
                .await
                .expect_err("disconnected HTTP wait must be aborted")
                .is_cancelled()
        );
        gate.release.notify_one();
        http_reconcile(&router, mode, 23, disconnected_request_id).await;

        let replay = http_tool(
            &router,
            mode,
            24,
            "press_keys",
            press_keys_arguments(disconnected_request_id, &window_ref, &observation_id),
        )
        .await;
        assert_ne!(replay["result"]["isError"], true);
        assert_eq!(gate.dispatches.load(Ordering::SeqCst), 2);
        assert_eq!(gate.effects.load(Ordering::SeqCst), 2);

        cancellation.cancel();
        server.close().await;
        runtime.shutdown().await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn both_http_representations_reconcile_after_timeout_and_disconnect() {
        exercise_http_mutation_interruptions(McpHttpResponseMode::Json).await;
        exercise_http_mutation_interruptions(McpHttpResponseMode::RequestScopedSse).await;
    }

    #[test]
    fn bearer_authentication_is_exact_and_constant_length() {
        let expected = AuthorizationToken::new("a".repeat(64));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", "a".repeat(64))).unwrap(),
        );
        assert!(bearer_matches(&headers, &expected));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", "a".repeat(63))).unwrap(),
        );
        assert!(!bearer_matches(&headers, &expected));
    }

    #[tokio::test]
    async fn provider_source_binds_a_fresh_runtime_epoch() {
        let source = AuthoritySource::Provider {
            registration_id: "registration-1".to_owned(),
            provider: Arc::new(TestProvider),
        };
        let authority = resolve_authority(&source, "runtime-1").await.unwrap();
        let metadata = authority.provider_metadata().unwrap();
        assert_eq!(metadata.runtime_epoch, "runtime-1");
        assert_eq!(metadata.registration_id, "registration-1");
        assert!(authority.accepts_request_id("provider_namespace_012345:mutation-1"));
    }

    #[tokio::test]
    async fn http_guard_rejects_origin_and_authenticates_before_dispatch() {
        let token = AuthorizationToken::new("a".repeat(64));
        let guard = HttpRequestGuard {
            authorization: Arc::new(token),
            inflight: Arc::new(Semaphore::new(1)),
            request_timeout: Duration::from_secs(30),
        };
        let router = Router::new()
            .route("/mcp", post(|| async { StatusCode::NO_CONTENT }))
            .layer(middleware::from_fn_with_state(guard, authorize_http));

        let unauthorized = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let forbidden_origin = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::ORIGIN, "https://untrusted.example")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden_origin.status(), StatusCode::FORBIDDEN);

        let accepted = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(64)))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::NO_CONTENT);

        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let blocking = Router::new()
            .route(
                "/mcp",
                post({
                    let entered = Arc::clone(&entered);
                    let release = Arc::clone(&release);
                    move || {
                        let entered = Arc::clone(&entered);
                        let release = Arc::clone(&release);
                        async move {
                            entered.notify_one();
                            release.notified().await;
                            StatusCode::NO_CONTENT
                        }
                    }
                }),
            )
            .layer(middleware::from_fn_with_state(
                HttpRequestGuard {
                    authorization: Arc::new(AuthorizationToken::new("a".repeat(64))),
                    inflight: Arc::new(Semaphore::new(1)),
                    request_timeout: Duration::from_secs(30),
                },
                authorize_http,
            ));
        let request = || {
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(header::AUTHORIZATION, format!("Bearer {}", "a".repeat(64)))
                .body(Body::empty())
                .unwrap()
        };
        let first = tokio::spawn(blocking.clone().oneshot(request()));
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("first authenticated request admitted");
        let busy = blocking.oneshot(request()).await.expect("busy response");
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        release.notify_one();
        assert_eq!(
            first
                .await
                .expect("first request task")
                .expect("first response")
                .status(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn tcp_connection_and_slow_body_are_bounded_and_shutdown_is_prompt() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind loopback test listener");
        let address = listener.local_addr().expect("test listener address");
        let cancellation = CancellationToken::new();
        let mut config = McpHttpConfig::new(AuthorizationToken::new("a".repeat(64)));
        config.max_connections = 1;
        config.max_inflight_requests = 1;
        config.io_idle_timeout = Duration::from_millis(120);
        config.connection_lifetime = Duration::from_secs(2);
        config.request_timeout = Duration::from_millis(100);
        let bounded = BoundedTcpListener::new(listener, &config, cancellation.clone());
        let router = Router::new()
            .route(
                "/mcp",
                post(|request: axum::extract::Request| async move {
                    let _ = to_bytes(request.into_body(), 1024).await;
                    StatusCode::NO_CONTENT
                }),
            )
            .layer(middleware::from_fn_with_state(
                HttpRequestGuard {
                    authorization: Arc::new(AuthorizationToken::new("a".repeat(64))),
                    inflight: Arc::new(Semaphore::new(1)),
                    request_timeout: Duration::from_millis(100),
                },
                authorize_http,
            ));
        let serving = tokio::spawn({
            let cancellation = cancellation.clone();
            async move {
                axum::serve(bounded, router)
                    .with_graceful_shutdown(cancellation.cancelled_owned())
                    .await
            }
        });

        let _idle = TcpStream::connect(address)
            .await
            .expect("connect idle client");
        tokio::time::sleep(Duration::from_millis(25)).await;
        let mut slow_body = TcpStream::connect(address)
            .await
            .expect("connect queued slow client");
        slow_body
            .write_all(
                format!(
                    "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Length: 10\r\n\r\nx",
                    "a".repeat(64)
                )
                .as_bytes(),
            )
            .await
            .expect("write partial authenticated request");
        let mut response = [0_u8; 1024];
        assert!(
            timeout(Duration::from_millis(50), slow_body.read(&mut response))
                .await
                .is_err(),
            "second client must not be served while the sole connection permit is occupied"
        );
        let response_bytes = timeout(Duration::from_secs(2), slow_body.read(&mut response))
            .await
            .expect("slow request must reach a bounded response deadline")
            .expect("read slow request response");
        let response_text = std::str::from_utf8(&response[..response_bytes])
            .expect("HTTP response is UTF-8 headers");
        assert!(response_text.starts_with("HTTP/1.1 408"), "{response_text}");

        cancellation.cancel();
        timeout(Duration::from_secs(1), serving)
            .await
            .expect("HTTP shutdown must not wait for connection timeouts")
            .expect("HTTP serving task")
            .expect("HTTP serving result");
    }
}
