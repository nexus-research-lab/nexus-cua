//! Authenticated lease/epoch-bound remote command and artifact projection.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use nexus_cua_protocol::{
    AccessibilityElement, ArtifactRef, AuthorizationToken, Command, CommandResult, CuaError,
    ErrorCode, MutationStatus, ObservationId, ObservationTruncation, PixelSize, RequestId,
    ResponseOutcome, ScreenRect, ScreenshotArtifact, WindowObservation, WindowRef,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{WorkerEpoch, WorkerError, WorkerId, WorkerLeaseId, WorkerRegistry};

/// Stable transport-neutral gateway contract identifier.
pub const GATEWAY_PROTOCOL_VERSION: &str = "nexus.cua.gateway.v1";

const MAX_REQUEST_ID_BYTES: usize = 128;
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 4 * 1024;

/// Authenticated remote command bound to one exact worker lease and epoch.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayExecuteRequest {
    /// Must equal [`GATEWAY_PROTOCOL_VERSION`].
    pub protocol_version: String,
    /// Stable retry identity reconciled by the selected worker dispatcher.
    pub request_id: RequestId,
    /// Bounded caller wait deadline.
    pub timeout_ms: u32,
    /// Gateway transport capability whose debug form is redacted.
    pub authorization: AuthorizationToken,
    /// Exact scheduler lease.
    pub lease_id: WorkerLeaseId,
    /// Exact worker health epoch.
    pub worker_epoch: WorkerEpoch,
    /// Unmodified core protocol command.
    pub command: Command,
}

/// Authenticated fetch for one checksum-bound screenshot ticket.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayArtifactRequest {
    /// Must equal [`GATEWAY_PROTOCOL_VERSION`].
    pub protocol_version: String,
    /// Gateway transport capability.
    pub authorization: AuthorizationToken,
    /// Exact scheduler lease used for the observation.
    pub lease_id: WorkerLeaseId,
    /// Exact worker health epoch.
    pub worker_epoch: WorkerEpoch,
    /// Opaque bounded artifact ticket returned by the gateway.
    pub ticket_ref: String,
}

/// Remote screenshot metadata without a worker-local filesystem path.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScreenshotArtifact {
    /// Original runtime artifact identity.
    pub artifact_ref: ArtifactRef,
    /// Gateway-local fetch capability.
    pub ticket_ref: String,
    /// MIME type; currently image/png.
    pub mime_type: String,
    /// Captured pixel dimensions.
    pub pixel_size: PixelSize,
    /// Exact encoded byte length.
    pub byte_length: u64,
    /// Lowercase SHA-256 of the encoded bytes.
    pub sha256: String,
}

/// Remote-safe observation projection with no local artifact path.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteWindowObservation {
    /// Fresh observation identity.
    pub observation_id: ObservationId,
    /// Exact session window identity.
    pub window_ref: WindowRef,
    /// RFC 3339 capture time.
    pub captured_at: String,
    /// Logical screen bounds used by semantic state.
    pub window_screen_bounds: ScreenRect,
    /// Optional separately fetched screenshot metadata.
    pub screenshot: Option<RemoteScreenshotArtifact>,
    /// Bounded accessibility elements.
    pub elements: Vec<AccessibilityElement>,
    /// Whether semantic traversal completed.
    pub elements_complete: bool,
    /// Explicit truncation evidence when incomplete.
    pub elements_truncation: Option<ObservationTruncation>,
}

/// Gateway success shape. Non-observation results preserve the core protocol;
/// observations use the path-free remote projection.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result_type", content = "data", rename_all = "snake_case")]
pub enum GatewayResult {
    /// A core result other than `window_observed`.
    Core(CommandResult),
    /// Path-free observation with an optional artifact ticket.
    WindowObserved(Box<RemoteWindowObservation>),
}

/// Remote command success/error outcome.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GatewayOutcome {
    /// Selected worker completed or replayed the request.
    Success {
        /// Projected result.
        result: GatewayResult,
    },
    /// Gateway, registry, or worker returned a stable public error.
    Error {
        /// Stable error without worker paths or credentials.
        error: CuaError,
    },
}

/// Lease/epoch-bound gateway response.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayExecuteResponse {
    /// Gateway contract identifier.
    pub protocol_version: String,
    /// Echoed stable request identity.
    pub request_id: RequestId,
    /// Exact selected worker.
    pub worker_id: Option<WorkerId>,
    /// Exact selected worker epoch.
    pub worker_epoch: Option<WorkerEpoch>,
    /// Projected success/error outcome.
    pub outcome: GatewayOutcome,
}

/// Verified encoded artifact body. JSON adapters serialize bytes as base64
/// instead of an unbounded integer array.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayArtifactResponse {
    /// Original runtime artifact identity.
    pub artifact_ref: ArtifactRef,
    /// MIME type.
    pub mime_type: String,
    /// Exact encoded byte length.
    pub byte_length: u64,
    /// Lowercase SHA-256 digest.
    pub sha256: String,
    /// Standard padded base64 payload.
    pub data_base64: String,
}

/// Gateway admission, ticket, and response bounds.
#[derive(Clone, Debug)]
pub struct WorkerGatewayConfig {
    /// Maximum concurrent gateway calls.
    pub max_inflight_requests: usize,
    /// Maximum unexpired screenshot tickets.
    pub max_artifact_tickets: usize,
    /// Screenshot ticket lifetime.
    pub artifact_ticket_ttl: Duration,
    /// Maximum screenshot payload.
    pub max_artifact_bytes: u64,
    /// Maximum remote wait deadline.
    pub max_request_timeout_ms: u32,
}

impl Default for WorkerGatewayConfig {
    fn default() -> Self {
        Self {
            max_inflight_requests: 64,
            max_artifact_tickets: 256,
            artifact_ticket_ttl: Duration::from_secs(5 * 60),
            max_artifact_bytes: 16 * 1024 * 1024,
            max_request_timeout_ms: 120_000,
        }
    }
}

struct TokenVerifier(Zeroizing<Vec<u8>>);

impl TokenVerifier {
    fn new(token: &AuthorizationToken) -> Option<Self> {
        let bytes = token.expose().as_bytes();
        (MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES)
            .contains(&bytes.len())
            .then(|| Self(Zeroizing::new(bytes.to_vec())))
    }

    fn accepts(&self, candidate: &AuthorizationToken) -> bool {
        let bytes = candidate.expose().as_bytes();
        bytes.len() == self.0.len() && self.0.as_slice().ct_eq(bytes).into()
    }
}

struct ArtifactTicket {
    lease_id: WorkerLeaseId,
    worker_epoch: WorkerEpoch,
    artifact: ScreenshotArtifact,
    expires_at: Instant,
}

/// Authenticated transport-neutral gateway above the bounded worker registry.
pub struct WorkerGateway {
    registry: Arc<WorkerRegistry>,
    token: TokenVerifier,
    config: WorkerGatewayConfig,
    inflight: Arc<Semaphore>,
    tickets: Mutex<HashMap<String, ArtifactTicket>>,
}

impl WorkerGateway {
    /// Creates a gateway without opening a network listener. A host may serve
    /// these closed request/response types over its authenticated TLS, SSH, or
    /// RPC channel.
    ///
    /// # Errors
    ///
    /// Returns an error when a token or resource bound is invalid.
    pub fn new(
        registry: Arc<WorkerRegistry>,
        token: &AuthorizationToken,
        config: WorkerGatewayConfig,
    ) -> Result<Self, WorkerError> {
        if config.max_inflight_requests == 0
            || config.max_artifact_tickets == 0
            || config.artifact_ticket_ttl.is_zero()
            || config.max_artifact_bytes == 0
            || config.max_request_timeout_ms == 0
        {
            return Err(WorkerError::Invalid(
                "gateway resource and lifetime bounds must be non-zero".to_owned(),
            ));
        }
        let token = TokenVerifier::new(token).ok_or_else(|| {
            WorkerError::Invalid("gateway token must contain 32 to 4096 bytes".to_owned())
        })?;
        Ok(Self {
            registry,
            token,
            inflight: Arc::new(Semaphore::new(config.max_inflight_requests)),
            config,
            tickets: Mutex::new(HashMap::new()),
        })
    }

    /// Dispatches or reconciles one request on the exact leased worker epoch.
    pub async fn execute(&self, request: GatewayExecuteRequest) -> GatewayExecuteResponse {
        let request_id = request.request_id.clone();
        let mutation = matches!(
            request.command,
            Command::LaunchApplication(_) | Command::WriteClipboard(_) | Command::PerformAction(_)
        );
        let failure = |error| GatewayExecuteResponse {
            protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
            request_id: request_id.clone(),
            worker_id: None,
            worker_epoch: None,
            outcome: GatewayOutcome::Error {
                error: gateway_error(&error, mutation),
            },
        };
        if request.protocol_version != GATEWAY_PROTOCOL_VERSION {
            return failure(GatewayFailure::Protocol);
        }
        if !self.token.accepts(&request.authorization) {
            return failure(GatewayFailure::Unauthorized);
        }
        if request.request_id.as_str().is_empty()
            || request.request_id.as_str().len() > MAX_REQUEST_ID_BYTES
            || request.request_id.as_str().trim() != request.request_id.as_str()
            || request.timeout_ms == 0
            || request.timeout_ms > self.config.max_request_timeout_ms
        {
            return failure(GatewayFailure::Invalid);
        }
        let Ok(_permit) = Arc::clone(&self.inflight).try_acquire_owned() else {
            return failure(GatewayFailure::Busy);
        };
        let (lease, worker) = match self
            .registry
            .resolve(&request.lease_id, &request.worker_epoch)
            .await
        {
            Ok(value) => value,
            Err(error) => return failure(GatewayFailure::Worker(error)),
        };
        let response = worker
            .dispatch(request.request_id, request.timeout_ms, request.command)
            .await;
        let outcome = match response.outcome {
            ResponseOutcome::Success { result } => match self
                .project_result(&lease.lease_id, &lease.worker_epoch, result)
                .await
            {
                Ok(result) => GatewayOutcome::Success { result },
                Err(error) => GatewayOutcome::Error { error },
            },
            ResponseOutcome::Error { error } => GatewayOutcome::Error { error },
        };
        GatewayExecuteResponse {
            protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
            request_id: response.request_id,
            worker_id: Some(lease.worker_id),
            worker_epoch: Some(lease.worker_epoch),
            outcome,
        }
    }

    /// Fetches a checksum-bound artifact without exposing the worker path.
    ///
    /// # Errors
    ///
    /// Returns a stable public error for authentication, stale affinity,
    /// expired ticket, changed bytes, or capacity failure.
    pub async fn fetch_artifact(
        &self,
        request: GatewayArtifactRequest,
    ) -> Result<GatewayArtifactResponse, CuaError> {
        if request.protocol_version != GATEWAY_PROTOCOL_VERSION {
            return Err(gateway_error(&GatewayFailure::Protocol, false));
        }
        if !self.token.accepts(&request.authorization) {
            return Err(gateway_error(&GatewayFailure::Unauthorized, false));
        }
        let Ok(_permit) = Arc::clone(&self.inflight).try_acquire_owned() else {
            return Err(gateway_error(&GatewayFailure::Busy, false));
        };
        let (_lease, worker) = self
            .registry
            .resolve(&request.lease_id, &request.worker_epoch)
            .await
            .map_err(|error| gateway_error(&GatewayFailure::Worker(error), false))?;
        let artifact = {
            let mut tickets = self.tickets.lock().await;
            purge_tickets(&mut tickets);
            let ticket = tickets
                .get(&request.ticket_ref)
                .ok_or_else(|| gateway_error(&GatewayFailure::StaleArtifact, false))?;
            if ticket.lease_id != request.lease_id || ticket.worker_epoch != request.worker_epoch {
                return Err(gateway_error(&GatewayFailure::StaleArtifact, false));
            }
            ticket.artifact.clone()
        };
        let bytes = Zeroizing::new(
            worker
                .read_artifact(&artifact, self.config.max_artifact_bytes)
                .await?,
        );
        Ok(GatewayArtifactResponse {
            artifact_ref: artifact.artifact_ref,
            mime_type: artifact.mime_type,
            byte_length: artifact.byte_length,
            sha256: artifact.sha256,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes.as_slice()),
        })
    }

    async fn project_result(
        &self,
        lease_id: &WorkerLeaseId,
        worker_epoch: &WorkerEpoch,
        result: CommandResult,
    ) -> Result<GatewayResult, CuaError> {
        let observation = match result {
            CommandResult::MutationReconciled(mut reconciliation) => {
                reconciliation.worker_epoch = Some(worker_epoch.as_str().to_owned());
                return Ok(GatewayResult::Core(CommandResult::MutationReconciled(
                    reconciliation,
                )));
            }
            CommandResult::WindowObserved(observation) => observation,
            other => return Ok(GatewayResult::Core(other)),
        };
        let WindowObservation {
            observation_id,
            window_ref,
            captured_at,
            window_screen_bounds,
            screenshot,
            elements,
            elements_complete,
            elements_truncation,
        } = *observation;
        let screenshot = match screenshot {
            Some(artifact) => Some(self.mint_ticket(lease_id, worker_epoch, artifact).await?),
            None => None,
        };
        Ok(GatewayResult::WindowObserved(Box::new(
            RemoteWindowObservation {
                observation_id,
                window_ref,
                captured_at,
                window_screen_bounds,
                screenshot,
                elements,
                elements_complete,
                elements_truncation,
            },
        )))
    }

    async fn mint_ticket(
        &self,
        lease_id: &WorkerLeaseId,
        worker_epoch: &WorkerEpoch,
        artifact: ScreenshotArtifact,
    ) -> Result<RemoteScreenshotArtifact, CuaError> {
        if artifact.byte_length == 0 || artifact.byte_length > self.config.max_artifact_bytes {
            return Err(gateway_error(&GatewayFailure::StaleArtifact, false));
        }
        let mut tickets = self.tickets.lock().await;
        purge_tickets(&mut tickets);
        if tickets.len() >= self.config.max_artifact_tickets {
            return Err(gateway_error(&GatewayFailure::Busy, false));
        }
        let ticket_ref = format!("ticket_{}", Uuid::new_v4().simple());
        let projected = RemoteScreenshotArtifact {
            artifact_ref: artifact.artifact_ref.clone(),
            ticket_ref: ticket_ref.clone(),
            mime_type: artifact.mime_type.clone(),
            pixel_size: artifact.mapping.pixel_size,
            byte_length: artifact.byte_length,
            sha256: artifact.sha256.clone(),
        };
        tickets.insert(
            ticket_ref,
            ArtifactTicket {
                lease_id: lease_id.clone(),
                worker_epoch: worker_epoch.clone(),
                artifact,
                expires_at: Instant::now() + self.config.artifact_ticket_ttl,
            },
        );
        Ok(projected)
    }
}

fn purge_tickets(tickets: &mut HashMap<String, ArtifactTicket>) {
    let now = Instant::now();
    tickets.retain(|_, ticket| ticket.expires_at > now);
}

enum GatewayFailure {
    Protocol,
    Unauthorized,
    Invalid,
    Busy,
    StaleArtifact,
    Worker(WorkerError),
}

fn gateway_error(failure: &GatewayFailure, mutation: bool) -> CuaError {
    let (code, message, retryable, recovery) = match failure {
        GatewayFailure::Protocol => (
            ErrorCode::ProtocolMismatch,
            "unsupported worker gateway protocol",
            false,
            Some("negotiate_gateway_protocol"),
        ),
        GatewayFailure::Unauthorized => (
            ErrorCode::Unauthorized,
            "invalid worker gateway authorization",
            false,
            None,
        ),
        GatewayFailure::Invalid => (
            ErrorCode::InvalidRequest,
            "worker gateway request is invalid",
            false,
            None,
        ),
        GatewayFailure::Busy | GatewayFailure::Worker(WorkerError::Busy) => (
            ErrorCode::Busy,
            "worker gateway capacity is busy",
            true,
            Some("retry_after_capacity"),
        ),
        GatewayFailure::StaleArtifact => (
            ErrorCode::ReferenceNotFound,
            "remote screenshot artifact is stale",
            false,
            Some("observe_window_again"),
        ),
        GatewayFailure::Worker(WorkerError::StaleLease) => (
            ErrorCode::SessionUnavailable,
            "desktop worker lease or epoch is stale",
            false,
            Some("acquire_worker_lease"),
        ),
        GatewayFailure::Worker(_) => (
            ErrorCode::Internal,
            "desktop worker registry is unavailable",
            true,
            Some("retry_after_worker_recovery"),
        ),
    };
    CuaError {
        code,
        message: message.to_owned(),
        retryable,
        recovery_action: recovery.map(str::to_owned),
        mutation_status: if mutation {
            MutationStatus::NotDispatched
        } else {
            MutationStatus::NotApplicable
        },
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use nexus_cua_protocol::{
        AccessibilityMode, CommandResult, MutationAdmission, MutationDisposition,
        MutationReconciliation, ObserveWindowInput, Platform, ReconcileMutationInput,
        ResponseEnvelope, ScreenshotMapping, SessionId,
    };
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{DesktopWorker, EnvironmentDescriptor, WorkerRegistryConfig, WorkerRequirements};

    struct ObservationWorker {
        environment: EnvironmentDescriptor,
        bytes: Vec<u8>,
    }

    struct ReconciliationWorker {
        environment: EnvironmentDescriptor,
    }

    #[async_trait]
    impl DesktopWorker for ObservationWorker {
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
            _command: Command,
        ) -> ResponseEnvelope {
            ResponseEnvelope {
                protocol_version: nexus_cua_protocol::PROTOCOL_VERSION.to_owned(),
                request_id,
                outcome: ResponseOutcome::Success {
                    result: CommandResult::WindowObserved(Box::new(WindowObservation {
                        observation_id: ObservationId::new("observation_fixture"),
                        window_ref: WindowRef::new("window_fixture"),
                        captured_at: "2026-08-27T00:00:00Z".to_owned(),
                        window_screen_bounds: ScreenRect {
                            x: 0.0,
                            y: 0.0,
                            width: 1.0,
                            height: 1.0,
                        },
                        screenshot: Some(ScreenshotArtifact {
                            artifact_ref: ArtifactRef::new("artifact_fixture"),
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
                            byte_length: self.bytes.len() as u64,
                            sha256: hex::encode(Sha256::digest(&self.bytes)),
                        }),
                        elements: Vec::new(),
                        elements_complete: true,
                        elements_truncation: None,
                    })),
                },
            }
        }

        async fn read_artifact(
            &self,
            artifact: &ScreenshotArtifact,
            max_bytes: u64,
        ) -> Result<Vec<u8>, CuaError> {
            assert_eq!(artifact.artifact_ref.as_str(), "artifact_fixture");
            assert!(artifact.byte_length <= max_bytes);
            Ok(self.bytes.clone())
        }
    }

    #[async_trait]
    impl DesktopWorker for ReconciliationWorker {
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
            let Command::ReconcileMutation(input) = command else {
                panic!("fixture received unexpected command");
            };
            ResponseEnvelope {
                protocol_version: nexus_cua_protocol::PROTOCOL_VERSION.to_owned(),
                request_id,
                outcome: ResponseOutcome::Success {
                    result: CommandResult::MutationReconciled(MutationReconciliation {
                        request_id: input.request_id,
                        runtime_epoch: "runtime_fixture".to_owned(),
                        worker_epoch: None,
                        admission: MutationAdmission::Admitted,
                        disposition: MutationDisposition::Succeeded,
                        dispatched: Some(true),
                        effect_status: None,
                        evidence_kind: None,
                        error_code: None,
                        fresh_observation_required: true,
                    }),
                },
            }
        }

        async fn read_artifact(
            &self,
            _artifact: &ScreenshotArtifact,
            _max_bytes: u64,
        ) -> Result<Vec<u8>, CuaError> {
            panic!("reconciliation fixture has no artifacts");
        }
    }

    #[tokio::test]
    async fn observation_uses_path_free_ticket_bound_to_lease_and_epoch() {
        let registry = Arc::new(WorkerRegistry::new(WorkerRegistryConfig::default()).unwrap());
        let bytes = vec![137, 80, 78, 71, 13, 10, 26, 10];
        registry
            .register(
                Arc::new(ObservationWorker {
                    environment: EnvironmentDescriptor::local_user_desktop(),
                    bytes: bytes.clone(),
                }),
                1,
            )
            .await
            .unwrap();
        let lease = registry
            .acquire(&WorkerRequirements::default(), Duration::from_secs(60))
            .await
            .unwrap();
        let token = AuthorizationToken::new("g".repeat(64));
        let gateway = WorkerGateway::new(
            registry,
            &token,
            WorkerGatewayConfig {
                max_artifact_bytes: 1024,
                ..WorkerGatewayConfig::default()
            },
        )
        .unwrap();
        let response = gateway
            .execute(GatewayExecuteRequest {
                protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
                request_id: RequestId::new("request_observe"),
                timeout_ms: 1_000,
                authorization: token.clone(),
                lease_id: lease.lease_id.clone(),
                worker_epoch: lease.worker_epoch.clone(),
                command: Command::ObserveWindow(ObserveWindowInput {
                    session_id: SessionId::new("session_fixture"),
                    window_ref: WindowRef::new("window_fixture"),
                    include_screenshot: true,
                    accessibility: AccessibilityMode::Disabled,
                }),
            })
            .await;
        let serialized = serde_json::to_string(&response).unwrap();
        assert!(!serialized.contains("path-must-not-cross"));
        let GatewayOutcome::Success {
            result: GatewayResult::WindowObserved(observation),
        } = response.outcome
        else {
            panic!("unexpected gateway result");
        };
        let screenshot = observation.screenshot.unwrap();
        let artifact = gateway
            .fetch_artifact(GatewayArtifactRequest {
                protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
                authorization: token,
                lease_id: lease.lease_id,
                worker_epoch: lease.worker_epoch,
                ticket_ref: screenshot.ticket_ref,
            })
            .await
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(artifact.data_base64)
                .unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn authentication_fails_before_worker_resolution() {
        let registry = Arc::new(WorkerRegistry::new(WorkerRegistryConfig::default()).unwrap());
        let token = AuthorizationToken::new("g".repeat(64));
        let gateway = WorkerGateway::new(registry, &token, WorkerGatewayConfig::default()).unwrap();
        let response = gateway
            .execute(GatewayExecuteRequest {
                protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
                request_id: RequestId::new("request_denied"),
                timeout_ms: 1_000,
                authorization: AuthorizationToken::new("x".repeat(64)),
                lease_id: WorkerLeaseId::random("lease"),
                worker_epoch: WorkerEpoch::random("epoch"),
                command: Command::GetCapabilities,
            })
            .await;
        let GatewayOutcome::Error { error } = response.outcome else {
            panic!("unauthorized request succeeded");
        };
        assert_eq!(error.code, ErrorCode::Unauthorized);
        assert!(response.worker_id.is_none());
    }

    #[tokio::test]
    async fn reconciliation_projects_the_selected_worker_epoch() {
        let registry = Arc::new(WorkerRegistry::new(WorkerRegistryConfig::default()).unwrap());
        registry
            .register(
                Arc::new(ReconciliationWorker {
                    environment: EnvironmentDescriptor::local_user_desktop(),
                }),
                1,
            )
            .await
            .unwrap();
        let lease = registry
            .acquire(&WorkerRequirements::default(), Duration::from_secs(60))
            .await
            .unwrap();
        let token = AuthorizationToken::new("g".repeat(64));
        let gateway = WorkerGateway::new(registry, &token, WorkerGatewayConfig::default()).unwrap();
        let response = gateway
            .execute(GatewayExecuteRequest {
                protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
                request_id: RequestId::new("request_reconcile"),
                timeout_ms: 1_000,
                authorization: token,
                lease_id: lease.lease_id,
                worker_epoch: lease.worker_epoch.clone(),
                command: Command::ReconcileMutation(ReconcileMutationInput {
                    request_id: RequestId::new("request_original_mutation"),
                    wait_ms: 100,
                }),
            })
            .await;
        let GatewayOutcome::Success {
            result: GatewayResult::Core(CommandResult::MutationReconciled(reconciliation)),
        } = response.outcome
        else {
            panic!("unexpected reconciliation result");
        };
        assert_eq!(
            reconciliation.worker_epoch.as_deref(),
            Some(lease.worker_epoch.as_str())
        );
    }
}
