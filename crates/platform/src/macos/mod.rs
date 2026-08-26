//! macOS `ScreenCaptureKit`, `AXUIElement`, and `CGEvent` driver.

mod capture;
mod input;
mod provenance;
mod semantic;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use core_graphics::access::ScreenCaptureAccess;
use nexus_cua_protocol::{
    AccessibilityMode, ActionKind, ApplicationProvenance, CaptureMode, DeliveryMode,
    DriverCapabilities, InputRoute, PROTOCOL_VERSION, PermissionState, PermissionStatus, Platform,
    StatePredicate,
};
use nexus_cua_runtime::{
    DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverError,
    DriverErrorKind, DriverObservation, DriverVerification, DriverWindow,
};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_core_graphics::CGPreflightPostEventAccess;

use capture::{CaptureActor, NativeWindow};
use input::{InputAction, InputActor};
use semantic::{SemanticAction, SemanticActor};
use tokio::sync::Semaphore;

use crate::observation::{contains_rect, fingerprint, fingerprints_match};

/// macOS native driver. Platform actors are initialized lazily on first use.
pub struct MacosDriver {
    capture: CaptureActor,
    semantic: SemanticActor,
    input: InputActor,
    provenance_workers: Arc<Semaphore>,
    provenance_cache: Arc<Mutex<provenance::Cache>>,
}

const PROVENANCE_WORKERS: usize = 2;

impl MacosDriver {
    /// Creates a driver without prompting for operating-system permissions.
    pub fn new() -> Result<Self, DriverError> {
        initialize_appkit()?;
        Ok(Self {
            capture: CaptureActor::spawn(),
            semantic: SemanticActor::spawn(),
            input: InputActor::spawn()?,
            provenance_workers: Arc::new(Semaphore::new(PROVENANCE_WORKERS)),
            provenance_cache: Arc::new(Mutex::new(provenance::Cache::default())),
        })
    }

    async fn windows(&self) -> Result<Vec<NativeWindow>, DriverError> {
        if !ScreenCaptureAccess.preflight() {
            return Err(DriverError::new(
                DriverErrorKind::PermissionRequired,
                "macOS screen-recording permission is required for trusted-host discovery",
            )
            .retryable("grant_screen_recording_permission"));
        }
        self.capture.list_windows().await
    }

    async fn current_window(&self, key: &str) -> Result<NativeWindow, DriverError> {
        if !ScreenCaptureAccess.preflight() {
            return Err(DriverError::new(
                DriverErrorKind::PermissionRequired,
                "macOS screen-recording permission is required for target validation",
            )
            .retryable("grant_screen_recording_permission"));
        }
        self.capture.resolve_window(key.to_owned()).await
    }
}

fn initialize_appkit() -> Result<(), DriverError> {
    let main_thread = MainThreadMarker::new().ok_or_else(|| {
        DriverError::new(
            DriverErrorKind::Platform,
            "macOS driver must be created on the process main thread",
        )
    })?;
    let application = NSApplication::sharedApplication(main_thread);
    application.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    Ok(())
}

#[async_trait]
impl DesktopDriver for MacosDriver {
    async fn capabilities(&self) -> Result<DriverCapabilities, DriverError> {
        Ok(DriverCapabilities {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: Platform::Macos,
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
        let accessibility = macos_accessibility_client::accessibility::application_is_trusted();
        Ok(PermissionStatus {
            screen_capture: if ScreenCaptureAccess.preflight() {
                PermissionState::Granted
            } else {
                PermissionState::Unknown
            },
            accessibility: if accessibility {
                PermissionState::Granted
            } else {
                PermissionState::Denied
            },
            input_control: if CGPreflightPostEventAccess() {
                PermissionState::Granted
            } else {
                PermissionState::Denied
            },
        })
    }

    async fn list_applications(&self) -> Result<Vec<DriverApplication>, DriverError> {
        let windows = self.windows().await?;
        let permit = Arc::clone(&self.provenance_workers)
            .try_acquire_owned()
            .map_err(|_| {
                DriverError::new(
                    DriverErrorKind::Busy,
                    "macOS provenance worker capacity is exhausted",
                )
                .retryable("retry_with_backoff")
            })?;
        let provenance_cache = Arc::clone(&self.provenance_cache);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut cache = provenance_cache.lock().map_err(|_| {
                DriverError::new(
                    DriverErrorKind::Platform,
                    "macOS provenance cache is unavailable",
                )
            })?;
            let mut applications = HashMap::new();
            for window in windows {
                applications
                    .entry(window.application_key.clone())
                    .or_insert_with(|| discovered_application(&window, &mut cache));
            }
            Ok(applications.into_values().collect())
        })
        .await
        .map_err(|_| {
            DriverError::new(
                DriverErrorKind::Platform,
                "macOS provenance worker stopped unexpectedly",
            )
        })?
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
        if include_screenshot && !ScreenCaptureAccess.preflight() {
            return Err(DriverError::new(
                DriverErrorKind::PermissionRequired,
                "macOS screen-recording permission is required",
            )
            .retryable("grant_screen_recording_permission"));
        }
        let before = self.current_window(&window.key).await?;
        if include_screenshot && !before.visible {
            return Err(DriverError::new(
                DriverErrorKind::TargetUnavailable,
                "minimized macOS target cannot provide a current exact-window frame",
            )
            .retryable("restore_window_then_observe"));
        }
        let capture = async {
            if include_screenshot {
                self.capture
                    .capture(window.key.clone(), before.screen_bounds)
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
                    .snapshot(
                        before.pid,
                        before.screen_bounds,
                        before.title.clone(),
                        accessibility,
                    )
                    .await
                    .map(Some)
            }
        };
        let (captured, semantics) = tokio::join!(capture, semantics);
        let captured = captured?;
        let semantics = semantics?;
        let after = self.current_window(&window.key).await?;
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
        let current = self.current_window(&window.key).await?;
        if fingerprint_value.contains(":visual:") {
            let (image, captured_bounds) = self
                .capture
                .capture(window.key.clone(), current.screen_bounds)
                .await?;
            if captured_bounds != current.screen_bounds {
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
        if foreground && !CGPreflightPostEventAccess() {
            return Err(DriverError::new(
                DriverErrorKind::PermissionRequired,
                "macOS event-synthesis permission is required",
            )
            .mutation_not_dispatched());
        }
        let current = self
            .current_window(&window.key)
            .await
            .map_err(DriverError::mutation_not_dispatched)?;
        self.dispatch_action(&current, action).await
    }

    async fn verify_state(
        &self,
        window: &DriverWindow,
        predicate: &StatePredicate,
    ) -> Result<DriverVerification, DriverError> {
        let current = self.current_window(&window.key).await?;
        let (matched, evidence) = match predicate {
            StatePredicate::WindowTitleContains { text } => (
                current.title.contains(text),
                "fresh window title comparison".to_owned(),
            ),
            StatePredicate::BoundsContained { inner, outer } => (
                contains_rect(*outer, *inner),
                "logical screen-bounds comparison".to_owned(),
            ),
            StatePredicate::ElementExists { role, name } => {
                let snapshot = self
                    .semantic
                    .snapshot(
                        current.pid,
                        current.screen_bounds,
                        current.title.clone(),
                        AccessibilityMode::Interactive,
                    )
                    .await?;
                (
                    snapshot.elements.iter().any(|element| {
                        role.as_ref().is_none_or(|role| role == &element.role)
                            && name.as_ref().is_none_or(|name| name == &element.name)
                    }),
                    "fresh bounded accessibility snapshot".to_owned(),
                )
            }
        };
        Ok(DriverVerification { matched, evidence })
    }
}

impl MacosDriver {
    async fn dispatch_action(
        &self,
        current: &NativeWindow,
        action: DriverAction,
    ) -> Result<DriverActionOutput, DriverError> {
        let delivery_mode = match action {
            DriverAction::FocusWindow => {
                self.semantic
                    .raise_window(current.pid, current.screen_bounds, current.title.clone())
                    .await?;
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
                self.dispatch_input_action(current.pid, input_action)
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
        pid: i32,
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
        self.input.perform(pid, action).await
    }

    async fn focus_for_input(&self, window: &NativeWindow) -> Result<(), DriverError> {
        self.semantic
            .raise_window(window.pid, window.screen_bounds, window.title.clone())
            .await
            .map_err(DriverError::mutation_not_dispatched)
    }
}

fn driver_application(window: &NativeWindow) -> DriverApplication {
    DriverApplication {
        key: window.application_key.clone(),
        process_generation: window.application_key.clone(),
        identity: format!(
            "bundle:{}:executable:{}",
            window.bundle_id.as_deref().unwrap_or(""),
            window.executable_path.as_deref().unwrap_or("")
        ),
        name: window.application_name.clone(),
        application_id: window.application_id.clone(),
        foreground: window.foreground,
        provenance: ApplicationProvenance::Macos {
            bundle_id: window.bundle_id.clone(),
            executable_path: window.executable_path.clone(),
            signing_team_id: None,
            designated_requirement: None,
        },
    }
}

fn discovered_application(
    window: &NativeWindow,
    cache: &mut provenance::Cache,
) -> DriverApplication {
    let mut application = driver_application(window);
    let signing = window
        .executable_path
        .as_deref()
        .map(|path| cache.inspect(&window.application_key, path))
        .unwrap_or_default();
    application.provenance = ApplicationProvenance::Macos {
        bundle_id: window.bundle_id.clone(),
        executable_path: window.executable_path.clone(),
        signing_team_id: signing.team_id,
        designated_requirement: signing.designated_requirement,
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
        minimized: !window.visible,
        visible: window.visible,
        foreground: window.foreground,
    }
}
