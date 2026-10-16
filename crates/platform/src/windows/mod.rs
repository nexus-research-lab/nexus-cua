//! Windows Graphics Capture, UI Automation, and `SendInput` driver.

mod capture;
mod clipboard;
mod discovery;
mod input;
mod launch;
pub(crate) mod provenance;
mod semantic;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, ActionEffectStatus, ActionEvidenceKind, ActionKind,
    ApplicationLifecycleOperation, ApplicationProvenance, CaptureMode, ClipboardOperation,
    DeliveryMode, DeliveryPreference, DriverCapabilities, ElementState,
    ForegroundRestorationStatus, InputRoute, PROTOCOL_VERSION, PermissionState, PermissionStatus,
    Platform, SensitiveText, SignatureStatus, StatePredicate,
};
use nexus_cua_runtime::{
    DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverClipboardSnapshot,
    DriverClipboardWriteOutput, DriverDisplay, DriverError, DriverErrorKind, DriverLaunchOutput,
    DriverLaunchableApplication, DriverObservation, DriverVerification, DriverWindow,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use capture::CaptureActor;
use clipboard::ClipboardActor;
use discovery::{DiscoveryActor, NativeWindow};
use input::{InputAction, InputActor};
use launch::LaunchActor;
use semantic::{SemanticAction, SemanticActor};
use tokio::sync::Semaphore;

use crate::observation::{contains_rect, fingerprint, fingerprints_match};

/// Windows native driver with one actor for each native threading domain.
pub struct WindowsDriver {
    discovery: DiscoveryActor,
    capture: CaptureActor,
    semantic: SemanticActor,
    input: InputActor,
    launch: LaunchActor,
    clipboard: ClipboardActor,
    provenance_workers: Arc<Semaphore>,
}

const PROVENANCE_WORKERS: usize = 2;
const LAUNCH_CONFIRM_TIMEOUT: Duration = Duration::from_secs(10);
const LAUNCH_CONFIRM_INTERVAL: Duration = Duration::from_millis(50);

impl WindowsDriver {
    /// Creates the native actors without prompting or widening OS authority.
    pub fn new() -> Result<Self, DriverError> {
        // SAFETY: This must run before creating platform windows. Failure means
        // the host already selected a process DPI mode; screenshot mappings
        // continue to make coordinate conversion explicit.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Ok(Self {
            discovery: DiscoveryActor::spawn(),
            capture: CaptureActor::spawn()?,
            semantic: SemanticActor::spawn()?,
            input: InputActor::spawn(),
            launch: LaunchActor::spawn(),
            clipboard: ClipboardActor::spawn()?,
            provenance_workers: Arc::new(Semaphore::new(PROVENANCE_WORKERS)),
        })
    }

    async fn windows(&self) -> Result<Vec<NativeWindow>, DriverError> {
        self.discovery.windows().await
    }

    async fn focus_for_input(&self, window: &NativeWindow) -> Result<(), DriverError> {
        self.input.perform(window.hwnd, InputAction::Activate).await
    }
}

#[async_trait]
impl DesktopDriver for WindowsDriver {
    async fn capabilities(&self) -> Result<DriverCapabilities, DriverError> {
        Ok(DriverCapabilities {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: Platform::Windows,
            capture_modes: vec![CaptureMode::Window, CaptureMode::Display],
            accessibility_tree: true,
            input_routes: vec![
                InputRoute::Semantic,
                InputRoute::System,
                InputRoute::Background,
                InputRoute::Foreground,
            ],
            application_lifecycle: vec![ApplicationLifecycleOperation::Launch],
            clipboard: vec![ClipboardOperation::ReadText, ClipboardOperation::WriteText],
            actions: vec![
                ActionKind::FocusWindow,
                ActionKind::SetWindowBounds,
                ActionKind::SetWindowMinimized,
                ActionKind::TerminateApplication,
                ActionKind::FocusElement,
                ActionKind::InvokeElement,
                ActionKind::ClickPoint,
                ActionKind::PressPoint,
                ActionKind::SetValue,
                ActionKind::SetRangeValue,
                ActionKind::ToggleElement,
                ActionKind::SelectElement,
                ActionKind::SetExpanded,
                ActionKind::MovePointer,
                ActionKind::TypeText,
                ActionKind::PressKeys,
                ActionKind::Scroll,
                ActionKind::Drag,
            ],
        })
    }

    async fn permission_status(&self) -> Result<PermissionStatus, DriverError> {
        Ok(PermissionStatus {
            screen_capture: PermissionState::NotApplicable,
            accessibility: PermissionState::NotApplicable,
            input_control: PermissionState::NotApplicable,
        })
    }

    async fn list_applications(&self) -> Result<Vec<DriverApplication>, DriverError> {
        let windows = self.windows().await?;
        let permit = Arc::clone(&self.provenance_workers)
            .try_acquire_owned()
            .map_err(|_| {
                DriverError::new(
                    DriverErrorKind::Busy,
                    "Windows provenance worker capacity is exhausted",
                )
                .retryable("retry_with_backoff")
            })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut applications = HashMap::new();
            for window in windows {
                applications
                    .entry(window.application_key.clone())
                    .or_insert_with(|| discovered_application(&window));
            }
            applications.into_values().collect()
        })
        .await
        .map_err(|_| {
            DriverError::new(
                DriverErrorKind::Platform,
                "Windows provenance worker stopped unexpectedly",
            )
        })
    }

    async fn list_launchable_applications(
        &self,
    ) -> Result<Vec<DriverLaunchableApplication>, DriverError> {
        self.launch.list().await
    }

    async fn launch_application(
        &self,
        application: &DriverLaunchableApplication,
        allow_foreground_activation: bool,
    ) -> Result<DriverLaunchOutput, DriverError> {
        self.launch
            .launch(application.clone(), allow_foreground_activation)
            .await?;
        let deadline = Instant::now() + LAUNCH_CONFIRM_TIMEOUT;
        loop {
            let windows = self
                .windows()
                .await
                .map_err(DriverError::mutation_indeterminate)?;
            if let Some(window) = windows
                .into_iter()
                .find(|window| window.application_id == application.identity)
            {
                if !allow_foreground_activation && window.foreground {
                    return Err(DriverError::new(
                        DriverErrorKind::Platform,
                        "Windows launch exceeded foreground activation authority",
                    )
                    .retryable("discover_applications")
                    .mutation_indeterminate());
                }
                let mut launched = driver_application(&window);
                launched.name.clone_from(&application.name);
                launched.provenance.clone_from(&application.provenance);
                return Ok(DriverLaunchOutput {
                    application: launched,
                    delivery_mode: if allow_foreground_activation {
                        DeliveryMode::Foreground
                    } else {
                        DeliveryMode::System
                    },
                });
            }
            if Instant::now() >= deadline {
                return Err(DriverError::new(
                    DriverErrorKind::TargetUnresponsive,
                    "Windows launched the application but no exact usable top-level window appeared",
                )
                .retryable("discover_applications")
                .mutation_indeterminate());
            }
            tokio::time::sleep(LAUNCH_CONFIRM_INTERVAL).await;
        }
    }

    async fn list_windows(
        &self,
        application_key: Option<&str>,
    ) -> Result<Vec<DriverWindow>, DriverError> {
        Ok(self
            .windows()
            .await?
            .into_iter()
            .filter(|window| application_key.is_none_or(|key| key == window.application_key))
            .map(driver_window)
            .collect())
    }

    async fn list_displays(&self) -> Result<Vec<DriverDisplay>, DriverError> {
        Ok(self
            .discovery
            .displays()
            .await?
            .into_iter()
            .map(|display| DriverDisplay {
                key: display.key,
                screen_bounds: display.screen_bounds,
                primary: display.primary,
            })
            .collect())
    }

    async fn observe_display(
        &self,
        display: &DriverDisplay,
    ) -> Result<(nexus_cua_runtime::RgbaImage, nexus_cua_protocol::ScreenRect), DriverError> {
        self.capture
            .capture_display(&display.key, display.screen_bounds)
            .await
    }

    async fn observe_window(
        &self,
        window: &DriverWindow,
        include_screenshot: bool,
        accessibility: AccessibilityMode,
    ) -> Result<DriverObservation, DriverError> {
        let before = current_window(&self.windows().await?, &window.key)?;
        if include_screenshot && before.minimized {
            return Err(DriverError::new(
                DriverErrorKind::TargetUnavailable,
                "minimized Windows target cannot provide a current exact-window frame",
            )
            .retryable("restore_window_then_observe"));
        }
        let capture = async {
            if include_screenshot {
                self.capture
                    .capture(&before.key, before.hwnd, before.screen_bounds)
                    .await
                    .map(Some)
            } else {
                Ok(None)
            }
        };
        let semantics = async {
            if accessibility == AccessibilityMode::Disabled {
                Ok(None)
            } else {
                self.semantic
                    .snapshot(before.hwnd, accessibility)
                    .await
                    .map(Some)
            }
        };
        let (captured, semantics) = tokio::join!(capture, semantics);
        let captured = captured?;
        let semantics = semantics?;
        let after = current_window(&self.windows().await?, &window.key)?;
        if before.screen_bounds != after.screen_bounds {
            return Err(DriverError::new(
                DriverErrorKind::StaleObservation,
                "target geometry changed during observation",
            )
            .retryable("observe_window"));
        }
        let fingerprint = fingerprint(
            &after.key,
            after.screen_bounds,
            captured.as_ref().map(|(image, _bounds)| image),
        );
        let (screenshot, screenshot_screen_bounds) = match captured {
            Some((image, bounds)) if bounds == after.screen_bounds => (Some(image), Some(bounds)),
            Some(_) => {
                return Err(DriverError::new(
                    DriverErrorKind::StaleObservation,
                    "captured surface does not match the target geometry",
                )
                .retryable("observe_window"));
            }
            None => (None, None),
        };
        let (elements, elements_complete, elements_truncation) = semantics.map_or_else(
            || (Vec::new(), true, None),
            |snapshot| (snapshot.elements, snapshot.complete, snapshot.truncation),
        );
        Ok(DriverObservation {
            window_key: window.key.clone(),
            window_screen_bounds: after.screen_bounds,
            screenshot,
            screenshot_screen_bounds,
            elements,
            elements_complete,
            elements_truncation,
            fingerprint,
        })
    }

    async fn read_clipboard(
        &self,
        max_text_bytes: usize,
    ) -> Result<DriverClipboardSnapshot, DriverError> {
        self.clipboard.read(max_text_bytes).await
    }

    async fn write_clipboard(
        &self,
        expected_change_marker: &str,
        text: &SensitiveText,
        max_text_bytes: usize,
    ) -> Result<DriverClipboardWriteOutput, DriverError> {
        self.clipboard
            .write(
                expected_change_marker.to_owned(),
                text.clone(),
                max_text_bytes,
            )
            .await
    }

    async fn observation_is_current(
        &self,
        window: &DriverWindow,
        fingerprint_value: &str,
    ) -> Result<bool, DriverError> {
        let current = current_window(&self.windows().await?, &window.key)?;
        if fingerprint_value.contains(":visual:") {
            let (image, bounds) = self
                .capture
                .capture(&current.key, current.hwnd, current.screen_bounds)
                .await?;
            if bounds != current.screen_bounds {
                return Ok(false);
            }
            Ok(fingerprints_match(
                fingerprint_value,
                &fingerprint(&current.key, current.screen_bounds, Some(&image)),
            ))
        } else {
            Ok(fingerprint(&current.key, current.screen_bounds, None) == fingerprint_value)
        }
    }

    async fn perform_action(
        &self,
        window: &DriverWindow,
        action: DriverAction,
        allow_foreground: bool,
        restore_foreground: bool,
    ) -> Result<DriverActionOutput, DriverError> {
        let foreground = matches!(
            &action,
            DriverAction::FocusWindow
                | DriverAction::ClickPoint {
                    delivery: DeliveryPreference::Foreground,
                    ..
                }
                | DriverAction::PressPoint { .. }
                | DriverAction::MovePointer { .. }
                | DriverAction::TypeText { .. }
                | DriverAction::PressKeys { .. }
                | DriverAction::Scroll { .. }
                | DriverAction::Drag { .. }
        );
        if foreground && !allow_foreground {
            return Err(DriverError::new(
                DriverErrorKind::ForegroundRequired,
                "action requires foreground input",
            )
            .mutation_not_dispatched());
        }
        let current = current_window(&self.windows().await?, &window.key)
            .map_err(DriverError::mutation_not_dispatched)?;
        let result = self
            .dispatch_action(&current, action, restore_foreground)
            .await;
        if result.is_ok() {
            self.capture.invalidate_after_mutation();
        }
        result
    }

    async fn verify_state(
        &self,
        window: &DriverWindow,
        predicate: &StatePredicate,
    ) -> Result<DriverVerification, DriverError> {
        let current = current_window(&self.windows().await?, &window.key)?;
        let (matched, evidence) = match predicate {
            StatePredicate::WindowTitleContains { text } => (
                current.title.contains(text),
                "fresh window title comparison".to_owned(),
            ),
            StatePredicate::BoundsContained { inner, outer } => (
                contains_rect(*outer, *inner),
                "screen-bounds comparison".to_owned(),
            ),
            StatePredicate::ElementExists { role, name } => {
                let snapshot = self
                    .semantic
                    .snapshot(current.hwnd, AccessibilityMode::Interactive)
                    .await?;
                (
                    snapshot.elements.iter().any(|element| {
                        role.as_ref().is_none_or(|role| role == &element.role)
                            && name.as_ref().is_none_or(|name| name == &element.name)
                    }),
                    "fresh cached UI Automation snapshot".to_owned(),
                )
            }
            StatePredicate::ElementState {
                role,
                name,
                state,
                value,
            } => {
                let snapshot = self
                    .semantic
                    .snapshot(current.hwnd, AccessibilityMode::Interactive)
                    .await?;
                (
                    snapshot.elements.iter().any(|element| {
                        role.as_ref().is_none_or(|role| role == &element.role)
                            && name.as_ref().is_none_or(|name| name == &element.name)
                            && match state {
                                ElementState::Enabled => element.enabled == *value,
                                ElementState::Focused => element.focused == *value,
                                ElementState::Selected => element.selected == Some(*value),
                                ElementState::Checked => element.checked == Some(*value),
                                ElementState::Expanded => element.expanded == Some(*value),
                            }
                    }),
                    "fresh cached UI Automation state comparison".to_owned(),
                )
            }
        };
        Ok(DriverVerification { matched, evidence })
    }
}

impl WindowsDriver {
    #[allow(clippy::too_many_lines)]
    async fn dispatch_action(
        &self,
        current: &NativeWindow,
        action: DriverAction,
        restore_foreground: bool,
    ) -> Result<DriverActionOutput, DriverError> {
        let (delivery_mode, effect_status, evidence_kind, foreground_restoration) = match action {
            DriverAction::FocusWindow => {
                self.focus_for_input(current).await?;
                (
                    DeliveryMode::Foreground,
                    ActionEffectStatus::Confirmed,
                    ActionEvidenceKind::NativeReadback,
                    ForegroundRestorationStatus::NotRequested,
                )
            }
            DriverAction::SetWindowBounds { bounds } => {
                self.input
                    .perform(current.hwnd, InputAction::SetWindowBounds(bounds))
                    .await?;
                (
                    DeliveryMode::System,
                    ActionEffectStatus::Confirmed,
                    ActionEvidenceKind::NativeReadback,
                    ForegroundRestorationStatus::NotRequested,
                )
            }
            DriverAction::SetWindowMinimized { minimized } => {
                self.input
                    .perform(current.hwnd, InputAction::SetWindowMinimized(minimized))
                    .await?;
                (
                    DeliveryMode::System,
                    ActionEffectStatus::Confirmed,
                    ActionEvidenceKind::NativeReadback,
                    ForegroundRestorationStatus::NotRequested,
                )
            }
            DriverAction::TerminateApplication => {
                let windows = self
                    .windows()
                    .await?
                    .into_iter()
                    .filter(|window| window.application_key == current.application_key)
                    .map(|window| window.hwnd)
                    .collect();
                self.input
                    .perform(
                        current.hwnd,
                        InputAction::TerminateApplication {
                            pid: current.pid,
                            process_generation: current.process_generation,
                            windows,
                        },
                    )
                    .await?;
                (
                    DeliveryMode::System,
                    ActionEffectStatus::Confirmed,
                    ActionEvidenceKind::NativeReadback,
                    ForegroundRestorationStatus::NotRequested,
                )
            }
            background_click @ DriverAction::ClickPoint {
                delivery: DeliveryPreference::Background,
                ..
            } => {
                self.dispatch_input_action(current.hwnd, background_click)
                    .await?;
                (
                    DeliveryMode::Background,
                    ActionEffectStatus::Unverified,
                    ActionEvidenceKind::DispatchAcceptance,
                    ForegroundRestorationStatus::NotRequested,
                )
            }
            semantic_action @ (DriverAction::FocusElement { .. }
            | DriverAction::InvokeElement { .. }
            | DriverAction::SetValue { .. }
            | DriverAction::SetRangeValue { .. }
            | DriverAction::ToggleElement { .. }
            | DriverAction::SelectElement { .. }
            | DriverAction::SetExpanded { .. }) => {
                self.dispatch_semantic_action(semantic_action).await?;
                (
                    DeliveryMode::Semantic,
                    ActionEffectStatus::Unverified,
                    ActionEvidenceKind::DispatchAcceptance,
                    ForegroundRestorationStatus::NotRequested,
                )
            }
            input_action => {
                let previous = if restore_foreground {
                    self.input.foreground_owner().await?
                } else {
                    None
                };
                let action = self.resolve_input_action(input_action).await?;
                let foreground_restoration = self
                    .input
                    .perform_foreground(
                        current.hwnd,
                        current.pid,
                        current.process_generation,
                        action,
                        restore_foreground,
                        previous,
                    )
                    .await?;
                (
                    DeliveryMode::Foreground,
                    ActionEffectStatus::Unverified,
                    ActionEvidenceKind::DispatchAcceptance,
                    foreground_restoration,
                )
            }
        };
        Ok(DriverActionOutput {
            delivery_mode,
            effect_status,
            evidence_kind,
            foreground_restoration,
        })
    }

    async fn dispatch_semantic_action(&self, action: DriverAction) -> Result<(), DriverError> {
        let (element_key, action) = match action {
            DriverAction::FocusElement { element_key } => (element_key, SemanticAction::Focus),
            DriverAction::InvokeElement { element_key } => (element_key, SemanticAction::Invoke),
            DriverAction::SetValue { element_key, value } => {
                (element_key, SemanticAction::SetValue(value))
            }
            DriverAction::SetRangeValue { element_key, value } => {
                (element_key, SemanticAction::SetRangeValue(value))
            }
            DriverAction::ToggleElement { element_key } => (element_key, SemanticAction::Toggle),
            DriverAction::SelectElement { element_key } => (element_key, SemanticAction::Select),
            DriverAction::SetExpanded {
                element_key,
                expanded,
            } => (element_key, SemanticAction::SetExpanded(expanded)),
            _ => {
                return Err(DriverError::new(
                    DriverErrorKind::Unsupported,
                    "action is not a semantic operation",
                )
                .mutation_not_dispatched());
            }
        };
        self.semantic.perform(element_key, action).await
    }

    async fn dispatch_input_action(
        &self,
        hwnd: isize,
        action: DriverAction,
    ) -> Result<(), DriverError> {
        let action = self.resolve_input_action(action).await?;
        self.input.perform(hwnd, action).await
    }

    async fn resolve_input_action(&self, action: DriverAction) -> Result<InputAction, DriverError> {
        let action = match action {
            DriverAction::ClickPoint {
                point,
                button,
                count,
                delivery,
            } => InputAction::Click {
                point,
                button,
                count,
                background: delivery == DeliveryPreference::Background,
            },
            DriverAction::PressPoint {
                point,
                button,
                duration_ms,
            } => InputAction::Press {
                point,
                button,
                duration_ms,
            },
            DriverAction::MovePointer { point, duration_ms } => {
                InputAction::Move { point, duration_ms }
            }
            DriverAction::TypeText { text } => InputAction::TypeText(text),
            DriverAction::PressKeys { keys } => InputAction::PressKeys(keys),
            DriverAction::Scroll {
                element_key,
                delta_x,
                delta_y,
            } => {
                if let Some(element_key) = element_key {
                    self.semantic
                        .perform(element_key, SemanticAction::Focus)
                        .await?;
                }
                InputAction::Scroll { delta_x, delta_y }
            }
            DriverAction::Drag {
                from,
                to,
                duration_ms,
            } => InputAction::Drag {
                from,
                to,
                duration_ms,
            },
            _ => {
                return Err(DriverError::new(
                    DriverErrorKind::Unsupported,
                    "action is not a foreground input operation",
                )
                .mutation_not_dispatched());
            }
        };
        Ok(action)
    }
}

fn driver_application(window: &NativeWindow) -> DriverApplication {
    DriverApplication {
        key: window.application_key.clone(),
        process_generation: window.application_key.clone(),
        identity: window.application_id.clone(),
        name: window.application_name.clone(),
        application_id: window.application_id.clone(),
        foreground: window.foreground,
        provenance: ApplicationProvenance::Windows {
            executable_path: window.application_id.clone(),
            publisher: None,
            signature_status: SignatureStatus::Unknown,
        },
    }
}

fn discovered_application(window: &NativeWindow) -> DriverApplication {
    let mut application = driver_application(window);
    let (publisher, signature_status) = provenance::inspect(&window.application_id);
    application.provenance = ApplicationProvenance::Windows {
        executable_path: window.application_id.clone(),
        publisher,
        signature_status,
    };
    application
}

fn driver_window(window: NativeWindow) -> DriverWindow {
    let application = driver_application(&window);
    DriverWindow {
        key: window.key,
        application,
        title: window.title,
        screen_bounds: window.screen_bounds,
        minimized: window.minimized,
        visible: window.visible,
        foreground: window.foreground,
    }
}

fn current_window(windows: &[NativeWindow], key: &str) -> Result<NativeWindow, DriverError> {
    windows
        .iter()
        .find(|window| window.key == key)
        .cloned()
        .ok_or_else(|| {
            DriverError::new(
                DriverErrorKind::TargetUnavailable,
                "target window generation is unavailable",
            )
            .retryable("list_windows")
        })
}
