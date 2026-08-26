//! Runtime authority, observation, and artifact lifecycle contracts.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, Action, ActionKind, ApplicationProvenance, CapabilityManifest, CaptureMode,
    Command, CommandResult, DeliveryMode, DiscoveryRef, DriverCapabilities, ErrorCode, InputRoute,
    ListWindowsInput, MutationStatus, ObserveWindowInput, OpenSessionInput, PermissionMode,
    PermissionState, PermissionStatus, Platform, ScreenRect, ScreenshotPoint, SessionInput,
};
use nexus_cua_runtime::{
    DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverElement, DriverError,
    DriverErrorKind, DriverObservation, DriverVerification, DriverWindow, RgbaImage, Runtime,
    RuntimeConfig,
};
use tokio::sync::Mutex;
use uuid::Uuid;

struct MockDriver {
    revision: Mutex<u64>,
    stale_observations_remaining: Mutex<u8>,
    observation_calls: AtomicUsize,
    application_generation: AtomicUsize,
    action_failure: Option<MutationStatus>,
}

impl MockDriver {
    fn new() -> Self {
        Self {
            revision: Mutex::new(1),
            stale_observations_remaining: Mutex::new(0),
            observation_calls: AtomicUsize::new(0),
            application_generation: AtomicUsize::new(1),
            action_failure: None,
        }
    }

    fn stale_once() -> Self {
        Self {
            revision: Mutex::new(1),
            stale_observations_remaining: Mutex::new(1),
            observation_calls: AtomicUsize::new(0),
            application_generation: AtomicUsize::new(1),
            action_failure: None,
        }
    }

    fn failing_action(status: MutationStatus) -> Self {
        Self {
            action_failure: Some(status),
            ..Self::new()
        }
    }

    fn application_for_generation(generation: usize) -> DriverApplication {
        DriverApplication {
            key: format!("app-key-{generation}"),
            process_generation: format!("launch-{generation}"),
            identity: "bundle:dev.nexus.fixture".to_owned(),
            name: "Fixture".to_owned(),
            application_id: "dev.nexus.fixture".to_owned(),
            foreground: true,
            provenance: ApplicationProvenance::Macos {
                bundle_id: Some("dev.nexus.fixture".to_owned()),
                executable_path: Some(
                    "/Applications/Fixture.app/Contents/MacOS/Fixture".to_owned(),
                ),
                signing_team_id: Some("NEXUSTEST".to_owned()),
                designated_requirement: None,
            },
        }
    }

    fn window_for_generation(generation: usize) -> DriverWindow {
        DriverWindow {
            key: format!("window-key-{generation}"),
            application: Self::application_for_generation(generation),
            title: "Fixture Window".to_owned(),
            screen_bounds: ScreenRect {
                x: 10.0,
                y: 20.0,
                width: 2.0,
                height: 2.0,
            },
            minimized: false,
            visible: true,
            foreground: true,
        }
    }

    fn restart_application(&self) {
        self.application_generation.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl DesktopDriver for MockDriver {
    async fn capabilities(&self) -> Result<DriverCapabilities, DriverError> {
        Ok(DriverCapabilities {
            protocol_version: "nexus.cua.v1".to_owned(),
            runtime_version: "test".to_owned(),
            platform: Platform::Macos,
            capture_modes: vec![CaptureMode::Window],
            accessibility_tree: true,
            input_routes: vec![InputRoute::Semantic, InputRoute::Foreground],
            actions: vec![ActionKind::InvokeElement, ActionKind::ClickPoint],
        })
    }

    async fn permission_status(&self) -> Result<PermissionStatus, DriverError> {
        Ok(PermissionStatus {
            screen_capture: PermissionState::Granted,
            accessibility: PermissionState::Granted,
            input_control: PermissionState::Granted,
        })
    }

    async fn list_applications(&self) -> Result<Vec<DriverApplication>, DriverError> {
        let generation = self.application_generation.load(Ordering::SeqCst);
        Ok(vec![Self::application_for_generation(generation)])
    }

    async fn list_windows(
        &self,
        _application_key: Option<&str>,
    ) -> Result<Vec<DriverWindow>, DriverError> {
        let generation = self.application_generation.load(Ordering::SeqCst);
        Ok(vec![Self::window_for_generation(generation)])
    }

    async fn observe_window(
        &self,
        window: &DriverWindow,
        include_screenshot: bool,
        accessibility: AccessibilityMode,
    ) -> Result<DriverObservation, DriverError> {
        self.observation_calls.fetch_add(1, Ordering::SeqCst);
        let mut stale_remaining = self.stale_observations_remaining.lock().await;
        if *stale_remaining > 0 {
            *stale_remaining -= 1;
            return Err(DriverError::new(
                DriverErrorKind::StaleObservation,
                "fixture geometry changed",
            ));
        }
        drop(stale_remaining);
        let revision = *self.revision.lock().await;
        Ok(DriverObservation {
            window_key: window.key.clone(),
            window_screen_bounds: window.screen_bounds,
            screenshot: include_screenshot.then(|| RgbaImage::new(2, 2, vec![255; 16])),
            screenshot_screen_bounds: include_screenshot.then_some(window.screen_bounds),
            elements: (accessibility != AccessibilityMode::Disabled)
                .then(|| DriverElement {
                    key: "button-key".to_owned(),
                    parent_key: None,
                    role: "button".to_owned(),
                    name: "Continue".to_owned(),
                    value: None,
                    screen_bounds: Some(window.screen_bounds),
                    enabled: true,
                    focused: false,
                    actions: vec!["press".to_owned()],
                })
                .into_iter()
                .collect(),
            elements_complete: true,
            elements_truncation: None,
            fingerprint: format!("revision-{revision}"),
        })
    }

    async fn observation_is_current(
        &self,
        _window: &DriverWindow,
        fingerprint: &str,
    ) -> Result<bool, DriverError> {
        let revision = *self.revision.lock().await;
        Ok(fingerprint == format!("revision-{revision}"))
    }

    async fn perform_action(
        &self,
        _window: &DriverWindow,
        action: DriverAction,
        allow_foreground: bool,
    ) -> Result<DriverActionOutput, DriverError> {
        if let Some(status) = self.action_failure {
            let error =
                DriverError::new(DriverErrorKind::TargetUnresponsive, "fixture action failed");
            return Err(match status {
                MutationStatus::NotDispatched => error.mutation_not_dispatched(),
                MutationStatus::Indeterminate => error.mutation_indeterminate(),
                MutationStatus::NotApplicable => error,
            });
        }
        let delivery_mode = match action {
            DriverAction::FocusElement { .. }
            | DriverAction::InvokeElement { .. }
            | DriverAction::ToggleElement { .. }
            | DriverAction::SelectElement { .. }
            | DriverAction::SetExpanded { .. } => DeliveryMode::Semantic,
            _ if allow_foreground => DeliveryMode::Foreground,
            _ => panic!("test runtime must reject foreground action before driver dispatch"),
        };
        *self.revision.lock().await += 1;
        Ok(DriverActionOutput { delivery_mode })
    }

    async fn verify_state(
        &self,
        _window: &DriverWindow,
        _predicate: &nexus_cua_protocol::StatePredicate,
    ) -> Result<DriverVerification, DriverError> {
        Ok(DriverVerification {
            matched: true,
            evidence: "fixture matched".to_owned(),
        })
    }
}

fn artifact_root() -> PathBuf {
    std::env::temp_dir().join(format!("nexus-cua-test-{}", Uuid::new_v4()))
}

fn manifest(mode: PermissionMode, discovery_ref: DiscoveryRef) -> CapabilityManifest {
    CapabilityManifest {
        mode,
        application_refs: vec![discovery_ref],
        allowed_actions: if mode == PermissionMode::Bounded {
            vec![ActionKind::InvokeElement, ActionKind::ClickPoint]
        } else {
            Vec::new()
        },
        allow_foreground_input: mode == PermissionMode::Bounded,
        ttl_seconds: 300,
    }
}

fn runtime() -> (Runtime, PathBuf) {
    let root = artifact_root();
    let runtime = Runtime::new(Arc::new(MockDriver::new()), RuntimeConfig::new(&root))
        .expect("create runtime");
    (runtime, root)
}

#[test]
fn runtime_rejects_zero_resource_bounds() {
    let root = artifact_root();
    let mut config = RuntimeConfig::new(&root);
    config.max_observations_per_session = 0;
    let error = Runtime::new(Arc::new(MockDriver::new()), config)
        .err()
        .expect("zero resource bound must fail");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let _ = std::fs::remove_dir_all(root);
}

async fn open_session(runtime: &Runtime, mode: PermissionMode) -> nexus_cua_protocol::SessionId {
    open_session_with_ttl(runtime, mode, 300).await
}

async fn open_session_with_ttl(
    runtime: &Runtime,
    mode: PermissionMode,
    ttl_seconds: u32,
) -> nexus_cua_protocol::SessionId {
    let discovery_ref = discover_application(runtime).await;
    let mut capability_manifest = manifest(mode, discovery_ref);
    capability_manifest.ttl_seconds = ttl_seconds;
    match runtime
        .execute(Command::OpenSession(OpenSessionInput {
            manifest: capability_manifest,
        }))
        .await
        .expect("open session")
    {
        CommandResult::SessionOpened(output) => output.session_id,
        _ => panic!("unexpected open-session result"),
    }
}

async fn discover_application(runtime: &Runtime) -> DiscoveryRef {
    let result = runtime
        .execute(Command::DiscoverApplications)
        .await
        .expect("discover applications");
    let CommandResult::ApplicationsDiscovered(output) = result else {
        panic!("unexpected discovery result");
    };
    output.applications[0].discovery_ref.clone()
}

#[tokio::test]
async fn read_only_manifest_cannot_smuggle_mutation_authority() {
    let (runtime, root) = runtime();
    let discovery_ref = discover_application(&runtime).await;
    let mut invalid = manifest(PermissionMode::ReadOnly, discovery_ref);
    invalid.allowed_actions.push(ActionKind::InvokeElement);
    let error = runtime
        .execute(Command::OpenSession(OpenSessionInput { manifest: invalid }))
        .await
        .expect_err("read-only action authority must fail");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn active_session_capacity_is_bounded() {
    let root = artifact_root();
    let mut config = RuntimeConfig::new(&root);
    config.max_active_sessions = 1;
    let runtime = Runtime::new(Arc::new(MockDriver::new()), config).expect("create runtime");
    let first = open_session(&runtime, PermissionMode::ReadOnly).await;
    let discovery_ref = discover_application(&runtime).await;

    let error = runtime
        .execute(Command::OpenSession(OpenSessionInput {
            manifest: manifest(PermissionMode::ReadOnly, discovery_ref),
        }))
        .await
        .expect_err("second live session must exceed capacity");
    assert_eq!(error.code, ErrorCode::Busy);

    runtime
        .execute(Command::CloseSession(SessionInput { session_id: first }))
        .await
        .expect("close first session");
    open_session(&runtime, PermissionMode::ReadOnly).await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn application_allowlist_has_an_aggregate_memory_bound() {
    let (runtime, root) = runtime();
    let mut invalid = manifest(
        PermissionMode::ReadOnly,
        DiscoveryRef::new("a".repeat(32 * 1024 + 1)),
    );
    invalid
        .application_refs
        .push(DiscoveryRef::new("discovery-2"));

    let error = runtime
        .execute(Command::OpenSession(OpenSessionInput { manifest: invalid }))
        .await
        .expect_err("oversized application identity must fail");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn open_session_rejects_restarted_discovery_process() {
    let root = artifact_root();
    let driver = Arc::new(MockDriver::new());
    let runtime = Runtime::new(driver.clone(), RuntimeConfig::new(&root)).expect("create runtime");
    let discovery_ref = discover_application(&runtime).await;
    driver.restart_application();

    let error = runtime
        .execute(Command::OpenSession(OpenSessionInput {
            manifest: manifest(PermissionMode::ReadOnly, discovery_ref),
        }))
        .await
        .expect_err("restarted process must make discovery stale");

    assert_eq!(error.code, ErrorCode::StaleDiscovery);
    assert_eq!(
        error.recovery_action.as_deref(),
        Some("discover_applications")
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(start_paused = true)]
async fn discovery_reference_expires_on_monotonic_deadline() {
    let root = artifact_root();
    let mut config = RuntimeConfig::new(&root);
    config.discovery_ttl = std::time::Duration::from_secs(5);
    let runtime = Runtime::new(Arc::new(MockDriver::new()), config).expect("create runtime");
    let discovery_ref = discover_application(&runtime).await;

    tokio::time::advance(std::time::Duration::from_secs(6)).await;
    tokio::task::yield_now().await;
    let error = runtime
        .execute(Command::OpenSession(OpenSessionInput {
            manifest: manifest(PermissionMode::ReadOnly, discovery_ref),
        }))
        .await
        .expect_err("expired discovery must fail");

    assert_eq!(error.code, ErrorCode::StaleDiscovery);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn discovery_reference_is_runtime_local() {
    let first_root = artifact_root();
    let second_root = artifact_root();
    let first = Runtime::new(Arc::new(MockDriver::new()), RuntimeConfig::new(&first_root))
        .expect("create first runtime");
    let second = Runtime::new(
        Arc::new(MockDriver::new()),
        RuntimeConfig::new(&second_root),
    )
    .expect("create second runtime");
    let discovery_ref = discover_application(&first).await;

    let error = second
        .execute(Command::OpenSession(OpenSessionInput {
            manifest: manifest(PermissionMode::ReadOnly, discovery_ref),
        }))
        .await
        .expect_err("foreign discovery reference must fail");

    assert_eq!(error.code, ErrorCode::StaleDiscovery);
    let _ = std::fs::remove_dir_all(first_root);
    let _ = std::fs::remove_dir_all(second_root);
}

#[tokio::test(start_paused = true)]
async fn idle_deadline_reaps_session_and_artifact_without_another_request() {
    let (runtime, root) = runtime();
    let session_id = open_session_with_ttl(&runtime, PermissionMode::ReadOnly, 1).await;
    let windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: None,
        }))
        .await
        .expect("list windows");
    let CommandResult::Windows(windows) = windows else {
        panic!("unexpected windows result");
    };
    let observed = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id: session_id.clone(),
            window_ref: windows[0].window_ref.clone(),
            include_screenshot: true,
            accessibility: AccessibilityMode::Disabled,
        }))
        .await
        .expect("observe window");
    let CommandResult::WindowObserved(observation) = observed else {
        panic!("unexpected observation result");
    };
    let artifact_path = observation.screenshot.expect("screenshot").path;
    assert!(PathBuf::from(&artifact_path).is_file());

    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    assert!(!PathBuf::from(&artifact_path).exists());

    let error = runtime
        .execute(Command::ListApps(SessionInput { session_id }))
        .await
        .expect_err("expired session must reject access");
    assert_eq!(error.code, ErrorCode::SessionUnavailable);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn graceful_shutdown_removes_sessions_and_rejects_new_commands() {
    let (runtime, root) = runtime();
    let session_id = open_session(&runtime, PermissionMode::ReadOnly).await;

    runtime.shutdown().await;

    let error = runtime
        .execute(Command::ListApps(SessionInput { session_id }))
        .await
        .expect_err("shutdown runtime must reject commands");
    assert_eq!(error.code, ErrorCode::SessionUnavailable);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn opaque_refs_observation_and_artifact_follow_session_lifecycle() {
    let (runtime, root) = runtime();
    let session_id = open_session(&runtime, PermissionMode::ReadOnly).await;
    let apps = runtime
        .execute(Command::ListApps(SessionInput {
            session_id: session_id.clone(),
        }))
        .await
        .expect("list apps");
    let app_ref = match apps {
        CommandResult::Apps(apps) => apps[0].app_ref.clone(),
        _ => panic!("unexpected apps result"),
    };
    let windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: Some(app_ref),
        }))
        .await
        .expect("list windows");
    let window_ref = match windows {
        CommandResult::Windows(windows) => windows[0].window_ref.clone(),
        _ => panic!("unexpected windows result"),
    };
    let observation = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id: session_id.clone(),
            window_ref,
            include_screenshot: true,
            accessibility: AccessibilityMode::Interactive,
        }))
        .await
        .expect("observe window");
    let artifact_path = match observation {
        CommandResult::WindowObserved(observation) => {
            assert_eq!(observation.elements.len(), 1);
            observation.screenshot.expect("screenshot artifact").path
        }
        _ => panic!("unexpected observation result"),
    };
    assert!(PathBuf::from(&artifact_path).is_file());

    runtime
        .execute(Command::CloseSession(SessionInput { session_id }))
        .await
        .expect("close session");
    assert!(!PathBuf::from(artifact_path).exists());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn successful_mutation_invalidates_its_observation() {
    let (runtime, root) = runtime();
    let session_id = open_session(&runtime, PermissionMode::Bounded).await;
    let windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: None,
        }))
        .await
        .expect("list windows");
    let window_ref = match windows {
        CommandResult::Windows(windows) => windows[0].window_ref.clone(),
        _ => panic!("unexpected windows result"),
    };
    let observed = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id: session_id.clone(),
            window_ref: window_ref.clone(),
            include_screenshot: false,
            accessibility: AccessibilityMode::Interactive,
        }))
        .await
        .expect("observe window");
    let (observation_id, element_ref) = match observed {
        CommandResult::WindowObserved(observation) => (
            observation.observation_id,
            observation.elements[0].element_ref.clone(),
        ),
        _ => panic!("unexpected observation result"),
    };
    let input = nexus_cua_protocol::PerformActionInput {
        session_id,
        window_ref,
        observation_id,
        action: Action::InvokeElement { element_ref },
    };
    let first = runtime
        .execute(Command::PerformAction(input.clone()))
        .await
        .expect("first action");
    assert!(matches!(first, CommandResult::ActionPerformed(_)));
    let second = runtime
        .execute(Command::PerformAction(input))
        .await
        .expect_err("reusing observation must fail");
    assert_eq!(second.code, ErrorCode::StaleObservation);
    assert_eq!(second.mutation_status, MutationStatus::NotDispatched);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn admitted_driver_failure_defaults_to_indeterminate() {
    let root = artifact_root();
    let runtime = Runtime::new(
        Arc::new(MockDriver::failing_action(MutationStatus::NotApplicable)),
        RuntimeConfig::new(&root),
    )
    .expect("create runtime");
    let input = fixture_invoke_input(&runtime).await;

    let error = runtime
        .execute(Command::PerformAction(input))
        .await
        .expect_err("admitted action must expose its uncertain disposition");

    assert_eq!(error.code, ErrorCode::TargetUnresponsive);
    assert_eq!(error.mutation_status, MutationStatus::Indeterminate);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn driver_preflight_failure_preserves_not_dispatched() {
    let root = artifact_root();
    let runtime = Runtime::new(
        Arc::new(MockDriver::failing_action(MutationStatus::NotDispatched)),
        RuntimeConfig::new(&root),
    )
    .expect("create runtime");
    let input = fixture_invoke_input(&runtime).await;

    let error = runtime
        .execute(Command::PerformAction(input))
        .await
        .expect_err("driver preflight must fail before dispatch");

    assert_eq!(error.code, ErrorCode::TargetUnresponsive);
    assert_eq!(error.mutation_status, MutationStatus::NotDispatched);
    let _ = std::fs::remove_dir_all(root);
}

async fn fixture_invoke_input(runtime: &Runtime) -> nexus_cua_protocol::PerformActionInput {
    let session_id = open_session(runtime, PermissionMode::Bounded).await;
    let windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: None,
        }))
        .await
        .expect("list windows");
    let CommandResult::Windows(windows) = windows else {
        panic!("unexpected windows result");
    };
    let window_ref = windows[0].window_ref.clone();
    let observed = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id: session_id.clone(),
            window_ref: window_ref.clone(),
            include_screenshot: false,
            accessibility: AccessibilityMode::Interactive,
        }))
        .await
        .expect("observe window");
    let CommandResult::WindowObserved(observation) = observed else {
        panic!("unexpected observation result");
    };
    nexus_cua_protocol::PerformActionInput {
        session_id,
        window_ref,
        observation_id: observation.observation_id,
        action: Action::InvokeElement {
            element_ref: observation.elements[0].element_ref.clone(),
        },
    }
}

#[tokio::test]
async fn coordinates_outside_observed_window_fail_before_dispatch() {
    let (runtime, root) = runtime();
    let session_id = open_session(&runtime, PermissionMode::Bounded).await;
    let windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: None,
        }))
        .await
        .expect("list windows");
    let window_ref = match windows {
        CommandResult::Windows(windows) => windows[0].window_ref.clone(),
        _ => panic!("unexpected windows result"),
    };
    let observed = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id: session_id.clone(),
            window_ref: window_ref.clone(),
            include_screenshot: true,
            accessibility: AccessibilityMode::Disabled,
        }))
        .await
        .expect("observe window");
    let observation_id = match observed {
        CommandResult::WindowObserved(observation) => observation.observation_id,
        _ => panic!("unexpected observation result"),
    };
    let error = runtime
        .execute(Command::PerformAction(
            nexus_cua_protocol::PerformActionInput {
                session_id,
                window_ref,
                observation_id,
                action: Action::ClickPoint {
                    point: ScreenshotPoint { x: 99, y: 99 },
                    button: nexus_cua_protocol::PointerButton::Left,
                    count: 1,
                },
            },
        ))
        .await
        .expect_err("out-of-bounds click must fail");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!(error.mutation_status, MutationStatus::NotDispatched);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn observation_retries_one_coherence_race() {
    let root = artifact_root();
    let driver = Arc::new(MockDriver::stale_once());
    let runtime = Runtime::new(driver.clone(), RuntimeConfig::new(&root)).expect("create runtime");
    let session_id = open_session(&runtime, PermissionMode::ReadOnly).await;
    let windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: None,
        }))
        .await
        .expect("list windows");
    let window_ref = match windows {
        CommandResult::Windows(windows) => windows[0].window_ref.clone(),
        _ => panic!("unexpected windows result"),
    };

    let observed = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id,
            window_ref,
            include_screenshot: false,
            accessibility: AccessibilityMode::Interactive,
        }))
        .await
        .expect("coherence retry succeeds");

    assert!(matches!(observed, CommandResult::WindowObserved(_)));
    assert_eq!(driver.observation_calls.load(Ordering::SeqCst), 2);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn screenshot_retention_is_bounded_per_session() {
    let root = artifact_root();
    let mut config = RuntimeConfig::new(&root);
    config.max_artifacts_per_session = 2;
    let runtime = Runtime::new(Arc::new(MockDriver::new()), config).expect("create runtime");
    let session_id = open_session(&runtime, PermissionMode::ReadOnly).await;
    let windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: None,
        }))
        .await
        .expect("list windows");
    let window_ref = match windows {
        CommandResult::Windows(windows) => windows[0].window_ref.clone(),
        _ => panic!("unexpected windows result"),
    };

    let mut paths = Vec::new();
    for _ in 0..3 {
        let observed = runtime
            .execute(Command::ObserveWindow(ObserveWindowInput {
                session_id: session_id.clone(),
                window_ref: window_ref.clone(),
                include_screenshot: true,
                accessibility: AccessibilityMode::Disabled,
            }))
            .await
            .expect("observe window");
        let CommandResult::WindowObserved(observation) = observed else {
            panic!("unexpected observation result");
        };
        paths.push(observation.screenshot.expect("screenshot artifact").path);
    }

    assert!(!PathBuf::from(&paths[0]).exists());
    assert!(paths[1..].iter().all(|path| PathBuf::from(path).is_file()));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dropping_runtime_removes_only_its_artifact_generation() {
    let root = artifact_root();
    std::fs::create_dir_all(&root).expect("create host artifact root");
    let sentinel = root.join("host-owned-sentinel");
    std::fs::write(&sentinel, b"keep").expect("write host sentinel");

    let runtime = Runtime::new(Arc::new(MockDriver::new()), RuntimeConfig::new(&root))
        .expect("create runtime");
    assert_eq!(
        std::fs::read_dir(&root)
            .expect("read artifact root")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("runtime_"))
            .count(),
        1
    );
    drop(runtime);

    assert!(sentinel.is_file());
    assert_eq!(
        std::fs::read_dir(&root)
            .expect("read artifact root")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("runtime_"))
            .count(),
        0
    );
    let _ = std::fs::remove_dir_all(root);
}
