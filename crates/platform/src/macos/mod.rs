//! macOS `ScreenCaptureKit`, `AXUIElement`, and `CGEvent` driver.

mod capture;
mod input;
mod semantic;

use std::collections::HashMap;

use async_trait::async_trait;
use core_graphics::access::ScreenCaptureAccess;
use nexus_cua_protocol::{
    AccessibilityMode, ActionKind, CaptureMode, DeliveryMode, DriverCapabilities, InputRoute,
    PROTOCOL_VERSION, PermissionState, PermissionStatus, Platform, StatePredicate,
};
use nexus_cua_runtime::{
    DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverError,
    DriverErrorKind, DriverObservation, DriverVerification, DriverWindow,
};

use capture::{CaptureActor, NativeWindow};
use input::{InputAction, InputActor};
use semantic::{SemanticAction, SemanticActor};

use crate::observation::{contains_rect, fingerprint, fingerprints_match};

/// macOS native driver. Platform actors are initialized lazily on first use.
pub struct MacosDriver {
    capture: CaptureActor,
    semantic: SemanticActor,
    input: InputActor,
}

impl MacosDriver {
    /// Creates a driver without prompting for operating-system permissions.
    pub fn new() -> Result<Self, DriverError> {
        Ok(Self {
            capture: CaptureActor::spawn(),
            semantic: SemanticActor::spawn(),
            input: InputActor::spawn()?,
        })
    }

    async fn windows(&self) -> Result<Vec<NativeWindow>, DriverError> {
        self.capture.list_windows().await
    }
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
            input_control: if accessibility {
                PermissionState::Granted
            } else {
                PermissionState::Denied
            },
        })
    }

    async fn list_applications(&self) -> Result<Vec<DriverApplication>, DriverError> {
        let mut applications = HashMap::new();
        for window in self.windows().await? {
            applications
                .entry(window.application_key.clone())
                .or_insert_with(|| driver_application(&window));
        }
        Ok(applications.into_values().collect())
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
        let capture = async {
            if include_screenshot {
                self.capture.capture(window.key.clone()).await.map(Some)
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
            let (image, captured_bounds) = self.capture.capture(window.key.clone()).await?;
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
            ));
        }
        let current = current_window(&self.windows().await?, &window.key)?;
        self.dispatch_action(&current, action).await
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
                self.input
                    .perform(current.pid, InputAction::Activate)
                    .await?;
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
                ));
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
                ));
            }
        };
        self.input.perform(pid, action).await
    }

    async fn focus_for_input(&self, window: &NativeWindow) -> Result<(), DriverError> {
        self.input
            .perform(window.pid, InputAction::Activate)
            .await?;
        self.semantic
            .raise_window(window.pid, window.screen_bounds, window.title.clone())
            .await
    }
}

fn driver_application(window: &NativeWindow) -> DriverApplication {
    DriverApplication {
        key: window.application_key.clone(),
        name: window.application_name.clone(),
        application_id: window.application_id.clone(),
        foreground: window.foreground,
    }
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
