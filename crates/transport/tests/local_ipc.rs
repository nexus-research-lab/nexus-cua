//! End-to-end local IPC smoke contract on each supported operating system.

#![cfg(any(unix, windows))]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, AuthorizationToken, CaptureMode, Command, DriverCapabilities, InputRoute,
    PROTOCOL_VERSION, PermissionState, PermissionStatus, Platform, RequestEnvelope, RequestId,
    ResponseOutcome, StatePredicate,
};
use nexus_cua_runtime::{
    DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverError,
    DriverObservation, DriverVerification, DriverWindow, Runtime, RuntimeConfig,
};
use nexus_cua_transport::{Dispatcher, LocalEndpoint, ServerConfig, request, serve_until};
use tokio::sync::oneshot;
use uuid::Uuid;

struct CapabilityDriver;

#[async_trait]
impl DesktopDriver for CapabilityDriver {
    async fn capabilities(&self) -> Result<DriverCapabilities, DriverError> {
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
        unreachable!("smoke test requests capabilities only")
    }

    async fn observation_is_current(
        &self,
        _window: &DriverWindow,
        _fingerprint: &str,
    ) -> Result<bool, DriverError> {
        unreachable!("smoke test requests capabilities only")
    }

    async fn perform_action(
        &self,
        _window: &DriverWindow,
        _action: DriverAction,
        _allow_foreground: bool,
    ) -> Result<DriverActionOutput, DriverError> {
        unreachable!("smoke test requests capabilities only")
    }

    async fn verify_state(
        &self,
        _window: &DriverWindow,
        _predicate: &StatePredicate,
    ) -> Result<DriverVerification, DriverError> {
        unreachable!("smoke test requests capabilities only")
    }
}

#[tokio::test]
async fn serves_one_authenticated_request_over_native_local_ipc() {
    let identity = Uuid::new_v4().simple().to_string();
    let root = std::env::temp_dir().join(format!("nexus-cua-ipc-{identity}"));
    std::fs::create_dir_all(&root).expect("create test root");
    let endpoint = test_endpoint(&root, &identity);
    let token = AuthorizationToken::new("0123456789abcdef0123456789abcdef");
    let runtime = Arc::new(
        Runtime::new(
            Arc::new(CapabilityDriver),
            RuntimeConfig::new(root.join("artifacts")),
        )
        .expect("create runtime"),
    );
    let config = ServerConfig::new(endpoint.clone());
    let dispatcher = Arc::new(
        Dispatcher::new(
            runtime,
            &token,
            config.max_inflight_requests,
            config.max_completed_requests,
            config.completed_request_ttl,
            config.max_request_timeout_ms,
        )
        .expect("create dispatcher"),
    );
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        serve_until(dispatcher, config, async {
            let _ = shutdown_rx.await;
        })
        .await
    });
    let envelope = RequestEnvelope {
        protocol_version: PROTOCOL_VERSION.to_owned(),
        request_id: RequestId::new(format!("request_{identity}")),
        timeout_ms: 1_000,
        authorization: token,
        command: Command::GetCapabilities,
    };

    let response = request_when_ready(&endpoint, &envelope)
        .await
        .expect("request native local endpoint");
    assert!(matches!(response.outcome, ResponseOutcome::Success { .. }));

    shutdown_tx.send(()).expect("signal server shutdown");
    server
        .await
        .expect("join local server")
        .expect("stop local server");
    #[cfg(unix)]
    assert!(!Path::new(endpoint.as_str()).exists());
    let _ = std::fs::remove_dir_all(root);
}

async fn request_when_ready(
    endpoint: &LocalEndpoint,
    envelope: &RequestEnvelope,
) -> Result<nexus_cua_protocol::ResponseEnvelope, String> {
    let mut last_error = "local endpoint did not start".to_owned();
    for _ in 0..100 {
        match request(endpoint, envelope, 1024 * 1024).await {
            Ok(response) => return Ok(response),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(last_error)
}

fn test_endpoint(_root: &Path, identity: &str) -> LocalEndpoint {
    #[cfg(windows)]
    let value = format!(r"\\.\pipe\nexus-cua-test-{identity}");
    #[cfg(unix)]
    let value = format!("/tmp/nxc-{}-{}.sock", std::process::id(), &identity[..8]);
    LocalEndpoint::new(value).expect("create local endpoint")
}
