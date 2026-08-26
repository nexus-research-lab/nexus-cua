//! Windows Graphics Capture, UI Automation, and `SendInput` driver.

mod capture;
mod discovery;
mod input;
mod provenance;
mod semantic;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, ActionKind, ApplicationProvenance, CaptureMode, DeliveryMode,
    DriverCapabilities, InputRoute, PROTOCOL_VERSION, PermissionState, PermissionStatus, Platform,
    SignatureStatus, StatePredicate,
};
use nexus_cua_runtime::{
    DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverError,
    DriverErrorKind, DriverObservation, DriverVerification, DriverWindow,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use capture::CaptureActor;
use discovery::{DiscoveryActor, NativeWindow};
use input::{InputAction, InputActor};
use semantic::{SemanticAction, SemanticActor};
use tokio::sync::Semaphore;

use crate::observation::{contains_rect, fingerprint, fingerprints_match};

/// Windows native driver with one actor for each native threading domain.
pub struct WindowsDriver {
    discovery: DiscoveryActor,
    capture: CaptureActor,
    semantic: SemanticActor,
    input: InputActor,
    provenance_workers: Arc<Semaphore>,
}

const PROVENANCE_WORKERS: usize = 2;

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
            capture_modes: vec![CaptureMode::Window],
            accessibility_tree: true,
            input_routes: vec![InputRoute::Semantic, InputRoute::Foreground],
            actions: vec![
                ActionKind::FocusWindow,
                ActionKind::FocusElement,
                ActionKind::InvokeElement,
                ActionKind::ClickPoint,
                ActionKind::SetValue,
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
    ) -> Result<DriverActionOutput, DriverError> {
        let foreground = matches!(
            &action,
            DriverAction::FocusWindow
                | DriverAction::ClickPoint { .. }
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
        let result = self.dispatch_action(&current, action).await;
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
        };
        Ok(DriverVerification { matched, evidence })
    }
}

impl WindowsDriver {
    async fn dispatch_action(
        &self,
        current: &NativeWindow,
        action: DriverAction,
    ) -> Result<DriverActionOutput, DriverError> {
        let delivery_mode = match action {
            DriverAction::FocusWindow => {
                self.focus_for_input(current).await?;
                DeliveryMode::Foreground
            }
            semantic_action @ (DriverAction::FocusElement { .. }
            | DriverAction::InvokeElement { .. }
            | DriverAction::SetValue { .. }
            | DriverAction::ToggleElement { .. }
            | DriverAction::SelectElement { .. }
            | DriverAction::SetExpanded { .. }) => {
                self.dispatch_semantic_action(semantic_action).await?;
                DeliveryMode::Semantic
            }
            input_action => {
                self.focus_for_input(current).await?;
                self.dispatch_input_action(current.hwnd, input_action)
                    .await?;
                DeliveryMode::Foreground
            }
        };
        Ok(DriverActionOutput { delivery_mode })
    }

    async fn dispatch_semantic_action(&self, action: DriverAction) -> Result<(), DriverError> {
        let (element_key, action) = match action {
            DriverAction::FocusElement { element_key } => (element_key, SemanticAction::Focus),
            DriverAction::InvokeElement { element_key } => (element_key, SemanticAction::Invoke),
            DriverAction::SetValue { element_key, value } => {
                (element_key, SemanticAction::SetValue(value))
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
        let action = match action {
            DriverAction::ClickPoint {
                point,
                button,
                count,
            } => InputAction::Click {
                point,
                button,
                count,
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
        self.input.perform(hwnd, action).await
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
