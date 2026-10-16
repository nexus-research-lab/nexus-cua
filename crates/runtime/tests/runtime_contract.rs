//! Runtime authority, observation, and artifact lifecycle contracts.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, AccessibilityRangeValue, Action, ActionKind, ApplicationLifecycleOperation,
    ApplicationProvenance, CapabilityManifest, CaptureMode, ClipboardOperation, Command,
    CommandResult, DeliveryMode, DeliveryPreference, DiscoveryRef, DriverCapabilities, ErrorCode,
    ForegroundRestorationStatus, InputRoute, LaunchApplicationInput, ListWindowsInput,
    MutationStatus, ObserveDisplayInput, ObserveWindowInput, OpenSessionInput, PermissionMode,
    PermissionState, PermissionStatus, Platform, ReadClipboardInput, ScreenRect, ScreenshotPoint,
    SensitiveText, SessionInput, WriteClipboardInput,
};
use nexus_cua_runtime::{
    DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverClipboardSnapshot,
    DriverClipboardWriteOutput, DriverDisplay, DriverElement, DriverError, DriverErrorKind,
    DriverLaunchOutput, DriverLaunchableApplication, DriverObservation, DriverVerification,
    DriverWindow, PolicyActionRule, PolicyAllow, PolicyDeny, PolicyOperation, RgbaImage, Runtime,
    RuntimeConfig, RuntimePolicyLayer, RuntimePolicyStack,
};
use sha2::Digest;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

struct MockDriver {
    revision: Mutex<u64>,
    stale_observations_remaining: Mutex<u8>,
    observation_calls: AtomicUsize,
    application_generation: AtomicUsize,
    launches: AtomicUsize,
    action_failure: Option<MutationStatus>,
    restore_foreground_requested: AtomicBool,
    action_gate: Option<Arc<ActionGate>>,
    forced_restoration: Option<ForegroundRestorationStatus>,
    clipboard: Mutex<(u64, String)>,
}

#[derive(Default)]
struct ActionGate {
    entered: Notify,
    release: Notify,
}

impl MockDriver {
    fn new() -> Self {
        Self {
            revision: Mutex::new(1),
            stale_observations_remaining: Mutex::new(0),
            observation_calls: AtomicUsize::new(0),
            application_generation: AtomicUsize::new(1),
            launches: AtomicUsize::new(0),
            action_failure: None,
            restore_foreground_requested: AtomicBool::new(false),
            action_gate: None,
            forced_restoration: None,
            clipboard: Mutex::new((1, "fixture clipboard".to_owned())),
        }
    }

    fn stale_once() -> Self {
        Self {
            revision: Mutex::new(1),
            stale_observations_remaining: Mutex::new(1),
            observation_calls: AtomicUsize::new(0),
            application_generation: AtomicUsize::new(1),
            launches: AtomicUsize::new(0),
            action_failure: None,
            restore_foreground_requested: AtomicBool::new(false),
            action_gate: None,
            forced_restoration: None,
            clipboard: Mutex::new((1, "fixture clipboard".to_owned())),
        }
    }

    fn failing_action(status: MutationStatus) -> Self {
        Self {
            action_failure: Some(status),
            ..Self::new()
        }
    }

    fn gated_action(gate: Arc<ActionGate>) -> Self {
        Self {
            action_gate: Some(gate),
            ..Self::new()
        }
    }

    fn invalid_restoration() -> Self {
        Self {
            forced_restoration: Some(ForegroundRestorationStatus::Restored),
            ..Self::new()
        }
    }

    async fn replace_clipboard_externally(&self, text: &str) {
        let mut clipboard = self.clipboard.lock().await;
        clipboard.0 += 1;
        text.clone_into(&mut clipboard.1);
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

    fn launchable_application() -> DriverLaunchableApplication {
        DriverLaunchableApplication {
            key: "installed:dev.nexus.launch-fixture:v1".to_owned(),
            launch_target: "/Applications/Launch.app".to_owned(),
            identity: "bundle:dev.nexus.launch-fixture:executable:/Applications/Launch.app"
                .to_owned(),
            name: "Launch Fixture".to_owned(),
            application_id: "dev.nexus.launch-fixture".to_owned(),
            provenance: ApplicationProvenance::Macos {
                bundle_id: Some("dev.nexus.launch-fixture".to_owned()),
                executable_path: Some("/Applications/Launch.app/Contents/MacOS/Launch".to_owned()),
                signing_team_id: Some("NEXUSTEST".to_owned()),
                designated_requirement: None,
            },
        }
    }

    fn launched_application() -> DriverApplication {
        let launchable = Self::launchable_application();
        DriverApplication {
            key: "app-key-launched".to_owned(),
            process_generation: "launch-generation-1".to_owned(),
            identity: launchable.identity,
            name: launchable.name,
            application_id: launchable.application_id,
            foreground: false,
            provenance: launchable.provenance,
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
            capture_modes: vec![CaptureMode::Window, CaptureMode::Display],
            accessibility_tree: true,
            input_routes: vec![
                InputRoute::Semantic,
                InputRoute::System,
                InputRoute::Foreground,
            ],
            application_lifecycle: vec![ApplicationLifecycleOperation::Launch],
            clipboard: vec![ClipboardOperation::ReadText, ClipboardOperation::WriteText],
            actions: vec![
                ActionKind::TerminateApplication,
                ActionKind::InvokeElement,
                ActionKind::SetRangeValue,
                ActionKind::ClickPoint,
            ],
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
        let mut applications = vec![Self::application_for_generation(generation)];
        if self.launches.load(Ordering::SeqCst) > 0 {
            applications.push(Self::launched_application());
        }
        Ok(applications)
    }

    async fn list_launchable_applications(
        &self,
    ) -> Result<Vec<DriverLaunchableApplication>, DriverError> {
        Ok(vec![Self::launchable_application()])
    }

    async fn launch_application(
        &self,
        application: &DriverLaunchableApplication,
        allow_foreground_activation: bool,
    ) -> Result<DriverLaunchOutput, DriverError> {
        if !application.matches_identity(&Self::launchable_application()) {
            return Err(DriverError::new(
                DriverErrorKind::TargetUnavailable,
                "launch identity changed",
            )
            .mutation_not_dispatched());
        }
        self.launches.fetch_add(1, Ordering::SeqCst);
        let mut launched = Self::launched_application();
        launched.foreground = allow_foreground_activation;
        Ok(DriverLaunchOutput {
            application: launched,
            delivery_mode: if allow_foreground_activation {
                DeliveryMode::Foreground
            } else {
                DeliveryMode::System
            },
        })
    }

    async fn list_windows(
        &self,
        _application_key: Option<&str>,
    ) -> Result<Vec<DriverWindow>, DriverError> {
        let generation = self.application_generation.load(Ordering::SeqCst);
        Ok(vec![Self::window_for_generation(generation)])
    }

    async fn list_displays(&self) -> Result<Vec<DriverDisplay>, DriverError> {
        Ok(vec![DriverDisplay {
            key: "display-primary".to_owned(),
            screen_bounds: ScreenRect {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            primary: true,
        }])
    }

    async fn observe_display(
        &self,
        display: &DriverDisplay,
    ) -> Result<(RgbaImage, ScreenRect), DriverError> {
        Ok((RgbaImage::new(2, 2, vec![255; 16]), display.screen_bounds))
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
            elements: if accessibility == AccessibilityMode::Disabled {
                Vec::new()
            } else {
                vec![
                    DriverElement {
                        key: "button-key".to_owned(),
                        parent_key: None,
                        role: "button".to_owned(),
                        name: "Continue".to_owned(),
                        value: None,
                        range_value: None,
                        screen_bounds: Some(window.screen_bounds),
                        enabled: true,
                        focused: false,
                        selected: None,
                        checked: None,
                        expanded: None,
                        actions: vec!["press".to_owned()],
                    },
                    DriverElement {
                        key: "slider-key".to_owned(),
                        parent_key: None,
                        role: "slider".to_owned(),
                        name: "Volume".to_owned(),
                        value: Some("50".to_owned()),
                        range_value: Some(AccessibilityRangeValue {
                            value: 50.0,
                            minimum: 0.0,
                            maximum: 100.0,
                            small_change: Some(1.0),
                            large_change: Some(10.0),
                            read_only: false,
                        }),
                        screen_bounds: Some(window.screen_bounds),
                        enabled: true,
                        focused: false,
                        selected: None,
                        checked: None,
                        expanded: None,
                        actions: vec!["focus".to_owned(), "set_range_value".to_owned()],
                    },
                    DriverElement {
                        key: "readonly-slider-key".to_owned(),
                        parent_key: None,
                        role: "progressbar".to_owned(),
                        name: "Progress".to_owned(),
                        value: Some("25".to_owned()),
                        range_value: Some(AccessibilityRangeValue {
                            value: 25.0,
                            minimum: 0.0,
                            maximum: 100.0,
                            small_change: None,
                            large_change: None,
                            read_only: true,
                        }),
                        screen_bounds: Some(window.screen_bounds),
                        enabled: true,
                        focused: false,
                        selected: None,
                        checked: None,
                        expanded: None,
                        actions: Vec::new(),
                    },
                ]
            },
            elements_complete: true,
            elements_truncation: None,
            fingerprint: format!("revision-{revision}"),
        })
    }

    async fn read_clipboard(
        &self,
        max_text_bytes: usize,
    ) -> Result<DriverClipboardSnapshot, DriverError> {
        let clipboard = self.clipboard.lock().await;
        if clipboard.1.len() > max_text_bytes {
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "fixture clipboard exceeds bound",
            ));
        }
        Ok(DriverClipboardSnapshot {
            change_marker: format!("fixture:{}", clipboard.0),
            text: Some(SensitiveText::new(clipboard.1.clone())),
        })
    }

    async fn write_clipboard(
        &self,
        expected_change_marker: &str,
        text: &SensitiveText,
        max_text_bytes: usize,
    ) -> Result<DriverClipboardWriteOutput, DriverError> {
        if text.expose().len() > max_text_bytes {
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "fixture clipboard replacement exceeds bound",
            )
            .mutation_not_dispatched());
        }
        let mut clipboard = self.clipboard.lock().await;
        if expected_change_marker != format!("fixture:{}", clipboard.0) {
            return Err(DriverError::new(
                DriverErrorKind::StaleClipboard,
                "fixture clipboard changed",
            )
            .retryable("read_clipboard_again")
            .mutation_not_dispatched());
        }
        clipboard.0 += 1;
        text.expose().clone_into(&mut clipboard.1);
        Ok(DriverClipboardWriteOutput {
            change_marker: format!("fixture:{}", clipboard.0),
            effect_status: nexus_cua_protocol::ActionEffectStatus::Confirmed,
            evidence_kind: nexus_cua_protocol::ActionEvidenceKind::NativeReadback,
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
        restore_foreground: bool,
    ) -> Result<DriverActionOutput, DriverError> {
        self.restore_foreground_requested
            .store(restore_foreground, Ordering::SeqCst);
        if let Some(gate) = &self.action_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if let Some(status) = self.action_failure {
            let error =
                DriverError::new(DriverErrorKind::TargetUnresponsive, "fixture action failed");
            return Err(match status {
                MutationStatus::NotDispatched => error.mutation_not_dispatched(),
                MutationStatus::Indeterminate => error.mutation_indeterminate(),
                MutationStatus::NotApplicable => error,
            });
        }
        let supports_foreground_restoration = action.supports_foreground_restoration();
        let (delivery_mode, confirmed) = match action {
            DriverAction::TerminateApplication => {
                self.application_generation.fetch_add(1, Ordering::SeqCst);
                (DeliveryMode::System, true)
            }
            DriverAction::FocusElement { .. }
            | DriverAction::InvokeElement { .. }
            | DriverAction::SetRangeValue { .. }
            | DriverAction::ToggleElement { .. }
            | DriverAction::SelectElement { .. }
            | DriverAction::SetExpanded { .. } => (DeliveryMode::Semantic, false),
            DriverAction::ClickPoint {
                delivery: DeliveryPreference::Background,
                ..
            } => (DeliveryMode::Background, false),
            _ if allow_foreground => (DeliveryMode::Foreground, false),
            _ => panic!("test runtime must reject foreground action before driver dispatch"),
        };
        *self.revision.lock().await += 1;
        let output = if confirmed {
            DriverActionOutput::confirmed(delivery_mode)
        } else {
            DriverActionOutput::unverified(delivery_mode)
        };
        if let Some(status) = self.forced_restoration {
            return Ok(output.with_foreground_restoration(status));
        }
        Ok(if restore_foreground && supports_foreground_restoration {
            output.with_foreground_restoration(ForegroundRestorationStatus::SkippedOwnerChanged)
        } else {
            output
        })
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
            vec![
                ActionKind::TerminateApplication,
                ActionKind::InvokeElement,
                ActionKind::SetRangeValue,
                ActionKind::ClickPoint,
            ]
        } else {
            Vec::new()
        },
        allow_foreground_input: mode == PermissionMode::Bounded,
        restore_foreground_after_input: false,
        allow_display_capture: false,
        allow_clipboard_read: false,
        allow_clipboard_write: false,
        ttl_seconds: 300,
    }
}

fn lifecycle_manifest(application_ref: DiscoveryRef) -> CapabilityManifest {
    CapabilityManifest {
        mode: PermissionMode::Bounded,
        application_refs: vec![application_ref],
        allowed_actions: vec![ActionKind::TerminateApplication, ActionKind::InvokeElement],
        allow_foreground_input: false,
        restore_foreground_after_input: false,
        allow_display_capture: false,
        allow_clipboard_read: false,
        allow_clipboard_write: false,
        ttl_seconds: 300,
    }
}

fn clipboard_manifest(mode: PermissionMode, write: bool) -> CapabilityManifest {
    CapabilityManifest {
        mode,
        application_refs: Vec::new(),
        allowed_actions: Vec::new(),
        allow_foreground_input: false,
        restore_foreground_after_input: false,
        allow_display_capture: false,
        allow_clipboard_read: true,
        allow_clipboard_write: write,
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

#[tokio::test]
async fn clipboard_only_session_reads_and_guardedly_replaces_text() {
    let root = artifact_root();
    let driver = Arc::new(MockDriver::new());
    let runtime = Runtime::new(driver.clone(), RuntimeConfig::new(&root)).expect("create runtime");
    let session_id =
        open_session_with_manifest(&runtime, clipboard_manifest(PermissionMode::Bounded, true))
            .await;

    let CommandResult::ClipboardRead(first) = runtime
        .execute(Command::ReadClipboard(ReadClipboardInput {
            session_id: session_id.clone(),
        }))
        .await
        .expect("read clipboard")
    else {
        panic!("unexpected clipboard-read result");
    };
    assert_eq!(
        first.text.as_ref().map(SensitiveText::expose),
        Some("fixture clipboard")
    );
    assert_eq!(first.byte_length, 17);

    driver
        .replace_clipboard_externally("external replacement")
        .await;
    let stale = runtime
        .execute(Command::WriteClipboard(WriteClipboardInput {
            session_id: session_id.clone(),
            clipboard_ref: first.clipboard_ref,
            text: SensitiveText::new("must not overwrite"),
        }))
        .await
        .expect_err("native change marker must reject stale write");
    assert_eq!(stale.code, ErrorCode::StaleClipboard);
    assert_eq!(stale.mutation_status, MutationStatus::NotDispatched);

    let CommandResult::ClipboardRead(second) = runtime
        .execute(Command::ReadClipboard(ReadClipboardInput {
            session_id: session_id.clone(),
        }))
        .await
        .expect("read changed clipboard")
    else {
        panic!("unexpected second clipboard-read result");
    };
    let consumed_ref = second.clipboard_ref.clone();
    let CommandResult::ClipboardWritten(written) = runtime
        .execute(Command::WriteClipboard(WriteClipboardInput {
            session_id: session_id.clone(),
            clipboard_ref: consumed_ref.clone(),
            text: SensitiveText::new("authorized replacement"),
        }))
        .await
        .expect("write guarded clipboard")
    else {
        panic!("unexpected clipboard-write result");
    };
    assert!(written.dispatched);
    assert_eq!(written.delivery_mode, DeliveryMode::System);
    assert_ne!(written.clipboard_ref, consumed_ref);

    let reused = runtime
        .execute(Command::WriteClipboard(WriteClipboardInput {
            session_id,
            clipboard_ref: consumed_ref,
            text: SensitiveText::new("duplicate write"),
        }))
        .await
        .expect_err("consumed clipboard guard must not be reusable");
    assert_eq!(reused.code, ErrorCode::StaleClipboard);
    assert_eq!(reused.mutation_status, MutationStatus::NotDispatched);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn clipboard_authority_and_text_bounds_fail_closed_without_consuming_valid_guard() {
    let (runtime, root) = runtime();
    let read_only = open_session_with_manifest(
        &runtime,
        clipboard_manifest(PermissionMode::ReadOnly, false),
    )
    .await;
    let CommandResult::ClipboardRead(snapshot) = runtime
        .execute(Command::ReadClipboard(ReadClipboardInput {
            session_id: read_only.clone(),
        }))
        .await
        .expect("read-only clipboard read")
    else {
        panic!("unexpected clipboard-read result");
    };
    let denied = runtime
        .execute(Command::WriteClipboard(WriteClipboardInput {
            session_id: read_only,
            clipboard_ref: snapshot.clipboard_ref,
            text: SensitiveText::new("denied"),
        }))
        .await
        .expect_err("read-only clipboard session must reject writes");
    assert_eq!(denied.code, ErrorCode::CapabilityDenied);
    assert_eq!(denied.mutation_status, MutationStatus::NotDispatched);

    let bounded =
        open_session_with_manifest(&runtime, clipboard_manifest(PermissionMode::Bounded, true))
            .await;
    let CommandResult::ClipboardRead(snapshot) = runtime
        .execute(Command::ReadClipboard(ReadClipboardInput {
            session_id: bounded.clone(),
        }))
        .await
        .expect("read bounded clipboard")
    else {
        panic!("unexpected clipboard-read result");
    };
    let clipboard_ref = snapshot.clipboard_ref;
    let too_large = runtime
        .execute(Command::WriteClipboard(WriteClipboardInput {
            session_id: bounded.clone(),
            clipboard_ref: clipboard_ref.clone(),
            text: SensitiveText::new("x".repeat(64 * 1024 + 1)),
        }))
        .await
        .expect_err("oversized clipboard write must fail before dispatch");
    assert_eq!(too_large.code, ErrorCode::InvalidRequest);
    assert_eq!(too_large.mutation_status, MutationStatus::NotDispatched);
    runtime
        .execute(Command::WriteClipboard(WriteClipboardInput {
            session_id: bounded,
            clipboard_ref,
            text: SensitiveText::new("within bound"),
        }))
        .await
        .expect("validation failure must not consume a valid guard");

    let invalid_manifest = CapabilityManifest {
        allow_clipboard_read: false,
        allow_clipboard_write: true,
        ..clipboard_manifest(PermissionMode::Bounded, true)
    };
    let invalid = runtime
        .execute(Command::OpenSession(OpenSessionInput {
            manifest: invalid_manifest,
        }))
        .await
        .expect_err("write authority without read authority must be invalid");
    assert_eq!(invalid.code, ErrorCode::InvalidRequest);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn display_overview_is_separately_authorized_and_never_creates_app_authority() {
    let (runtime, root) = runtime();
    let denied_session = open_session(&runtime, PermissionMode::ReadOnly).await;
    let denied = runtime
        .execute(Command::ListDisplays(SessionInput {
            session_id: denied_session,
        }))
        .await
        .expect_err("ordinary window session must not inherit display capture");
    assert_eq!(denied.code, ErrorCode::CapabilityDenied);

    let display_session = open_session_with_manifest(
        &runtime,
        CapabilityManifest {
            mode: PermissionMode::ReadOnly,
            application_refs: Vec::new(),
            allowed_actions: Vec::new(),
            allow_foreground_input: false,
            restore_foreground_after_input: false,
            allow_display_capture: true,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            ttl_seconds: 300,
        },
    )
    .await;
    let CommandResult::Displays(displays) = runtime
        .execute(Command::ListDisplays(SessionInput {
            session_id: display_session.clone(),
        }))
        .await
        .expect("list separately authorized displays")
    else {
        panic!("unexpected display-list result");
    };
    assert_eq!(displays.len(), 1);
    assert!(displays[0].primary);

    let CommandResult::DisplayObserved(observation) = runtime
        .execute(Command::ObserveDisplay(ObserveDisplayInput {
            session_id: display_session.clone(),
            display_ref: displays[0].display_ref.clone(),
        }))
        .await
        .expect("observe exact display")
    else {
        panic!("unexpected display observation result");
    };
    assert_eq!(observation.display_screen_bounds, displays[0].screen_bounds);
    assert!(std::path::Path::new(&observation.screenshot.path).is_file());

    let CommandResult::Apps(apps) = runtime
        .execute(Command::ListApps(SessionInput {
            session_id: display_session,
        }))
        .await
        .expect("display-only session lists no controlled applications")
    else {
        panic!("unexpected app-list result");
    };
    assert!(apps.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

async fn open_session(runtime: &Runtime, mode: PermissionMode) -> nexus_cua_protocol::SessionId {
    open_session_with_ttl(runtime, mode, 300).await
}

async fn open_session_with_manifest(
    runtime: &Runtime,
    manifest: CapabilityManifest,
) -> nexus_cua_protocol::SessionId {
    match runtime
        .execute(Command::OpenSession(OpenSessionInput { manifest }))
        .await
        .expect("open session")
    {
        CommandResult::SessionOpened(output) => output.session_id,
        _ => panic!("unexpected open-session result"),
    }
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

async fn foreground_click_input(
    runtime: &Runtime,
    session_id: nexus_cua_protocol::SessionId,
    window_ref: nexus_cua_protocol::WindowRef,
) -> nexus_cua_protocol::PerformActionInput {
    let observed = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id: session_id.clone(),
            window_ref: window_ref.clone(),
            include_screenshot: true,
            accessibility: AccessibilityMode::Disabled,
        }))
        .await
        .expect("observe foreground target");
    let CommandResult::WindowObserved(observation) = observed else {
        panic!("unexpected observation result");
    };
    nexus_cua_protocol::PerformActionInput {
        session_id,
        window_ref,
        observation_id: observation.observation_id,
        action: Action::ClickPoint {
            point: ScreenshotPoint { x: 1, y: 1 },
            button: nexus_cua_protocol::PointerButton::Left,
            count: 1,
            delivery: DeliveryPreference::Foreground,
        },
    }
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
async fn foreground_restoration_requires_foreground_authority() {
    let (runtime, root) = runtime();
    let discovery_ref = discover_application(&runtime).await;
    let mut invalid = manifest(PermissionMode::Bounded, discovery_ref);
    invalid.allow_foreground_input = false;
    invalid.restore_foreground_after_input = true;
    let error = runtime
        .execute(Command::OpenSession(OpenSessionInput { manifest: invalid }))
        .await
        .expect_err("restoration without foreground authority must fail");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn runtime_policy_denies_before_observation_consumption() {
    let root = artifact_root();
    let layer = RuntimePolicyLayer {
        schema_version: 1,
        allow: PolicyAllow {
            operations: vec![
                PolicyOperation::DiscoverApplications,
                PolicyOperation::OpenSession,
                PolicyOperation::ListWindows,
                PolicyOperation::ObserveWindow,
                PolicyOperation::PerformAction,
            ],
            actions: Vec::new(),
            action_rules: vec![PolicyActionRule {
                action: ActionKind::ClickPoint,
                max_text_bytes: None,
                max_keys: None,
                max_click_count: Some(1),
                max_duration_ms: None,
                max_abs_scroll_delta: None,
                deliveries: vec![DeliveryPreference::Background],
            }],
        },
        deny: PolicyDeny::default(),
    };
    let mut config = RuntimeConfig::new(&root);
    config.policy = RuntimePolicyStack::from_layers(Some(layer), None).expect("valid policy");
    let runtime = Runtime::new(Arc::new(MockDriver::new()), config).expect("create runtime");
    let discovery_ref = discover_application(&runtime).await;
    let session_id = open_session_with_manifest(
        &runtime,
        CapabilityManifest {
            mode: PermissionMode::Bounded,
            application_refs: vec![discovery_ref],
            allowed_actions: vec![ActionKind::ClickPoint],
            allow_foreground_input: false,
            restore_foreground_after_input: false,
            allow_display_capture: false,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            ttl_seconds: 300,
        },
    )
    .await;
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
            include_screenshot: true,
            accessibility: AccessibilityMode::Disabled,
        }))
        .await
        .expect("observe window");
    let CommandResult::WindowObserved(observation) = observed else {
        panic!("unexpected observation result");
    };
    let action = |count| {
        Command::PerformAction(nexus_cua_protocol::PerformActionInput {
            session_id: session_id.clone(),
            window_ref: window_ref.clone(),
            observation_id: observation.observation_id.clone(),
            action: Action::ClickPoint {
                point: ScreenshotPoint { x: 1, y: 1 },
                button: nexus_cua_protocol::PointerButton::Left,
                count,
                delivery: DeliveryPreference::Background,
            },
        })
    };

    let denied = runtime
        .execute(action(2))
        .await
        .expect_err("policy must reject count above the argument limit");
    assert_eq!(denied.code, ErrorCode::CapabilityDenied);
    assert_eq!(denied.mutation_status, MutationStatus::NotDispatched);
    let admitted = runtime
        .execute(action(1))
        .await
        .expect("denial must not consume the guarded observation");
    assert!(matches!(admitted, CommandResult::ActionPerformed(_)));
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

#[tokio::test]
async fn cold_launch_is_single_use_revalidated_and_returns_session_authority() {
    let (runtime, root) = runtime();
    let CommandResult::LaunchableApplicationsDiscovered(discovered) = runtime
        .execute(Command::DiscoverLaunchableApplications)
        .await
        .expect("discover installed applications")
    else {
        panic!("unexpected launch discovery result");
    };
    assert!(discovered.complete);
    assert_eq!(discovered.applications.len(), 1);
    let launch_ref = discovered.applications[0].launch_ref.clone();

    let CommandResult::ApplicationLaunched(output) = runtime
        .execute(Command::LaunchApplication(LaunchApplicationInput {
            launch_ref: launch_ref.clone(),
            allow_foreground_activation: false,
        }))
        .await
        .expect("launch exact installed identity")
    else {
        panic!("unexpected launch result");
    };
    assert!(output.dispatched);
    assert_eq!(output.delivery_mode, DeliveryMode::System);
    assert!(!output.application.foreground);
    assert_eq!(
        output.application.application_id,
        "dev.nexus.launch-fixture"
    );
    open_session_with_manifest(
        &runtime,
        CapabilityManifest {
            mode: PermissionMode::ReadOnly,
            application_refs: vec![output.application.discovery_ref],
            allowed_actions: Vec::new(),
            allow_foreground_input: false,
            restore_foreground_after_input: false,
            allow_display_capture: false,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            ttl_seconds: 300,
        },
    )
    .await;

    let stale = runtime
        .execute(Command::LaunchApplication(LaunchApplicationInput {
            launch_ref,
            allow_foreground_activation: false,
        }))
        .await
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleDiscovery);
    assert_eq!(stale.mutation_status, MutationStatus::NotDispatched);

    runtime.shutdown().await;
    drop(runtime);
    let _ = std::fs::remove_dir_all(root);
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
            assert_eq!(observation.elements.len(), 3);
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
async fn numeric_range_action_is_bounded_by_the_guarded_native_metadata() {
    let (runtime, root) = runtime();
    let discovery_ref = discover_application(&runtime).await;
    let session_id = open_session_with_manifest(
        &runtime,
        CapabilityManifest {
            mode: PermissionMode::Bounded,
            application_refs: vec![discovery_ref],
            allowed_actions: vec![ActionKind::SetRangeValue],
            allow_foreground_input: false,
            restore_foreground_after_input: false,
            allow_display_capture: false,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            ttl_seconds: 300,
        },
    )
    .await;
    let (window_ref, observation) = observed_fixture_window(&runtime, &session_id).await;
    let range_element = observation.elements[1].clone();
    assert_eq!(
        range_element.range_value,
        Some(AccessibilityRangeValue {
            value: 50.0,
            minimum: 0.0,
            maximum: 100.0,
            small_change: Some(1.0),
            large_change: Some(10.0),
            read_only: false,
        })
    );

    let outside = runtime
        .execute(Command::PerformAction(
            nexus_cua_protocol::PerformActionInput {
                session_id: session_id.clone(),
                window_ref: window_ref.clone(),
                observation_id: observation.observation_id.clone(),
                action: Action::SetRangeValue {
                    element_ref: range_element.element_ref.clone(),
                    value: 100.5,
                },
            },
        ))
        .await
        .expect_err("out-of-range value must fail before dispatch");
    assert_eq!(outside.code, ErrorCode::InvalidRequest);
    assert_eq!(outside.mutation_status, MutationStatus::NotDispatched);

    let output = runtime
        .execute(Command::PerformAction(
            nexus_cua_protocol::PerformActionInput {
                session_id: session_id.clone(),
                window_ref: window_ref.clone(),
                observation_id: observation.observation_id,
                action: Action::SetRangeValue {
                    element_ref: range_element.element_ref,
                    value: 75.0,
                },
            },
        ))
        .await
        .expect("in-range value must use semantic dispatch without foreground authority");
    let CommandResult::ActionPerformed(output) = output else {
        panic!("unexpected action result");
    };
    assert_eq!(output.delivery_mode, DeliveryMode::Semantic);

    let (_, observation) = observed_fixture_window(&runtime, &session_id).await;
    let read_only = &observation.elements[2];
    let error = runtime
        .execute(Command::PerformAction(
            nexus_cua_protocol::PerformActionInput {
                session_id,
                window_ref,
                observation_id: observation.observation_id,
                action: Action::SetRangeValue {
                    element_ref: read_only.element_ref.clone(),
                    value: 30.0,
                },
            },
        ))
        .await
        .expect_err("read-only native range must fail before dispatch");
    assert_eq!(error.code, ErrorCode::InvalidRequest);
    assert_eq!(error.mutation_status, MutationStatus::NotDispatched);
    let _ = std::fs::remove_dir_all(root);
}

async fn observed_fixture_window(
    runtime: &Runtime,
    session_id: &nexus_cua_protocol::SessionId,
) -> (
    nexus_cua_protocol::WindowRef,
    nexus_cua_protocol::WindowObservation,
) {
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
        .expect("observe range control");
    let CommandResult::WindowObserved(observation) = observed else {
        panic!("unexpected observation result");
    };
    (window_ref, *observation)
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

#[tokio::test]
async fn runtime_rejects_driver_restoration_without_authority() {
    let root = artifact_root();
    let runtime = Runtime::new(
        Arc::new(MockDriver::invalid_restoration()),
        RuntimeConfig::new(&root),
    )
    .expect("create runtime");
    let input = fixture_invoke_input(&runtime).await;

    let error = runtime
        .execute(Command::PerformAction(input))
        .await
        .expect_err("unauthorized driver restoration must fail closed");

    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(error.mutation_status, MutationStatus::Indeterminate);
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
                    delivery: nexus_cua_protocol::DeliveryPreference::Foreground,
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
async fn cooperative_termination_is_system_routed_and_invalidates_peer_sessions() {
    let (runtime, root) = runtime();
    let discovery_ref = discover_application(&runtime).await;
    let first_session =
        open_session_with_manifest(&runtime, lifecycle_manifest(discovery_ref.clone())).await;
    let second_session =
        open_session_with_manifest(&runtime, lifecycle_manifest(discovery_ref)).await;

    let first_windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: first_session.clone(),
            app_ref: None,
        }))
        .await
        .expect("list first-session windows");
    let CommandResult::Windows(first_windows) = first_windows else {
        panic!("unexpected windows result");
    };
    let first_window = first_windows[0].window_ref.clone();
    let first_observation = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id: first_session.clone(),
            window_ref: first_window.clone(),
            include_screenshot: false,
            accessibility: AccessibilityMode::Disabled,
        }))
        .await
        .expect("observe first-session window");
    let CommandResult::WindowObserved(first_observation) = first_observation else {
        panic!("unexpected observation result");
    };

    let second_windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: second_session.clone(),
            app_ref: None,
        }))
        .await
        .expect("list second-session windows");
    let CommandResult::Windows(second_windows) = second_windows else {
        panic!("unexpected windows result");
    };
    let second_window = second_windows[0].window_ref.clone();
    let second_observation = runtime
        .execute(Command::ObserveWindow(ObserveWindowInput {
            session_id: second_session.clone(),
            window_ref: second_window.clone(),
            include_screenshot: false,
            accessibility: AccessibilityMode::Interactive,
        }))
        .await
        .expect("observe second-session window");
    let CommandResult::WindowObserved(second_observation) = second_observation else {
        panic!("unexpected observation result");
    };
    let second_element = second_observation.elements[0].element_ref.clone();
    let second_observation_id = second_observation.observation_id.clone();

    let terminated = runtime
        .execute(Command::PerformAction(
            nexus_cua_protocol::PerformActionInput {
                session_id: first_session,
                window_ref: first_window,
                observation_id: first_observation.observation_id,
                action: Action::TerminateApplication,
            },
        ))
        .await
        .expect("terminate exact application generation");
    let CommandResult::ActionPerformed(terminated) = terminated else {
        panic!("unexpected action result");
    };
    assert_eq!(terminated.delivery_mode, DeliveryMode::System);
    assert_eq!(
        terminated.effect_status,
        nexus_cua_protocol::ActionEffectStatus::Confirmed
    );
    assert_eq!(
        terminated.evidence_kind,
        nexus_cua_protocol::ActionEvidenceKind::NativeReadback
    );

    let error = runtime
        .execute(Command::PerformAction(
            nexus_cua_protocol::PerformActionInput {
                session_id: second_session,
                window_ref: second_window,
                observation_id: second_observation_id,
                action: Action::InvokeElement {
                    element_ref: second_element,
                },
            },
        ))
        .await
        .expect_err("peer-session observation must be invalidated");
    assert_eq!(error.code, ErrorCode::StaleObservation);
    assert_eq!(error.mutation_status, MutationStatus::NotDispatched);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn authorized_foreground_restoration_is_forwarded_and_projected() {
    let root = artifact_root();
    let driver = Arc::new(MockDriver::new());
    let runtime = Runtime::new(driver.clone(), RuntimeConfig::new(&root)).expect("create runtime");
    let discovery_ref = discover_application(&runtime).await;
    let mut authority = manifest(PermissionMode::Bounded, discovery_ref);
    authority.restore_foreground_after_input = true;
    let session_id = open_session_with_manifest(&runtime, authority).await;
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
    let input = foreground_click_input(&runtime, session_id, windows[0].window_ref.clone()).await;

    let output = runtime
        .execute(Command::PerformAction(input))
        .await
        .expect("perform foreground action");
    let CommandResult::ActionPerformed(output) = output else {
        panic!("unexpected action result");
    };
    assert!(driver.restore_foreground_requested.load(Ordering::SeqCst));
    assert_eq!(
        output.foreground_restoration,
        ForegroundRestorationStatus::SkippedOwnerChanged
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn concurrent_foreground_action_fails_fast_without_queueing() {
    let root = artifact_root();
    let gate = Arc::new(ActionGate::default());
    let runtime = Arc::new(
        Runtime::new(
            Arc::new(MockDriver::gated_action(gate.clone())),
            RuntimeConfig::new(&root),
        )
        .expect("create runtime"),
    );
    let first_session = open_session(&runtime, PermissionMode::Bounded).await;
    let second_session = open_session(&runtime, PermissionMode::Bounded).await;
    let first_windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: first_session.clone(),
            app_ref: None,
        }))
        .await
        .expect("list first-session windows");
    let CommandResult::Windows(first_windows) = first_windows else {
        panic!("unexpected windows result");
    };
    let second_windows = runtime
        .execute(Command::ListWindows(ListWindowsInput {
            session_id: second_session.clone(),
            app_ref: None,
        }))
        .await
        .expect("list second-session windows");
    let CommandResult::Windows(second_windows) = second_windows else {
        panic!("unexpected windows result");
    };
    let first_input =
        foreground_click_input(&runtime, first_session, first_windows[0].window_ref.clone()).await;
    let second_input = foreground_click_input(
        &runtime,
        second_session,
        second_windows[0].window_ref.clone(),
    )
    .await;

    let first_runtime = runtime.clone();
    let first = tokio::spawn(async move {
        first_runtime
            .execute(Command::PerformAction(first_input))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), gate.entered.notified())
        .await
        .expect("first action entered driver");

    let error = runtime
        .execute(Command::PerformAction(second_input))
        .await
        .expect_err("second foreground action must not queue");
    assert_eq!(error.code, ErrorCode::Busy);
    assert_eq!(error.mutation_status, MutationStatus::NotDispatched);

    gate.release.notify_one();
    first
        .await
        .expect("first action task")
        .expect("first action dispatch");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn background_click_does_not_require_foreground_authority() {
    let (runtime, root) = runtime();
    let discovery_ref = discover_application(&runtime).await;
    let session_id = match runtime
        .execute(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::Bounded,
                application_refs: vec![discovery_ref],
                allowed_actions: vec![ActionKind::ClickPoint],
                allow_foreground_input: false,
                restore_foreground_after_input: false,
                allow_display_capture: false,
                allow_clipboard_read: false,
                allow_clipboard_write: false,
                ttl_seconds: 300,
            },
        }))
        .await
        .expect("open background-only session")
    {
        CommandResult::SessionOpened(output) => output.session_id,
        _ => panic!("unexpected open-session result"),
    };
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
        .expect("observe background target");
    let observation_id = match observed {
        CommandResult::WindowObserved(observation) => observation.observation_id,
        _ => panic!("unexpected observation result"),
    };
    let output = runtime
        .execute(Command::PerformAction(
            nexus_cua_protocol::PerformActionInput {
                session_id,
                window_ref,
                observation_id,
                action: Action::ClickPoint {
                    point: ScreenshotPoint { x: 1, y: 1 },
                    button: nexus_cua_protocol::PointerButton::Left,
                    count: 1,
                    delivery: DeliveryPreference::Background,
                },
            },
        ))
        .await
        .expect("background click must dispatch without foreground grant");
    let CommandResult::ActionPerformed(output) = output else {
        panic!("unexpected action result");
    };
    assert_eq!(output.delivery_mode, DeliveryMode::Background);
    assert_eq!(
        output.effect_status,
        nexus_cua_protocol::ActionEffectStatus::Unverified
    );
    assert_eq!(
        output.evidence_kind,
        nexus_cua_protocol::ActionEvidenceKind::DispatchAcceptance
    );
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

#[tokio::test]
async fn artifact_read_revalidates_runtime_identity_length_and_digest() {
    let (runtime, root) = runtime();
    let session_id = open_session(&runtime, PermissionMode::ReadOnly).await;
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
            session_id,
            window_ref: windows[0].window_ref.clone(),
            include_screenshot: true,
            accessibility: AccessibilityMode::Disabled,
        }))
        .await
        .expect("observe screenshot");
    let CommandResult::WindowObserved(observation) = observed else {
        panic!("unexpected observation result");
    };
    let artifact = observation.screenshot.expect("screenshot artifact");
    let bytes = runtime
        .read_artifact(&artifact, artifact.byte_length)
        .await
        .expect("read exact runtime artifact");
    assert_eq!(bytes.len() as u64, artifact.byte_length);
    assert_eq!(hex::encode(sha2::Sha256::digest(&bytes)), artifact.sha256);
    assert!(
        runtime
            .read_artifact(&artifact, artifact.byte_length - 1)
            .await
            .is_err()
    );
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
