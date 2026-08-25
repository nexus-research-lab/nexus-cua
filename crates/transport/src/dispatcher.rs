//! Authenticated command dispatch with in-process idempotency.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nexus_cua_protocol::{
    AuthorizationToken, Command, CuaError, ErrorCode, PROTOCOL_VERSION, RequestEnvelope, RequestId,
    ResponseEnvelope, ResponseOutcome,
};
use nexus_cua_runtime::Runtime;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, watch};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

use crate::TransportError;
use crate::auth::TokenVerifier;

const MAX_REQUEST_ID_BYTES: usize = 128;

/// Authenticates, deduplicates, and executes protocol requests.
pub struct Dispatcher {
    runtime: Arc<Runtime>,
    token: TokenVerifier,
    ledger: Arc<Mutex<RequestLedger>>,
    max_request_timeout_ms: u32,
}

impl Dispatcher {
    /// Creates a dispatcher. Tokens outside the supported bounded length are rejected.
    ///
    /// # Errors
    ///
    /// Returns an error when an authority or resource bound is invalid.
    pub fn new(
        runtime: Arc<Runtime>,
        token: &AuthorizationToken,
        max_inflight_requests: usize,
        max_completed_requests: usize,
        completed_request_ttl: Duration,
        max_request_timeout_ms: u32,
    ) -> Result<Self, TransportError> {
        if max_inflight_requests == 0
            || max_completed_requests == 0
            || completed_request_ttl.is_zero()
            || max_request_timeout_ms == 0
        {
            return Err(TransportError::InvalidConfiguration(
                "request ledger and timeout bounds must be non-zero".to_owned(),
            ));
        }
        let token = TokenVerifier::new(token).ok_or_else(|| {
            TransportError::InvalidConfiguration(
                "authorization token must contain at least 32 bytes".to_owned(),
            )
        })?;
        Ok(Self {
            runtime,
            token,
            ledger: Arc::new(Mutex::new(RequestLedger {
                entries: HashMap::new(),
                inflight_requests: 0,
                max_inflight_requests,
                max_completed_requests,
                completed_request_ttl,
            })),
            max_request_timeout_ms,
        })
    }

    /// Dispatches one decoded request without logging secret command content.
    pub async fn dispatch(&self, request: RequestEnvelope) -> ResponseEnvelope {
        let started = Instant::now();
        let operation = operation_name(&request.command);
        let request_id_log = request_id_log_value(&request.request_id).to_owned();
        let response = self.dispatch_inner(request, operation).await;
        let (status, error_code) = response_status(&response);
        info!(
            request_id = request_id_log,
            operation,
            status,
            error_code,
            wait_elapsed_us = elapsed_micros(started),
            "local request completed"
        );
        response
    }

    async fn dispatch_inner(
        &self,
        request: RequestEnvelope,
        operation: &'static str,
    ) -> ResponseEnvelope {
        let request_id = request.request_id.clone();
        if request.protocol_version != PROTOCOL_VERSION {
            return failure(
                request_id,
                public_error(
                    ErrorCode::ProtocolMismatch,
                    "unsupported protocol version",
                    false,
                    Some("negotiate_protocol_version"),
                ),
            );
        }
        if !self.token.accepts(&request.authorization) {
            warn!(
                request_id = request_id_log_value(&request_id),
                "rejected unauthorized local request"
            );
            return failure(
                request_id,
                public_error(
                    ErrorCode::Unauthorized,
                    "invalid local transport authorization",
                    false,
                    None,
                ),
            );
        }
        if !valid_request_id(&request_id) {
            return failure(
                request_id,
                public_error(
                    ErrorCode::InvalidRequest,
                    "request_id is empty, too long, or not normalized",
                    false,
                    Some("use_fresh_request_id"),
                ),
            );
        }
        if request.timeout_ms == 0 || request.timeout_ms > self.max_request_timeout_ms {
            return failure(
                request_id,
                public_error(
                    ErrorCode::InvalidRequest,
                    "timeout_ms is outside the configured bound",
                    false,
                    Some("use_bounded_timeout"),
                ),
            );
        }

        let Some(digest) = command_digest(&request.command) else {
            return failure(
                request_id,
                public_error(
                    ErrorCode::Internal,
                    "failed to compute request identity",
                    false,
                    None,
                ),
            );
        };
        let deadline =
            Instant::now() + std::time::Duration::from_millis(u64::from(request.timeout_ms));
        match self.begin_request(&request_id, digest).await {
            BeginRequest::Execute(receiver) => {
                debug!(request_id = %request_id, operation, "dispatching local request");
                self.spawn_execution(request_id.clone(), digest, operation, request.command);
                wait_for_response(receiver, deadline)
                    .await
                    .unwrap_or_else(|| deadline_exceeded(request_id))
            }
            BeginRequest::Replay(response) => {
                debug!(request_id = %request_id, operation, "replayed idempotent response");
                response
            }
            BeginRequest::Wait(receiver) => wait_for_response(receiver, deadline)
                .await
                .unwrap_or_else(|| deadline_exceeded(request_id)),
            BeginRequest::Busy => failure(
                request_id,
                public_error(
                    ErrorCode::Busy,
                    "in-flight request capacity is exhausted",
                    true,
                    Some("retry_after_capacity"),
                ),
            ),
            BeginRequest::Conflict => failure(
                request_id,
                public_error(
                    ErrorCode::InvalidRequest,
                    "request_id was already used for a different command",
                    false,
                    Some("use_fresh_request_id"),
                ),
            ),
        }
    }

    async fn begin_request(&self, request_id: &RequestId, digest: [u8; 32]) -> BeginRequest {
        let mut ledger = self.ledger.lock().await;
        ledger.purge_expired(Instant::now());
        match ledger.entries.get(request_id) {
            Some(RequestEntry::Pending {
                digest: existing,
                response,
            }) if *existing == digest => BeginRequest::Wait(response.subscribe()),
            Some(RequestEntry::Complete {
                digest: existing,
                response,
                ..
            }) if *existing == digest => BeginRequest::Replay(response.clone()),
            Some(_) => BeginRequest::Conflict,
            None if ledger.inflight_requests >= ledger.max_inflight_requests => BeginRequest::Busy,
            None if ledger.entries.len() >= ledger.max_completed_requests => BeginRequest::Busy,
            None => {
                let (response, receiver) = watch::channel(None);
                ledger.entries.insert(
                    request_id.clone(),
                    RequestEntry::Pending { digest, response },
                );
                ledger.inflight_requests += 1;
                BeginRequest::Execute(receiver)
            }
        }
    }

    fn spawn_execution(
        &self,
        request_id: RequestId,
        digest: [u8; 32],
        operation: &'static str,
        command: Command,
    ) {
        let runtime = Arc::clone(&self.runtime);
        let ledger = Arc::clone(&self.ledger);
        tokio::spawn(async move {
            let started = Instant::now();
            let outcome = match runtime.execute(command).await {
                Ok(result) => ResponseOutcome::Success { result },
                Err(error) => ResponseOutcome::Error { error },
            };
            let response = ResponseEnvelope {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                request_id: request_id.clone(),
                outcome,
            };
            let (status, error_code) = response_status(&response);
            complete_request(&ledger, request_id.clone(), digest, response).await;
            info!(
                request_id = %request_id,
                operation,
                status,
                error_code,
                execution_elapsed_us = elapsed_micros(started),
                "local request execution recorded"
            );
        });
    }
}

enum BeginRequest {
    Execute(watch::Receiver<Option<ResponseEnvelope>>),
    Replay(ResponseEnvelope),
    Wait(watch::Receiver<Option<ResponseEnvelope>>),
    Busy,
    Conflict,
}

struct RequestLedger {
    entries: HashMap<RequestId, RequestEntry>,
    inflight_requests: usize,
    max_inflight_requests: usize,
    max_completed_requests: usize,
    completed_request_ttl: Duration,
}

impl RequestLedger {
    fn purge_expired(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            !matches!(entry, RequestEntry::Complete { expires_at, .. } if *expires_at <= now)
        });
    }
}

enum RequestEntry {
    Pending {
        digest: [u8; 32],
        response: watch::Sender<Option<ResponseEnvelope>>,
    },
    Complete {
        digest: [u8; 32],
        response: ResponseEnvelope,
        expires_at: Instant,
    },
}

async fn complete_request(
    ledger: &Mutex<RequestLedger>,
    request_id: RequestId,
    digest: [u8; 32],
    response: ResponseEnvelope,
) {
    let mut ledger = ledger.lock().await;
    if let Some(RequestEntry::Pending {
        response: sender, ..
    }) = ledger.entries.remove(&request_id)
    {
        ledger.inflight_requests = ledger.inflight_requests.saturating_sub(1);
        sender.send_replace(Some(response.clone()));
    }
    let expires_at = Instant::now() + ledger.completed_request_ttl;
    ledger.entries.insert(
        request_id,
        RequestEntry::Complete {
            digest,
            response,
            expires_at,
        },
    );
}

async fn wait_for_response(
    mut receiver: watch::Receiver<Option<ResponseEnvelope>>,
    deadline: Instant,
) -> Option<ResponseEnvelope> {
    if let Some(response) = receiver.borrow().clone() {
        return Some(response);
    }
    match tokio::time::timeout_at(deadline, receiver.changed()).await {
        Ok(Ok(())) => receiver.borrow().clone(),
        Ok(Err(_)) | Err(_) => None,
    }
}

fn deadline_exceeded(request_id: RequestId) -> ResponseEnvelope {
    failure(
        request_id,
        public_error(
            ErrorCode::DeadlineExceeded,
            "request deadline elapsed; retry with the same request_id to reconcile",
            true,
            Some("retry_same_request_id"),
        ),
    )
}

fn command_digest(command: &Command) -> Option<[u8; 32]> {
    let bytes = Zeroizing::new(serde_json::to_vec(command).ok()?);
    let mut digest = Sha256::new();
    digest.update(bytes.as_slice());
    Some(digest.finalize().into())
}

fn operation_name(command: &Command) -> &'static str {
    match command {
        Command::GetCapabilities => "get_capabilities",
        Command::GetPermissionStatus => "get_permission_status",
        Command::DiscoverApplications => "discover_applications",
        Command::OpenSession(_) => "open_session",
        Command::CloseSession(_) => "close_session",
        Command::ListApps(_) => "list_apps",
        Command::ListWindows(_) => "list_windows",
        Command::ObserveWindow(_) => "observe_window",
        Command::PerformAction(_) => "perform_action",
        Command::VerifyState(_) => "verify_state",
    }
}

fn valid_request_id(request_id: &RequestId) -> bool {
    let value = request_id.as_str();
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn request_id_log_value(request_id: &RequestId) -> &str {
    if valid_request_id(request_id) {
        request_id.as_str()
    } else {
        "invalid"
    }
}

fn response_status(response: &ResponseEnvelope) -> (&'static str, &'static str) {
    match &response.outcome {
        ResponseOutcome::Success { .. } => ("success", "none"),
        ResponseOutcome::Error { error } => ("error", error_code_name(error.code)),
    }
}

fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::ProtocolMismatch => "protocol_mismatch",
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::Busy => "busy",
        ErrorCode::DeadlineExceeded => "deadline_exceeded",
        ErrorCode::SessionUnavailable => "session_unavailable",
        ErrorCode::StaleDiscovery => "stale_discovery",
        ErrorCode::CapabilityDenied => "capability_denied",
        ErrorCode::ReferenceNotFound => "reference_not_found",
        ErrorCode::StaleObservation => "stale_observation",
        ErrorCode::PermissionRequired => "permission_required",
        ErrorCode::Unsupported => "unsupported",
        ErrorCode::ForegroundRequired => "foreground_required",
        ErrorCode::TargetUnavailable => "target_unavailable",
        ErrorCode::DriverFailure => "driver_failure",
        ErrorCode::Internal => "internal",
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

pub(crate) fn public_error(
    code: ErrorCode,
    message: &str,
    retryable: bool,
    recovery_action: Option<&str>,
) -> CuaError {
    CuaError {
        code,
        message: message.to_owned(),
        retryable,
        recovery_action: recovery_action.map(str::to_owned),
    }
}

pub(crate) fn failure(request_id: RequestId, error: CuaError) -> ResponseEnvelope {
    ResponseEnvelope {
        protocol_version: PROTOCOL_VERSION.to_owned(),
        request_id,
        outcome: ResponseOutcome::Error { error },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use nexus_cua_protocol::{
        AccessibilityMode, CaptureMode, DriverCapabilities, InputRoute, PermissionState,
        PermissionStatus, Platform, StatePredicate,
    };
    use nexus_cua_runtime::{
        DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverError,
        DriverErrorKind, DriverObservation, DriverVerification, DriverWindow, RuntimeConfig,
    };

    use super::*;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    struct CountingDriver {
        calls: AtomicUsize,
        delay: Duration,
    }

    #[async_trait]
    impl DesktopDriver for CountingDriver {
        async fn capabilities(&self) -> Result<DriverCapabilities, DriverError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(DriverCapabilities {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                runtime_version: "test".to_owned(),
                platform: Platform::Unsupported,
                capture_modes: vec![CaptureMode::Window],
                accessibility_tree: false,
                input_routes: vec![InputRoute::Foreground],
                actions: Vec::new(),
            })
        }

        async fn permission_status(&self) -> Result<PermissionStatus, DriverError> {
            Ok(PermissionStatus {
                screen_capture: PermissionState::Unknown,
                accessibility: PermissionState::Unknown,
                input_control: PermissionState::Unknown,
            })
        }

        async fn list_applications(&self) -> Result<Vec<DriverApplication>, DriverError> {
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
            Err(unexpected_call())
        }

        async fn observation_is_current(
            &self,
            _window: &DriverWindow,
            _fingerprint: &str,
        ) -> Result<bool, DriverError> {
            Err(unexpected_call())
        }

        async fn perform_action(
            &self,
            _window: &DriverWindow,
            _action: DriverAction,
            _allow_foreground: bool,
        ) -> Result<DriverActionOutput, DriverError> {
            Err(unexpected_call())
        }

        async fn verify_state(
            &self,
            _window: &DriverWindow,
            _predicate: &StatePredicate,
        ) -> Result<DriverVerification, DriverError> {
            Err(unexpected_call())
        }
    }

    #[tokio::test]
    async fn concurrent_identical_requests_execute_once() {
        let (dispatcher, driver, root) = dispatcher(Duration::from_millis(30));
        let request = request("request-concurrent", 500, Command::GetCapabilities);
        let (first, second) = tokio::join!(
            dispatcher.dispatch(request.clone()),
            dispatcher.dispatch(request)
        );

        assert!(matches!(first.outcome, ResponseOutcome::Success { .. }));
        assert!(matches!(second.outcome, ResponseOutcome::Success { .. }));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        cleanup(root);
    }

    #[tokio::test]
    async fn timed_out_request_can_reconcile_with_a_longer_wait() {
        let (dispatcher, driver, root) = dispatcher(Duration::from_millis(40));
        let first = dispatcher
            .dispatch(request("request-timeout", 5, Command::GetCapabilities))
            .await;
        assert_error_code(&first, ErrorCode::DeadlineExceeded);

        let reconciled = dispatcher
            .dispatch(request("request-timeout", 500, Command::GetCapabilities))
            .await;
        assert!(matches!(
            reconciled.outcome,
            ResponseOutcome::Success { .. }
        ));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        cleanup(root);
    }

    #[tokio::test]
    async fn reused_request_id_with_different_command_fails_closed() {
        let (dispatcher, driver, root) = dispatcher(Duration::from_millis(40));
        let first = dispatcher
            .dispatch(request("request-conflict", 5, Command::GetCapabilities))
            .await;
        let conflict = dispatcher
            .dispatch(request(
                "request-conflict",
                500,
                Command::GetPermissionStatus,
            ))
            .await;

        assert_error_code(&first, ErrorCode::DeadlineExceeded);
        assert_error_code(&conflict, ErrorCode::InvalidRequest);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        cleanup(root);
    }

    #[tokio::test]
    async fn request_identity_is_bounded_before_ledger_admission() {
        let (dispatcher, driver, root) = dispatcher(Duration::ZERO);
        let response = dispatcher
            .dispatch(request("invalid\nrequest", 500, Command::GetCapabilities))
            .await;

        assert_error_code(&response, ErrorCode::InvalidRequest);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
        cleanup(root);
    }

    #[tokio::test]
    async fn protocol_mismatch_returns_readable_current_envelope() {
        let (dispatcher, driver, root) = dispatcher(Duration::ZERO);
        let mut mismatched = request("request-protocol-mismatch", 500, Command::GetCapabilities);
        mismatched.protocol_version = "nexus.cua.v0".to_owned();

        let response = dispatcher.dispatch(mismatched).await;

        assert_eq!(response.protocol_version, PROTOCOL_VERSION);
        assert_error_code(&response, ErrorCode::ProtocolMismatch);
        assert_eq!(driver.calls.load(Ordering::SeqCst), 0);
        cleanup(root);
    }

    #[tokio::test]
    async fn distinct_inflight_requests_are_bounded_before_execution() {
        let (dispatcher, driver, root) =
            dispatcher_with_inflight_limit(Duration::from_millis(30), 1);
        let first_dispatcher = Arc::clone(&dispatcher);
        let first = tokio::spawn(async move {
            first_dispatcher
                .dispatch(request("request-first", 500, Command::GetCapabilities))
                .await
        });
        loop {
            let admitted = dispatcher
                .ledger
                .lock()
                .await
                .entries
                .values()
                .any(|entry| matches!(entry, RequestEntry::Pending { .. }));
            if admitted {
                break;
            }
            tokio::task::yield_now().await;
        }

        let rejected = dispatcher
            .dispatch(request("request-second", 500, Command::GetCapabilities))
            .await;
        assert_error_code(&rejected, ErrorCode::Busy);
        let completed = first.await.expect("join first request");
        assert!(matches!(completed.outcome, ResponseOutcome::Success { .. }));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 1);
        cleanup(root);
    }

    #[tokio::test(start_paused = true)]
    async fn unexpired_reconciliation_result_is_not_evicted_for_new_mutation() {
        let horizon = Duration::from_secs(10 * 60);
        let (dispatcher, driver, root) = dispatcher_with_limits(Duration::ZERO, 1, 1, horizon);
        let completed = dispatcher
            .dispatch(request("request-completed", 500, Command::GetCapabilities))
            .await;
        assert!(matches!(completed.outcome, ResponseOutcome::Success { .. }));

        let rejected = dispatcher
            .dispatch(request(
                "request-mutation",
                500,
                Command::CloseSession(nexus_cua_protocol::SessionInput {
                    session_id: nexus_cua_protocol::SessionId::new("session-old"),
                }),
            ))
            .await;
        assert_error_code(&rejected, ErrorCode::Busy);

        tokio::time::advance(horizon + Duration::from_secs(1)).await;
        let admitted = dispatcher
            .dispatch(request(
                "request-after-horizon",
                500,
                Command::GetCapabilities,
            ))
            .await;
        assert!(matches!(admitted.outcome, ResponseOutcome::Success { .. }));
        assert_eq!(driver.calls.load(Ordering::SeqCst), 2);
        cleanup(root);
    }

    fn dispatcher(delay: Duration) -> (Arc<Dispatcher>, Arc<CountingDriver>, PathBuf) {
        dispatcher_with_inflight_limit(delay, 64)
    }

    fn dispatcher_with_inflight_limit(
        delay: Duration,
        max_inflight_requests: usize,
    ) -> (Arc<Dispatcher>, Arc<CountingDriver>, PathBuf) {
        dispatcher_with_limits(
            delay,
            max_inflight_requests,
            64,
            Duration::from_secs(10 * 60),
        )
    }

    fn dispatcher_with_limits(
        delay: Duration,
        max_inflight_requests: usize,
        max_completed_requests: usize,
        completed_request_ttl: Duration,
    ) -> (Arc<Dispatcher>, Arc<CountingDriver>, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "nexus-cua-transport-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let driver = Arc::new(CountingDriver {
            calls: AtomicUsize::new(0),
            delay,
        });
        let runtime = Arc::new(
            Runtime::new(driver.clone(), RuntimeConfig::new(&root)).expect("create test runtime"),
        );
        let token = AuthorizationToken::new("0123456789abcdef0123456789abcdef");
        let dispatcher = Arc::new(
            Dispatcher::new(
                runtime,
                &token,
                max_inflight_requests,
                max_completed_requests,
                completed_request_ttl,
                1_000,
            )
            .expect("create test dispatcher"),
        );
        (dispatcher, driver, root)
    }

    fn request(request_id: &str, timeout_ms: u32, command: Command) -> RequestEnvelope {
        RequestEnvelope {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            request_id: RequestId::new(request_id),
            timeout_ms,
            authorization: AuthorizationToken::new("0123456789abcdef0123456789abcdef"),
            command,
        }
    }

    fn assert_error_code(response: &ResponseEnvelope, expected: ErrorCode) {
        let ResponseOutcome::Error { error } = &response.outcome else {
            panic!("expected error response");
        };
        assert_eq!(error.code, expected);
    }

    fn unexpected_call() -> DriverError {
        DriverError::new(
            DriverErrorKind::Platform,
            "unexpected test driver operation",
        )
    }

    fn cleanup(root: PathBuf) {
        let _ = std::fs::remove_dir_all(root);
    }
}
