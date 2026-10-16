//! In-process worker endpoint backed by the existing idempotent dispatcher.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexus_cua_protocol::{
    AuthorizationToken, Command, CuaError, PROTOCOL_VERSION, Platform, RequestEnvelope, RequestId,
    ResponseEnvelope, ScreenshotArtifact,
};
use nexus_cua_runtime::Runtime;
use nexus_cua_transport::{Dispatcher, TransportError};
use uuid::Uuid;

use crate::{DesktopWorker, EnvironmentDescriptor};

/// Idempotency and artifact bounds for one embedded desktop worker.
#[derive(Clone, Copy, Debug)]
pub struct LocalRuntimeWorkerConfig {
    /// Maximum distinct requests executing at once.
    pub max_inflight_requests: usize,
    /// Maximum retained completed request results.
    pub max_completed_requests: usize,
    /// Same-request reconciliation horizon.
    pub completed_request_ttl: Duration,
    /// Maximum caller wait deadline.
    pub max_request_timeout_ms: u32,
    /// Maximum screenshot artifact returned to a remote gateway.
    pub max_artifact_bytes: u64,
}

impl Default for LocalRuntimeWorkerConfig {
    fn default() -> Self {
        Self {
            max_inflight_requests: 64,
            max_completed_requests: 1_024,
            completed_request_ttl: Duration::from_secs(10 * 60),
            max_request_timeout_ms: 120_000,
            max_artifact_bytes: 16 * 1024 * 1024,
        }
    }
}

/// One local runtime projected as a desktop-worker endpoint. The internal
/// dispatcher token never leaves this object.
pub struct LocalRuntimeWorker {
    runtime: Arc<Runtime>,
    dispatcher: Dispatcher,
    token: AuthorizationToken,
    platform: Platform,
    environment: EnvironmentDescriptor,
    max_artifact_bytes: u64,
}

impl LocalRuntimeWorker {
    /// Wraps one runtime with the same bounded idempotency ledger used by local
    /// IPC before it can be registered for remote scheduling.
    ///
    /// # Errors
    ///
    /// Returns an error when dispatcher or artifact bounds are invalid.
    pub fn new(
        runtime: Arc<Runtime>,
        platform: Platform,
        environment: EnvironmentDescriptor,
        config: LocalRuntimeWorkerConfig,
    ) -> Result<Self, TransportError> {
        if config.max_artifact_bytes == 0 {
            return Err(TransportError::InvalidConfiguration(
                "worker artifact bound must be non-zero".to_owned(),
            ));
        }
        let token = AuthorizationToken::new(format!(
            "{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        ));
        let dispatcher = Dispatcher::new(
            Arc::clone(&runtime),
            &token,
            config.max_inflight_requests,
            config.max_completed_requests,
            config.completed_request_ttl,
            config.max_request_timeout_ms,
        )?;
        Ok(Self {
            runtime,
            dispatcher,
            token,
            platform,
            environment,
            max_artifact_bytes: config.max_artifact_bytes,
        })
    }
}

#[async_trait]
impl DesktopWorker for LocalRuntimeWorker {
    fn platform(&self) -> Platform {
        self.platform
    }

    fn environment(&self) -> &EnvironmentDescriptor {
        &self.environment
    }

    async fn dispatch(
        &self,
        request_id: RequestId,
        timeout_ms: u32,
        command: Command,
    ) -> ResponseEnvelope {
        self.dispatcher
            .dispatch(RequestEnvelope {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                request_id,
                timeout_ms,
                authorization: self.token.clone(),
                command,
            })
            .await
    }

    async fn read_artifact(
        &self,
        artifact: &ScreenshotArtifact,
        max_bytes: u64,
    ) -> Result<Vec<u8>, CuaError> {
        self.runtime
            .read_artifact(artifact, max_bytes.min(self.max_artifact_bytes))
            .await
    }
}
