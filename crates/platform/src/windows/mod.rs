//! Windows Graphics Capture, UI Automation, and SendInput driver.

mod capture;
mod discovery;
mod input;
mod semantic;

use std::collections::HashMap;

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, ActionKind, CaptureMode, DeliveryMode, DriverCapabilities, InputRoute,
    PROTOCOL_VERSION, PermissionState, PermissionStatus, Platform, StatePredicate,
};
use nexus_cua_runtime::{
    DesktopDriver, DriverAction, DriverActionOutput, DriverApplication, DriverError,
    DriverErrorKind, DriverObservation, DriverVerification, DriverWindow,
};
use sha2::{Digest, Sha256};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use capture::CaptureActor;
use discovery::{DiscoveryActor, NativeWindow};
use input::{InputAction, InputActor};
use semantic::{SemanticAction, SemanticActor};

/// Windows native driver with one actor for each native threading domain.
pub struct WindowsDriver {
    discovery: DiscoveryActor,
    capture: CaptureActor,
    semantic: SemanticActor,
    input: InputActor,
}

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
            screen_capture: PermissionState::Granted,
            accessibility: PermissionState::Granted,
            input_control: PermissionState::Granted,
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
                self.capture
                    .capture(before.hwnd, before.screen_bounds)
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
        let fingerprint = fingerprint(&after, captured.as_ref().map(|(image, _bounds)| image));
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
                .capture(current.hwnd, current.screen_bounds)
                .await?;
            if bounds != current.screen_bounds {
                return Ok(false);
            }
            Ok(fingerprints_match(
                fingerprint_value,
                &fingerprint(&current, Some(&image)),
            ))
        } else {
            Ok(fingerprint(&current, None) == fingerprint_value)
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
        let delivery_mode = match action {
            DriverAction::FocusWindow => {
                self.focus_for_input(&current).await?;
                DeliveryMode::Foreground
            }
            DriverAction::FocusElement { element_key } => {
                self.semantic
                    .perform(element_key, SemanticAction::Focus)
                    .await?;
                DeliveryMode::Semantic
            }
            DriverAction::InvokeElement { element_key } => {
                self.semantic
                    .perform(element_key, SemanticAction::Invoke)
                    .await?;
                DeliveryMode::Semantic
            }
            DriverAction::SetValue { element_key, value } => {
                self.semantic
                    .perform(element_key, SemanticAction::SetValue(value))
                    .await?;
                DeliveryMode::Semantic
            }
            DriverAction::ToggleElement { element_key } => {
                self.semantic
                    .perform(element_key, SemanticAction::Toggle)
                    .await?;
                DeliveryMode::Semantic
            }
            DriverAction::SelectElement { element_key } => {
                self.semantic
                    .perform(element_key, SemanticAction::Select)
                    .await?;
                DeliveryMode::Semantic
            }
            DriverAction::SetExpanded {
                element_key,
                expanded,
            } => {
                self.semantic
                    .perform(element_key, SemanticAction::SetExpanded(expanded))
                    .await?;
                DeliveryMode::Semantic
            }
            DriverAction::ClickPoint {
                point,
                button,
                count,
            } => {
                self.focus_for_input(&current).await?;
                self.input
                    .perform(
                        current.hwnd,
                        InputAction::Click {
                            point,
                            button,
                            count,
                        },
                    )
                    .await?;
                DeliveryMode::Foreground
            }
            DriverAction::MovePointer { point, duration_ms } => {
                self.focus_for_input(&current).await?;
                self.input
                    .perform(current.hwnd, InputAction::Move { point, duration_ms })
                    .await?;
                DeliveryMode::Foreground
            }
            DriverAction::TypeText { text } => {
                self.focus_for_input(&current).await?;
                self.input
                    .perform(current.hwnd, InputAction::TypeText(text))
                    .await?;
                DeliveryMode::Foreground
            }
            DriverAction::PressKeys { keys } => {
                self.focus_for_input(&current).await?;
                self.input
                    .perform(current.hwnd, InputAction::PressKeys(keys))
                    .await?;
                DeliveryMode::Foreground
            }
            DriverAction::Scroll {
                element_key,
                delta_x,
                delta_y,
            } => {
                self.focus_for_input(&current).await?;
                if let Some(element_key) = element_key {
                    self.semantic
                        .perform(element_key, SemanticAction::Focus)
                        .await?;
                }
                self.input
                    .perform(current.hwnd, InputAction::Scroll { delta_x, delta_y })
                    .await?;
                DeliveryMode::Foreground
            }
            DriverAction::Drag {
                from,
                to,
                duration_ms,
            } => {
                self.focus_for_input(&current).await?;
                self.input
                    .perform(
                        current.hwnd,
                        InputAction::Drag {
                            from,
                            to,
                            duration_ms,
                        },
                    )
                    .await?;
                DeliveryMode::Foreground
            }
        };
        Ok(DriverActionOutput { delivery_mode })
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

fn fingerprint(window: &NativeWindow, image: Option<&nexus_cua_runtime::RgbaImage>) -> String {
    let mut digest = Sha256::new();
    digest.update(window.key.as_bytes());
    digest.update(window.screen_bounds.x.to_bits().to_be_bytes());
    digest.update(window.screen_bounds.y.to_bits().to_be_bytes());
    digest.update(window.screen_bounds.width.to_bits().to_be_bytes());
    digest.update(window.screen_bounds.height.to_bits().to_be_bytes());
    let geometry = hex::encode(digest.finalize());
    image.map_or_else(
        || format!("geometry:{geometry}"),
        |image| {
            format!(
                "geometry:{geometry}:visual:{}",
                hex::encode(visual_signature(image))
            )
        },
    )
}

fn visual_signature(image: &nexus_cua_runtime::RgbaImage) -> Vec<u8> {
    const GRID: u32 = 32;
    if image.width == 0 || image.height == 0 {
        return Vec::new();
    }
    let mut signature = Vec::with_capacity((GRID * GRID) as usize);
    for grid_y in 0..GRID {
        let y = ((u64::from(grid_y) * u64::from(image.height) + u64::from(GRID / 2))
            / u64::from(GRID))
        .min(u64::from(image.height - 1));
        for grid_x in 0..GRID {
            let x = ((u64::from(grid_x) * u64::from(image.width) + u64::from(GRID / 2))
                / u64::from(GRID))
            .min(u64::from(image.width - 1));
            let index = usize::try_from((y * u64::from(image.width) + x) * 4).unwrap_or(0);
            let pixel = image.pixels.get(index..index + 3).unwrap_or(&[0, 0, 0]);
            let luma =
                (u16::from(pixel[0]) * 54 + u16::from(pixel[1]) * 183 + u16::from(pixel[2]) * 19)
                    / 256;
            signature.push(u8::try_from(luma).unwrap_or(u8::MAX));
        }
    }
    signature
}

fn fingerprints_match(expected: &str, current: &str) -> bool {
    let Some((expected_geometry, expected_visual)) = expected.split_once(":visual:") else {
        return expected == current;
    };
    let Some((current_geometry, current_visual)) = current.split_once(":visual:") else {
        return false;
    };
    if expected_geometry != current_geometry {
        return false;
    }
    let (Ok(expected), Ok(current)) = (hex::decode(expected_visual), hex::decode(current_visual))
    else {
        return false;
    };
    if expected.is_empty() || expected.len() != current.len() {
        return false;
    }
    let mut total_difference = 0_u64;
    let mut material_changes = 0_usize;
    for (expected, current) in expected.iter().zip(&current) {
        let difference = expected.abs_diff(*current);
        total_difference += u64::from(difference);
        material_changes += usize::from(difference > 24);
    }
    let mean_difference = total_difference as f64 / expected.len() as f64;
    mean_difference <= 8.0 && material_changes * 5 <= expected.len()
}

fn contains_rect(
    outer: nexus_cua_protocol::ScreenRect,
    inner: nexus_cua_protocol::ScreenRect,
) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width <= outer.x + outer.width
        && inner.y + inner.height <= outer.y + outer.height
}
