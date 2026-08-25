//! Runtime authority, observation, and artifact lifecycle contracts.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, Action, ActionKind, CapabilityManifest, CaptureMode, Command, CommandResult,
    DeliveryMode, DriverCapabilities, ErrorCode, InputRoute, ListWindowsInput, ObserveWindowInput,
    OpenSessionInput, PermissionMode, PermissionState, PermissionStatus, Platform, ScreenRect,
    ScreenshotPoint, SessionInput,
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
}

impl MockDriver {
    fn new() -> Self {
        Self {
            revision: Mutex::new(1),
            stale_observations_remaining: Mutex::new(0),
            observation_calls: AtomicUsize::new(0),
        }
    }

    fn stale_once() -> Self {
        Self {
            revision: Mutex::new(1),
            stale_observations_remaining: Mutex::new(1),
            observation_calls: AtomicUsize::new(0),
        }
    }

    fn application() -> DriverApplication {
        DriverApplication {
            key: "app-key".to_owned(),
            name: "Fixture".to_owned(),
            application_id: "dev.nexus.fixture".to_owned(),
            foreground: true,
        }
    }

    fn window() -> DriverWindow {
        DriverWindow {
            key: "window-key".to_owned(),
            application: Self::application(),
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
        _window: &DriverWindow,
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
            window_key: "window-key".to_owned(),
            window_screen_bounds: Self::window().screen_bounds,
            screenshot: include_screenshot.then(|| RgbaImage {
                width: 2,
                height: 2,
                pixels: vec![255; 16],
            }),
            screenshot_screen_bounds: include_screenshot.then(|| Self::window().screen_bounds),
            elements: (accessibility != AccessibilityMode::Disabled)
                .then(|| DriverElement {
                    key: "button-key".to_owned(),
                    parent_key: None,
                    role: "button".to_owned(),
                    name: "Continue".to_owned(),
                    value: None,
                    screen_bounds: Some(Self::window().screen_bounds),
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

fn manifest(mode: PermissionMode) -> CapabilityManifest {
    CapabilityManifest {
        mode,
        allowed_application_ids: vec!["dev.nexus.fixture".to_owned()],
        allowed_actions: if mode == PermissionMode::Bounded {
            vec![ActionKind::InvokeElement, ActionKind::ClickPoint]
        } else {
            Vec::new()
        },
        allow_foreground_input: mode == PermissionMode::Bounded,
        allow_desktop_capture: false,
        ttl_seconds: 300,
    }
}

fn runtime() -> (Runtime, PathBuf) {
    let root = artifact_root();
    let runtime = Runtime::new(Arc::new(MockDriver::new()), RuntimeConfig::new(&root))
        .expect("create runtime");
    (runtime, root)
}

async fn open_session(runtime: &Runtime, mode: PermissionMode) -> nexus_cua_protocol::SessionId {
    match runtime
        .execute(Command::OpenSession(OpenSessionInput {
            manifest: manifest(mode),
        }))
        .await
        .expect("open session")
    {
        CommandResult::SessionOpened(output) => output.session_id,
        _ => panic!("unexpected open-session result"),
    }
}

#[tokio::test]
async fn read_only_manifest_cannot_smuggle_mutation_authority() {
    let (runtime, root) = runtime();
    let mut invalid = manifest(PermissionMode::ReadOnly);
    invalid.allowed_actions.push(ActionKind::InvokeElement);
    let error = runtime
        .execute(Command::OpenSession(OpenSessionInput { manifest: invalid }))
        .await
        .expect_err("read-only action authority must fail");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
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
    let _ = std::fs::remove_dir_all(root);
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

    for _ in 0..3 {
        runtime
            .execute(Command::ObserveWindow(ObserveWindowInput {
                session_id: session_id.clone(),
                window_ref: window_ref.clone(),
                include_screenshot: true,
                accessibility: AccessibilityMode::Disabled,
            }))
            .await
            .expect("observe window");
    }

    let artifact_count = std::fs::read_dir(root.join(session_id.as_str()))
        .expect("read session artifacts")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|value| value == "png"))
        .count();
    assert_eq!(artifact_count, 2);
    let _ = std::fs::remove_dir_all(root);
}
