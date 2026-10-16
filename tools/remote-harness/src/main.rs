//! Two-process mTLS remote gateway conformance harness.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use clap::{Parser, Subcommand};
use nexus_cua_host::{
    DesktopWorker, EnvironmentDescriptor, GATEWAY_PROTOCOL_VERSION, GatewayArtifactOutcome,
    GatewayArtifactRequest, GatewayExecuteRequest, GatewayOutcome, GatewayResult,
    GatewayTransportError, GatewayWireRequest, GatewayWireResponse, WorkerGateway,
    WorkerGatewayConfig, WorkerLease, WorkerRegistry, WorkerRegistryConfig, WorkerRequirements,
    gateway_round_trip, serve_gateway_stream,
};
use nexus_cua_protocol::{
    Action, ActionEffectStatus, ActionEvidenceKind, ActionOutput, ArtifactRef, AuthorizationToken,
    Command, CommandResult, CuaError, DeliveryMode, ErrorCode, ForegroundRestorationStatus,
    MutationAdmission, MutationDisposition, MutationReconciliation, ObservationId,
    ObserveWindowInput, PerformActionInput, PixelSize, Platform, ReconcileMutationInput, RequestId,
    ResponseEnvelope, ResponseOutcome, ScreenRect, ScreenshotArtifact, ScreenshotMapping,
    SessionId, WindowObservation, WindowRef,
};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tokio_rustls::{TlsAcceptor, TlsConnector, client::TlsStream};
use uuid::Uuid;

const EVIDENCE_SCHEMA: &str = "nexus.cua.remote-transport.v1";
const FIXTURE_ARTIFACT: &[u8] = b"\x89PNG\r\n\x1a\nfixture-remote-observation";
const MAX_CONNECTIONS: usize = 9;
const EXPECTED_TLS_CONNECTIONS: usize = 8;
const EXPECTED_TLS_REJECTIONS: usize = 1;
const MUTATION_LEDGER_ENTRIES: usize = 16;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Parser)]
#[command(name = "nexus-cua-remote-harness")]
struct Cli {
    #[command(subcommand)]
    command: HarnessCommand,
}

#[derive(Debug, Subcommand)]
enum HarnessCommand {
    /// Run the two-process conformance sequence and write sanitized evidence.
    Run {
        /// Exact lowercase 40-character source revision under validation.
        #[arg(long)]
        source_revision: String,
        /// New JSON evidence path. Existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
        /// Maximum encoded gateway frame size.
        #[arg(long, default_value_t = 64 * 1024)]
        max_frame_bytes: usize,
    },
    /// Internal worker mode. Not a public deployment interface.
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        bootstrap: PathBuf,
        #[arg(long)]
        ca_cert: PathBuf,
        #[arg(long)]
        server_cert: PathBuf,
        #[arg(long)]
        server_key: PathBuf,
        #[arg(long)]
        max_frame_bytes: usize,
    },
}

#[derive(Debug, Error)]
enum HarnessError {
    #[error("invalid harness configuration")]
    InvalidConfiguration,
    #[error("harness I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("harness JSON failed")]
    Json(#[from] serde_json::Error),
    #[error("certificate generation failed")]
    Certificate(#[from] rcgen::Error),
    #[error("TLS configuration or handshake failed")]
    Tls(String),
    #[error("gateway transport failed: {0}")]
    Gateway(#[from] GatewayTransportError),
    #[error("worker registry failed")]
    WorkerRegistry,
    #[error("worker bootstrap timed out")]
    BootstrapTimeout,
    #[error("worker process failed")]
    WorkerProcess,
    #[error("remote transport conformance assertion failed: {0}")]
    Assertion(&'static str),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerBootstrap {
    address: SocketAddr,
    authorization: AuthorizationToken,
    lease: WorkerLease,
    binary_sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteTransportEvidence {
    schema_version: String,
    source_revision: String,
    controller_binary_sha256: String,
    worker_binary_sha256: String,
    certificate_identity_sha256: String,
    transport: &'static str,
    gateway_protocol: &'static str,
    process_count: usize,
    resource_bounds: ResourceBounds,
    scenarios: Vec<ScenarioEvidence>,
    accepted: bool,
    issues: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ResourceBounds {
    max_connections: usize,
    max_frame_bytes: usize,
    max_inflight_requests: usize,
    max_artifact_tickets: usize,
    max_artifact_bytes: u64,
    max_request_timeout_ms: u32,
    mutation_ledger_entries: usize,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ScenarioEvidence {
    scenario: &'static str,
    disposition: &'static str,
    accepted: bool,
    evidence: String,
}

struct TemporaryRoot(PathBuf);

impl TemporaryRoot {
    fn create() -> Result<Self, HarnessError> {
        let path = std::env::temp_dir().join(format!(
            "nexus-cua-remote-harness-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&path)?;
        set_owner_private_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct CertificateMaterial {
    ca: Vec<u8>,
    server_cert: Vec<u8>,
    server_key: Vec<u8>,
    client_cert: Vec<u8>,
    client_key: Vec<u8>,
    untrusted_client_cert: Vec<u8>,
    untrusted_client_key: Vec<u8>,
}

#[derive(Clone, Copy)]
struct ClientIdentity<'a> {
    certificate: &'a [u8],
    private_key: &'a [u8],
}

#[tokio::main]
async fn main() {
    if rustls::crypto::ring::default_provider()
        .install_default()
        .is_err()
    {
        eprintln!("TLS provider initialization failed");
        std::process::exit(2);
    }
    let result = match Cli::parse().command {
        HarnessCommand::Run {
            source_revision,
            output,
            max_frame_bytes,
        } => run_controller(&source_revision, &output, max_frame_bytes).await,
        HarnessCommand::Worker {
            bootstrap,
            ca_cert,
            server_cert,
            server_key,
            max_frame_bytes,
        } => {
            run_worker(
                &bootstrap,
                &ca_cert,
                &server_cert,
                &server_key,
                max_frame_bytes,
            )
            .await
        }
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn validate_source_revision(value: &str) -> Result<(), HarnessError> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(HarnessError::InvalidConfiguration)
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn current_binary_digest() -> Result<String, HarnessError> {
    Ok(digest(&fs::read(std::env::current_exe()?)?))
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), HarnessError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    set_owner_private_path(path, false)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_private_dir(path: &Path) -> Result<(), HarnessError> {
    set_owner_private_path(path, true)
}

#[cfg(unix)]
fn set_owner_private_path(path: &Path, directory: bool) -> Result<(), HarnessError> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = if directory { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(windows)]
fn set_owner_private_dir(path: &Path) -> Result<(), HarnessError> {
    set_owner_private_path(path, true)
}

#[cfg(windows)]
fn set_owner_private_path(path: &Path, _directory: bool) -> Result<(), HarnessError> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };
    use windows::core::PCWSTR;

    const OWNER_SYSTEM_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";
    let sddl = OWNER_SYSTEM_SDDL
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &raw mut descriptor,
            None,
        )
    }
    .map_err(|_| HarnessError::Io(std::io::Error::last_os_error()))?;
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let applied = unsafe {
        SetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    if applied.as_bool() {
        Ok(())
    } else {
        Err(HarnessError::Io(std::io::Error::last_os_error()))
    }
}

#[cfg(not(any(unix, windows)))]
fn set_owner_private_dir(_path: &Path) -> Result<(), HarnessError> {
    Err(HarnessError::InvalidConfiguration)
}

#[cfg(not(any(unix, windows)))]
fn set_owner_private_path(_path: &Path, _directory: bool) -> Result<(), HarnessError> {
    Err(HarnessError::InvalidConfiguration)
}

fn certificate_params(common_name: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, common_name);
    params.distinguished_name = distinguished_name;
    params
}

fn issue_leaf(
    issuer: &CertifiedIssuer<'_, KeyPair>,
    common_name: &str,
    usages: Vec<ExtendedKeyUsagePurpose>,
    subject_alt_names: Vec<String>,
) -> Result<(Vec<u8>, Vec<u8>), HarnessError> {
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(subject_alt_names)?;
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, common_name);
    params.distinguished_name = distinguished_name;
    params.extended_key_usages = usages;
    let certificate = params.signed_by(&key, issuer)?;
    Ok((certificate.der().to_vec(), key.serialize_der()))
}

fn generate_certificates() -> Result<CertificateMaterial, HarnessError> {
    let mut ca_params = certificate_params("nexus-cua temporary harness CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let issuer = CertifiedIssuer::self_signed(ca_params, KeyPair::generate()?)?;
    let (server_cert, server_key) = issue_leaf(
        &issuer,
        "nexus-cua harness worker",
        vec![ExtendedKeyUsagePurpose::ServerAuth],
        vec!["localhost".to_owned()],
    )?;
    let (client_cert, client_key) = issue_leaf(
        &issuer,
        "nexus-cua harness controller",
        vec![ExtendedKeyUsagePurpose::ClientAuth],
        Vec::new(),
    )?;

    let mut untrusted_ca_params = certificate_params("untrusted temporary harness CA");
    untrusted_ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let untrusted_issuer = CertifiedIssuer::self_signed(untrusted_ca_params, KeyPair::generate()?)?;
    let (untrusted_client_cert, untrusted_client_key) = issue_leaf(
        &untrusted_issuer,
        "untrusted harness controller",
        vec![ExtendedKeyUsagePurpose::ClientAuth],
        Vec::new(),
    )?;

    Ok(CertificateMaterial {
        ca: issuer.der().to_vec(),
        server_cert,
        server_key,
        client_cert,
        client_key,
        untrusted_client_cert,
        untrusted_client_key,
    })
}

fn certificate_chain(bytes: &[u8]) -> Vec<CertificateDer<'static>> {
    vec![CertificateDer::from(bytes.to_vec())]
}

fn private_key(bytes: &[u8]) -> PrivateKeyDer<'static> {
    PrivatePkcs8KeyDer::from(bytes.to_vec()).into()
}

fn server_tls_config(
    ca: &[u8],
    certificate: &[u8],
    key: &[u8],
) -> Result<ServerConfig, HarnessError> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca.to_vec()))
        .map_err(|error| HarnessError::Tls(error.to_string()))?;
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|error| HarnessError::Tls(error.to_string()))?;
    ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificate_chain(certificate), private_key(key))
        .map_err(|error| HarnessError::Tls(error.to_string()))
}

fn client_tls_config(
    ca: &[u8],
    identity: ClientIdentity<'_>,
) -> Result<ClientConfig, HarnessError> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca.to_vec()))
        .map_err(|error| HarnessError::Tls(error.to_string()))?;
    ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            certificate_chain(identity.certificate),
            private_key(identity.private_key),
        )
        .map_err(|error| HarnessError::Tls(error.to_string()))
}

async fn connect_tls(
    address: SocketAddr,
    config: ClientConfig,
) -> Result<TlsStream<TcpStream>, HarnessError> {
    let stream = timeout(IO_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| HarnessError::BootstrapTimeout)??;
    let name = ServerName::try_from("localhost".to_owned())
        .map_err(|error| HarnessError::Tls(error.to_string()))?;
    timeout(
        IO_TIMEOUT,
        TlsConnector::from(Arc::new(config)).connect(name, stream),
    )
    .await
    .map_err(|_| HarnessError::BootstrapTimeout)?
    .map_err(|error| HarnessError::Tls(error.to_string()))
}

#[derive(Default)]
struct FixtureState {
    mutations: HashMap<RequestId, CommandResult>,
}

struct FixtureWorker {
    environment: EnvironmentDescriptor,
    state: Mutex<FixtureState>,
}

impl FixtureWorker {
    fn new() -> Self {
        Self {
            environment: EnvironmentDescriptor::local_user_desktop(),
            state: Mutex::new(FixtureState::default()),
        }
    }
}

#[async_trait]
impl DesktopWorker for FixtureWorker {
    fn platform(&self) -> Platform {
        Platform::Macos
    }

    fn environment(&self) -> &EnvironmentDescriptor {
        &self.environment
    }

    async fn dispatch(
        &self,
        request_id: RequestId,
        _timeout_ms: u32,
        command: Command,
    ) -> ResponseEnvelope {
        let result = match command {
            Command::ObserveWindow(_) => fixture_observation(),
            Command::PerformAction(_) => {
                sleep(Duration::from_millis(150)).await;
                let mut state = self.state.lock().await;
                if let Some(result) = state.mutations.get(&request_id) {
                    result.clone()
                } else if state.mutations.len() >= MUTATION_LEDGER_ENTRIES {
                    return error_response(request_id, ErrorCode::Busy);
                } else {
                    let result = fixture_action_output();
                    state.mutations.insert(request_id.clone(), result.clone());
                    result
                }
            }
            Command::ReconcileMutation(input) => {
                let state = self.state.lock().await;
                let (admission, disposition, output) = match state.mutations.get(&input.request_id)
                {
                    Some(CommandResult::ActionPerformed(output)) => (
                        MutationAdmission::Admitted,
                        MutationDisposition::Succeeded,
                        Some(output),
                    ),
                    _ => (
                        MutationAdmission::Unknown,
                        MutationDisposition::Unknown,
                        None,
                    ),
                };
                CommandResult::MutationReconciled(MutationReconciliation {
                    request_id: input.request_id,
                    runtime_epoch: "runtime_fixture".to_owned(),
                    worker_epoch: None,
                    admission,
                    disposition,
                    dispatched: output.map(|value| value.dispatched),
                    effect_status: output.map(|value| value.effect_status),
                    evidence_kind: output.map(|value| value.evidence_kind),
                    error_code: None,
                    fresh_observation_required: output
                        .is_some_and(|value| value.observation_invalidated),
                })
            }
            _ => CommandResult::Acknowledged,
        };
        ResponseEnvelope {
            protocol_version: nexus_cua_protocol::PROTOCOL_VERSION.to_owned(),
            request_id,
            outcome: ResponseOutcome::Success { result },
        }
    }

    async fn read_artifact(
        &self,
        artifact: &ScreenshotArtifact,
        max_bytes: u64,
    ) -> Result<Vec<u8>, CuaError> {
        if artifact.artifact_ref.as_str() == "artifact_remote_fixture"
            && artifact.byte_length == FIXTURE_ARTIFACT.len() as u64
            && artifact.byte_length <= max_bytes
            && artifact.sha256 == digest(FIXTURE_ARTIFACT)
        {
            Ok(FIXTURE_ARTIFACT.to_vec())
        } else {
            Err(public_error(ErrorCode::ReferenceNotFound))
        }
    }
}

fn fixture_observation() -> CommandResult {
    CommandResult::WindowObserved(Box::new(WindowObservation {
        observation_id: ObservationId::new("observation_remote_fixture"),
        window_ref: WindowRef::new("window_remote_fixture"),
        captured_at: "2026-09-02T00:00:00Z".to_owned(),
        window_screen_bounds: ScreenRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        },
        screenshot: Some(ScreenshotArtifact {
            artifact_ref: ArtifactRef::new("artifact_remote_fixture"),
            path: "/private/worker/path-must-not-cross-gateway.png".to_owned(),
            mime_type: "image/png".to_owned(),
            mapping: ScreenshotMapping {
                screen_bounds: ScreenRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                pixel_size: PixelSize {
                    width: 1,
                    height: 1,
                },
            },
            byte_length: FIXTURE_ARTIFACT.len() as u64,
            sha256: digest(FIXTURE_ARTIFACT),
        }),
        elements: Vec::new(),
        elements_complete: true,
        elements_truncation: None,
    }))
}

fn fixture_action_output() -> CommandResult {
    CommandResult::ActionPerformed(ActionOutput {
        delivery_mode: DeliveryMode::System,
        dispatched: true,
        effect_status: ActionEffectStatus::Confirmed,
        evidence_kind: ActionEvidenceKind::NativeReadback,
        foreground_restoration: ForegroundRestorationStatus::NotRequested,
        observation_invalidated: true,
    })
}

fn public_error(code: ErrorCode) -> CuaError {
    CuaError {
        code,
        message: "fixture request failed".to_owned(),
        retryable: false,
        recovery_action: None,
        mutation_status: nexus_cua_protocol::MutationStatus::NotApplicable,
    }
}

fn error_response(request_id: RequestId, code: ErrorCode) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: nexus_cua_protocol::PROTOCOL_VERSION.to_owned(),
        request_id,
        outcome: ResponseOutcome::Error {
            error: public_error(code),
        },
    }
}

async fn run_worker(
    bootstrap_path: &Path,
    ca_path: &Path,
    server_cert_path: &Path,
    server_key_path: &Path,
    max_frame_bytes: usize,
) -> Result<(), HarnessError> {
    if !(1024..=64 * 1024 * 1024).contains(&max_frame_bytes) {
        return Err(HarnessError::InvalidConfiguration);
    }
    let tls = server_tls_config(
        &fs::read(ca_path)?,
        &fs::read(server_cert_path)?,
        &fs::read(server_key_path)?,
    )?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;

    let fixture = Arc::new(FixtureWorker::new());
    let registry = Arc::new(
        WorkerRegistry::new(WorkerRegistryConfig {
            max_workers: 2,
            max_leases: 4,
            max_lease_ttl: Duration::from_secs(60),
        })
        .map_err(|_| HarnessError::WorkerRegistry)?,
    );
    let descriptor = registry
        .register(fixture.clone(), 1)
        .await
        .map_err(|_| HarnessError::WorkerRegistry)?;
    let lease = registry
        .acquire(&WorkerRequirements::default(), Duration::from_secs(30))
        .await
        .map_err(|_| HarnessError::WorkerRegistry)?;
    let authorization = AuthorizationToken::new(format!(
        "{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    ));
    let gateway_config = WorkerGatewayConfig {
        max_inflight_requests: 4,
        max_artifact_tickets: 4,
        artifact_ticket_ttl: Duration::from_secs(30),
        max_artifact_bytes: 4 * 1024,
        max_request_timeout_ms: 5_000,
    };
    let gateway = Arc::new(
        WorkerGateway::new(Arc::clone(&registry), &authorization, gateway_config)
            .map_err(|_| HarnessError::WorkerRegistry)?,
    );
    let bootstrap = WorkerBootstrap {
        address: listener.local_addr()?,
        authorization,
        lease,
        binary_sha256: current_binary_digest()?,
    };
    write_private_new(bootstrap_path, &serde_json::to_vec(&bootstrap)?)?;

    let mut tls_connections = 0_usize;
    let mut tls_rejections = 0_usize;
    for _ in 0..MAX_CONNECTIONS {
        let (socket, _) = timeout(IO_TIMEOUT, listener.accept())
            .await
            .map_err(|_| HarnessError::BootstrapTimeout)??;
        let stream = match timeout(IO_TIMEOUT, acceptor.accept(socket)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(_)) => {
                tls_rejections += 1;
                continue;
            }
            Err(_) => return Err(HarnessError::BootstrapTimeout),
        };
        tls_connections += 1;
        let result = serve_gateway_stream(Arc::clone(&gateway), stream, max_frame_bytes).await;
        match tls_connections {
            5 => {}
            8 if matches!(result, Err(GatewayTransportError::FrameTooLarge)) => {}
            8 => return Err(HarnessError::Assertion("oversized frame was not rejected")),
            _ => result?,
        }
        if tls_connections == 6 {
            if !registry.unregister(&descriptor.worker_id).await {
                return Err(HarnessError::WorkerRegistry);
            }
            registry
                .register(fixture.clone(), 1)
                .await
                .map_err(|_| HarnessError::WorkerRegistry)?;
        }
    }
    if tls_connections != EXPECTED_TLS_CONNECTIONS || tls_rejections != EXPECTED_TLS_REJECTIONS {
        return Err(HarnessError::Assertion(
            "unexpected accepted or rejected TLS connection count",
        ));
    }
    Ok(())
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    async fn finish(mut self) -> Result<(), HarnessError> {
        for _ in 0..500 {
            let Some(child) = self.0.as_mut() else {
                return Err(HarnessError::WorkerProcess);
            };
            if let Some(status) = child.try_wait()? {
                self.0 = None;
                return if status.success() {
                    Ok(())
                } else {
                    Err(HarnessError::WorkerProcess)
                };
            }
            sleep(Duration::from_millis(10)).await;
        }
        Err(HarnessError::WorkerProcess)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn wait_for_bootstrap(path: &Path) -> Result<WorkerBootstrap, HarnessError> {
    for _ in 0..500 {
        match fs::read(path) {
            Ok(bytes) => return Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(HarnessError::BootstrapTimeout)
}

fn spawn_worker(
    bootstrap: &Path,
    ca_cert: &Path,
    server_cert: &Path,
    server_key: &Path,
    max_frame_bytes: usize,
) -> Result<ChildGuard, HarnessError> {
    let child = ProcessCommand::new(std::env::current_exe()?)
        .arg("worker")
        .arg("--bootstrap")
        .arg(bootstrap)
        .arg("--ca-cert")
        .arg(ca_cert)
        .arg("--server-cert")
        .arg(server_cert)
        .arg("--server-key")
        .arg(server_key)
        .arg("--max-frame-bytes")
        .arg(max_frame_bytes.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    Ok(ChildGuard(Some(child)))
}

fn good_identity(certificates: &CertificateMaterial) -> ClientIdentity<'_> {
    ClientIdentity {
        certificate: &certificates.client_cert,
        private_key: &certificates.client_key,
    }
}

async fn round_trip(
    bootstrap: &WorkerBootstrap,
    certificates: &CertificateMaterial,
    request: &GatewayWireRequest,
    max_frame_bytes: usize,
) -> Result<GatewayWireResponse, HarnessError> {
    let config = client_tls_config(&certificates.ca, good_identity(certificates))?;
    let mut stream = connect_tls(bootstrap.address, config).await?;
    let response = timeout(
        IO_TIMEOUT,
        gateway_round_trip(&mut stream, request, max_frame_bytes),
    )
    .await
    .map_err(|_| HarnessError::BootstrapTimeout)??;
    timeout(IO_TIMEOUT, stream.shutdown())
        .await
        .map_err(|_| HarnessError::BootstrapTimeout)??;
    Ok(response)
}

fn execute_request(
    bootstrap: &WorkerBootstrap,
    request_id: &str,
    command: Command,
) -> GatewayWireRequest {
    GatewayWireRequest::Execute(GatewayExecuteRequest {
        protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
        request_id: RequestId::new(request_id),
        timeout_ms: 2_000,
        authorization: bootstrap.authorization.clone(),
        lease_id: bootstrap.lease.lease_id.clone(),
        worker_epoch: bootstrap.lease.worker_epoch.clone(),
        command,
    })
}

fn expect_execute(
    response: GatewayWireResponse,
) -> Result<nexus_cua_host::GatewayExecuteResponse, HarnessError> {
    match response {
        GatewayWireResponse::Execute(response) => Ok(response),
        GatewayWireResponse::Artifact(_) => Err(HarnessError::Assertion(
            "execute request returned artifact response",
        )),
    }
}

fn expect_gateway_error(
    response: GatewayWireResponse,
    expected: ErrorCode,
    require_no_worker: bool,
) -> Result<(), HarnessError> {
    let response = expect_execute(response)?;
    let GatewayOutcome::Error { error } = response.outcome else {
        return Err(HarnessError::Assertion(
            "gateway request unexpectedly succeeded",
        ));
    };
    if error.code != expected
        || (require_no_worker && (response.worker_id.is_some() || response.worker_epoch.is_some()))
    {
        return Err(HarnessError::Assertion(
            "gateway error or worker disclosure did not match",
        ));
    }
    Ok(())
}

async fn assert_untrusted_client_rejected(
    bootstrap: &WorkerBootstrap,
    certificates: &CertificateMaterial,
) -> Result<(), HarnessError> {
    let config = client_tls_config(
        &certificates.ca,
        ClientIdentity {
            certificate: &certificates.untrusted_client_cert,
            private_key: &certificates.untrusted_client_key,
        },
    )?;
    let Ok(mut stream) = connect_tls(bootstrap.address, config).await else {
        return Ok(());
    };
    let probe = async {
        stream.write_all(&0_u32.to_be_bytes()).await?;
        stream.flush().await?;
        let mut byte = [0_u8; 1];
        stream.read(&mut byte).await
    };
    match timeout(IO_TIMEOUT, probe).await {
        Ok(Err(_) | Ok(0)) => Ok(()),
        _ => Err(HarnessError::Assertion(
            "untrusted client certificate reached gateway framing",
        )),
    }
}

async fn send_and_disconnect(
    bootstrap: &WorkerBootstrap,
    certificates: &CertificateMaterial,
    request: &GatewayWireRequest,
    max_frame_bytes: usize,
) -> Result<(), HarnessError> {
    let payload = serde_json::to_vec(request)?;
    if payload.is_empty() || payload.len() > max_frame_bytes {
        return Err(HarnessError::Assertion(
            "disconnect request exceeded frame bound",
        ));
    }
    let length = u32::try_from(payload.len()).map_err(|_| HarnessError::InvalidConfiguration)?;
    let config = client_tls_config(&certificates.ca, good_identity(certificates))?;
    let mut stream = connect_tls(bootstrap.address, config).await?;
    timeout(IO_TIMEOUT, async {
        stream.write_all(&length.to_be_bytes()).await?;
        stream.write_all(&payload).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| HarnessError::BootstrapTimeout)??;
    drop(stream);
    Ok(())
}

async fn send_oversized_prefix(
    bootstrap: &WorkerBootstrap,
    certificates: &CertificateMaterial,
    max_frame_bytes: usize,
) -> Result<(), HarnessError> {
    let declared = u32::try_from(
        max_frame_bytes
            .checked_add(1)
            .ok_or(HarnessError::InvalidConfiguration)?,
    )
    .map_err(|_| HarnessError::InvalidConfiguration)?;
    let config = client_tls_config(&certificates.ca, good_identity(certificates))?;
    let mut stream = connect_tls(bootstrap.address, config).await?;
    timeout(IO_TIMEOUT, async {
        stream.write_all(&declared.to_be_bytes()).await?;
        stream.flush().await?;
        stream.shutdown().await
    })
    .await
    .map_err(|_| HarnessError::BootstrapTimeout)??;
    Ok(())
}

fn scenario(
    scenario: &'static str,
    disposition: &'static str,
    evidence: impl Into<String>,
) -> ScenarioEvidence {
    ScenarioEvidence {
        scenario,
        disposition,
        accepted: true,
        evidence: evidence.into(),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the linear sequence mirrors the required transport conformance ordering"
)]
async fn run_controller(
    source_revision: &str,
    output: &Path,
    max_frame_bytes: usize,
) -> Result<(), HarnessError> {
    validate_source_revision(source_revision)?;
    if !(1024..u32::MAX as usize).contains(&max_frame_bytes) || max_frame_bytes > 64 * 1024 * 1024 {
        return Err(HarnessError::InvalidConfiguration);
    }
    if output.exists() {
        return Err(HarnessError::InvalidConfiguration);
    }

    let temporary_root = TemporaryRoot::create()?;
    let certificates = generate_certificates()?;
    let ca_path = temporary_root.path().join("ca.der");
    let server_cert_path = temporary_root.path().join("server.der");
    let server_key_path = temporary_root.path().join("server-key.der");
    let bootstrap_path = temporary_root.path().join("worker-bootstrap.json");
    write_private_new(&ca_path, &certificates.ca)?;
    write_private_new(&server_cert_path, &certificates.server_cert)?;
    write_private_new(&server_key_path, &certificates.server_key)?;

    let child = spawn_worker(
        &bootstrap_path,
        &ca_path,
        &server_cert_path,
        &server_key_path,
        max_frame_bytes,
    )?;
    let bootstrap = wait_for_bootstrap(&bootstrap_path).await?;
    let binary_sha256 = current_binary_digest()?;
    if bootstrap.binary_sha256 != binary_sha256 {
        return Err(HarnessError::Assertion(
            "controller and worker executable digests differ",
        ));
    }

    let mut scenarios = Vec::with_capacity(8);
    assert_untrusted_client_rejected(&bootstrap, &certificates).await?;
    scenarios.push(scenario(
        "mutual_tls_rejects_untrusted_client",
        "rejected_before_framing",
        "an untrusted client certificate was rejected before gateway framing",
    ));

    let invalid_lease = serde_json::from_str("\"lease_invalid\"")?;
    let invalid_epoch = serde_json::from_str("\"epoch_invalid\"")?;
    let unauthorized = GatewayWireRequest::Execute(GatewayExecuteRequest {
        protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
        request_id: RequestId::new("request_unauthorized"),
        timeout_ms: 2_000,
        authorization: AuthorizationToken::new("x".repeat(64)),
        lease_id: invalid_lease,
        worker_epoch: invalid_epoch,
        command: Command::GetCapabilities,
    });
    expect_gateway_error(
        round_trip(&bootstrap, &certificates, &unauthorized, max_frame_bytes).await?,
        ErrorCode::Unauthorized,
        true,
    )?;
    scenarios.push(scenario(
        "authentication_precedes_worker_resolution",
        "unauthorized_not_resolved",
        "bad transport authority disclosed no worker identity",
    ));

    let observe = execute_request(
        &bootstrap,
        "request_observe",
        Command::ObserveWindow(ObserveWindowInput {
            session_id: SessionId::new("session_remote_fixture"),
            window_ref: WindowRef::new("window_remote_fixture"),
            include_screenshot: true,
            accessibility: nexus_cua_protocol::AccessibilityMode::Disabled,
        }),
    );
    let observe_response =
        expect_execute(round_trip(&bootstrap, &certificates, &observe, max_frame_bytes).await?)?;
    if observe_response.worker_id.as_ref() != Some(&bootstrap.lease.worker_id)
        || observe_response.worker_epoch.as_ref() != Some(&bootstrap.lease.worker_epoch)
    {
        return Err(HarnessError::Assertion(
            "valid round trip lost worker affinity",
        ));
    }
    let GatewayOutcome::Success {
        result: GatewayResult::WindowObserved(observation),
    } = observe_response.outcome
    else {
        return Err(HarnessError::Assertion(
            "valid observation round trip failed",
        ));
    };
    let observation_json = serde_json::to_string(&observation)?;
    if observation_json.contains("path") || observation_json.contains("/private/worker") {
        return Err(HarnessError::Assertion(
            "remote observation exposed worker-local path",
        ));
    }
    let screenshot = observation.screenshot.ok_or(HarnessError::Assertion(
        "observation omitted screenshot ticket",
    ))?;
    scenarios.push(scenario(
        "closed_gateway_round_trip",
        "succeeded",
        "a path-free observation preserved exact worker lease and epoch affinity",
    ));

    let stale_epoch = serde_json::from_str("\"epoch_stale\"")?;
    let stale_request = GatewayWireRequest::Execute(GatewayExecuteRequest {
        protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
        request_id: RequestId::new("request_stale_epoch"),
        timeout_ms: 2_000,
        authorization: bootstrap.authorization.clone(),
        lease_id: bootstrap.lease.lease_id.clone(),
        worker_epoch: stale_epoch,
        command: Command::GetCapabilities,
    });
    expect_gateway_error(
        round_trip(&bootstrap, &certificates, &stale_request, max_frame_bytes).await?,
        ErrorCode::SessionUnavailable,
        true,
    )?;
    scenarios.push(scenario(
        "lease_epoch_mismatch_fails_closed",
        "stale_lease",
        "a mismatched worker epoch was rejected before dispatch",
    ));

    let artifact_request = GatewayWireRequest::FetchArtifact(GatewayArtifactRequest {
        protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
        authorization: bootstrap.authorization.clone(),
        lease_id: bootstrap.lease.lease_id.clone(),
        worker_epoch: bootstrap.lease.worker_epoch.clone(),
        ticket_ref: screenshot.ticket_ref,
    });
    let GatewayWireResponse::Artifact(GatewayArtifactOutcome::Success { artifact }) = round_trip(
        &bootstrap,
        &certificates,
        &artifact_request,
        max_frame_bytes,
    )
    .await?
    else {
        return Err(HarnessError::Assertion("artifact ticket fetch failed"));
    };
    let artifact_bytes = base64::engine::general_purpose::STANDARD
        .decode(&artifact.data_base64)
        .map_err(|_| HarnessError::Assertion("artifact base64 was invalid"))?;
    if artifact.byte_length != artifact_bytes.len() as u64
        || artifact.sha256 != digest(&artifact_bytes)
        || artifact_bytes != FIXTURE_ARTIFACT
    {
        return Err(HarnessError::Assertion(
            "artifact length or digest did not verify",
        ));
    }
    scenarios.push(scenario(
        "path_free_artifact_ticket",
        "verified",
        format!(
            "ticket fetch verified {} bytes with SHA-256 metadata",
            artifact.byte_length
        ),
    ));

    let mutation_id = "request_disconnect_mutation";
    let mutation = execute_request(
        &bootstrap,
        mutation_id,
        Command::PerformAction(PerformActionInput {
            session_id: SessionId::new("session_remote_fixture"),
            window_ref: WindowRef::new("window_remote_fixture"),
            observation_id: ObservationId::new("observation_remote_fixture"),
            action: Action::FocusWindow,
        }),
    );
    send_and_disconnect(&bootstrap, &certificates, &mutation, max_frame_bytes).await?;
    let reconcile = execute_request(
        &bootstrap,
        "request_reconcile",
        Command::ReconcileMutation(ReconcileMutationInput {
            request_id: RequestId::new(mutation_id),
            wait_ms: 1_000,
        }),
    );
    let reconcile_response =
        expect_execute(round_trip(&bootstrap, &certificates, &reconcile, max_frame_bytes).await?)?;
    let GatewayOutcome::Success {
        result: GatewayResult::Core(CommandResult::MutationReconciled(reconciliation)),
    } = reconcile_response.outcome
    else {
        return Err(HarnessError::Assertion(
            "disconnected mutation did not reconcile",
        ));
    };
    if reconciliation.admission != MutationAdmission::Admitted
        || reconciliation.disposition != MutationDisposition::Succeeded
        || reconciliation.worker_epoch.as_deref() != Some(bootstrap.lease.worker_epoch.as_str())
    {
        return Err(HarnessError::Assertion(
            "reconciliation lost admission, result, or worker epoch",
        ));
    }
    scenarios.push(scenario(
        "post_admission_disconnect_reconciles",
        "reconciled_succeeded",
        "the mutation completed once and reconciled as succeeded on reconnect",
    ));

    expect_gateway_error(
        round_trip(&bootstrap, &certificates, &mutation, max_frame_bytes).await?,
        ErrorCode::SessionUnavailable,
        true,
    )?;
    scenarios.push(scenario(
        "worker_epoch_rotation_blocks_replay",
        "stale_lease",
        "the prior lease could not replay a mutation after worker re-registration",
    ));

    send_oversized_prefix(&bootstrap, &certificates, max_frame_bytes).await?;
    child.finish().await?;
    scenarios.push(scenario(
        "oversized_frame_rejected",
        "frame_too_large",
        "a declared frame one byte above the configured bound was rejected",
    ));

    let mut certificate_identity = Sha256::new();
    certificate_identity.update(&certificates.ca);
    certificate_identity.update(&certificates.server_cert);
    certificate_identity.update(&certificates.client_cert);
    let evidence = RemoteTransportEvidence {
        schema_version: EVIDENCE_SCHEMA.to_owned(),
        source_revision: source_revision.to_owned(),
        controller_binary_sha256: binary_sha256.clone(),
        worker_binary_sha256: binary_sha256,
        certificate_identity_sha256: hex::encode(certificate_identity.finalize()),
        transport: "tcp+mutual_tls_1_2_or_1_3",
        gateway_protocol: GATEWAY_PROTOCOL_VERSION,
        process_count: 2,
        resource_bounds: ResourceBounds {
            max_connections: MAX_CONNECTIONS,
            max_frame_bytes,
            max_inflight_requests: 4,
            max_artifact_tickets: 4,
            max_artifact_bytes: 4 * 1024,
            max_request_timeout_ms: 5_000,
            mutation_ledger_entries: MUTATION_LEDGER_ENTRIES,
        },
        scenarios,
        accepted: true,
        issues: Vec::new(),
    };
    let serialized = serde_json::to_vec_pretty(&evidence)?;
    if serialized
        .windows(bootstrap.authorization.expose().len())
        .any(|window| window == bootstrap.authorization.expose().as_bytes())
        || serialized
            .windows(bootstrap.lease.lease_id.as_str().len())
            .any(|window| window == bootstrap.lease.lease_id.as_str().as_bytes())
        || serialized
            .windows(temporary_root.path().as_os_str().len())
            .any(|window| window == temporary_root.path().as_os_str().as_encoded_bytes())
        || String::from_utf8_lossy(&serialized).contains("/private/worker")
    {
        return Err(HarnessError::Assertion(
            "sanitized evidence retained a secret or local path",
        ));
    }
    write_private_new(output, &serialized)
}
