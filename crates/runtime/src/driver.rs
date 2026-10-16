//! Narrow internal contract implemented by platform drivers.

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, AccessibilityRangeValue, ActionEffectStatus, ActionEvidenceKind,
    ApplicationProvenance, DeliveryMode, DeliveryPreference, DriverCapabilities,
    ForegroundRestorationStatus, ObservationTruncation, PermissionStatus, PointerButton,
    ScreenPoint, ScreenRect, SensitiveText, StatePredicate,
};
use zeroize::Zeroize;

use crate::DriverError;

/// Internal application identity owned by a platform driver.
#[derive(Clone, Debug)]
pub struct DriverApplication {
    /// Stable key within the lifetime of one driver process.
    pub key: String,
    /// Platform process generation, such as launch time plus process identity.
    pub process_generation: String,
    /// Normalized executable or bundle identity used for TOCTOU revalidation.
    pub identity: String,
    /// Human-readable display name.
    pub name: String,
    /// Bundle identifier or executable identity matched by manifests.
    pub application_id: String,
    /// Whether the application currently owns foreground input.
    pub foreground: bool,
    /// Public best-effort provenance summary without native authority handles.
    pub provenance: ApplicationProvenance,
}

/// Internal installed-application identity owned by a platform driver.
#[derive(Clone, Debug)]
pub struct DriverLaunchableApplication {
    /// Stable native registration key, never exposed as public authority.
    pub key: String,
    /// Driver-owned native launch target; public callers never supply it.
    pub launch_target: String,
    /// Normalized executable/bundle identity used for revalidation.
    pub identity: String,
    /// Human-readable display name.
    pub name: String,
    /// Stable bundle, package, or executable identity.
    pub application_id: String,
    /// Public best-effort provenance summary.
    pub provenance: ApplicationProvenance,
}

impl DriverLaunchableApplication {
    /// Returns true only when native rediscovery produced the same installed identity.
    pub fn matches_identity(&self, current: &Self) -> bool {
        self.key == current.key
            && self.launch_target == current.launch_target
            && self.identity == current.identity
            && self.application_id == current.application_id
            && self.provenance == current.provenance
    }
}

/// Native cold-launch result.
pub struct DriverLaunchOutput {
    /// Newly started process generation.
    pub application: DriverApplication,
    /// Native delivery route used by launch.
    pub delivery_mode: DeliveryMode,
}

impl DriverApplication {
    /// Returns true only for the same running process generation and identity.
    pub fn matches_generation(&self, current: &Self) -> bool {
        self.key == current.key
            && self.process_generation == current.process_generation
            && self.identity == current.identity
            && self.application_id == current.application_id
    }
}

/// Internal top-level window identity owned by a platform driver.
#[derive(Clone, Debug)]
pub struct DriverWindow {
    /// Stable key within the lifetime of one driver process.
    pub key: String,
    /// Owning application.
    pub application: DriverApplication,
    /// Current title.
    pub title: String,
    /// Logical top-left screen bounds.
    pub screen_bounds: ScreenRect,
    /// Whether the window is minimized.
    pub minimized: bool,
    /// Whether the window is visible on a display.
    pub visible: bool,
    /// Whether the window owns foreground input.
    pub foreground: bool,
}

/// Internal physical-display identity owned by a platform driver.
#[derive(Clone, Debug)]
pub struct DriverDisplay {
    /// Stable key within the lifetime of one driver process.
    pub key: String,
    /// Logical top-left bounds in the multi-display desktop coordinate space.
    pub screen_bounds: ScreenRect,
    /// Whether this display owns the operating-system primary origin.
    pub primary: bool,
}

/// Raw RGBA screenshot returned to the runtime for validated PNG encoding.
pub struct RgbaImage {
    /// Physical pixel width.
    pub width: u32,
    /// Physical pixel height.
    pub height: u32,
    /// Row-major RGBA8 pixels.
    pub pixels: Vec<u8>,
    recycler: Option<Box<dyn FnOnce(Vec<u8>) + Send + 'static>>,
}

impl RgbaImage {
    /// Creates an image whose pixel storage is released normally.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels,
            recycler: None,
        }
    }

    /// Creates an image whose storage returns to a bounded driver-owned pool.
    pub fn with_recycler(
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        recycler: impl FnOnce(Vec<u8>) + Send + 'static,
    ) -> Self {
        Self {
            width,
            height,
            pixels,
            recycler: Some(Box::new(recycler)),
        }
    }
}

impl Drop for RgbaImage {
    fn drop(&mut self) {
        self.pixels.zeroize();
        if let Some(recycler) = self.recycler.take() {
            recycler(std::mem::take(&mut self.pixels));
        }
    }
}

/// Internal accessibility element with a driver-owned key.
#[derive(Clone, Debug)]
pub struct DriverElement {
    /// Stable only within the containing driver observation.
    pub key: String,
    /// Optional parent key from the same observation.
    pub parent_key: Option<String>,
    /// Normalized role.
    pub role: String,
    /// Accessible label.
    pub name: String,
    /// Non-sensitive display value when available.
    pub value: Option<String>,
    /// Numeric range metadata when the native provider exposes it coherently.
    pub range_value: Option<AccessibilityRangeValue>,
    /// Logical top-left screen bounds.
    pub screen_bounds: Option<ScreenRect>,
    /// Whether actions are currently accepted.
    pub enabled: bool,
    /// Whether the element owns keyboard focus.
    pub focused: bool,
    /// Whether the platform reports the element as selected.
    pub selected: Option<bool>,
    /// Whether the platform reports the element as checked.
    pub checked: Option<bool>,
    /// Whether the platform reports the element as expanded.
    pub expanded: Option<bool>,
    /// Normalized semantic actions.
    pub actions: Vec<String>,
}

/// Immutable platform observation before public reference projection.
pub struct DriverObservation {
    /// Exact driver window key.
    pub window_key: String,
    /// Logical window bounds.
    pub window_screen_bounds: ScreenRect,
    /// Optional raw image.
    pub screenshot: Option<RgbaImage>,
    /// Logical screen rectangle represented by the raw image.
    pub screenshot_screen_bounds: Option<ScreenRect>,
    /// Normalized elements.
    pub elements: Vec<DriverElement>,
    /// False when the platform tree is partial.
    pub elements_complete: bool,
    /// Stable reason when the semantic tree is partial.
    pub elements_truncation: Option<ObservationTruncation>,
    /// Driver-computed opaque state fingerprint.
    pub fingerprint: String,
}

/// Resolved action containing only driver-owned target keys.
pub enum DriverAction {
    /// Bring the target window to the foreground.
    FocusWindow,
    /// Move and resize the exact target window.
    SetWindowBounds {
        /// Requested logical top-left screen bounds.
        bounds: ScreenRect,
    },
    /// Minimize or restore the exact target window.
    SetWindowMinimized {
        /// Requested minimized state.
        minimized: bool,
    },
    /// Cooperatively terminate the exact application process generation.
    TerminateApplication,
    /// Move semantic focus to one accessibility element.
    FocusElement {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
    },
    /// Invoke one accessibility element.
    InvokeElement {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
    },
    /// Click a runtime-resolved logical screen point.
    ClickPoint {
        /// Logical screen coordinate resolved from the guarded screenshot.
        point: ScreenPoint,
        /// Requested pointer button.
        button: PointerButton,
        /// Validated click count.
        count: u8,
        /// Requested foreground or exact-target background route.
        delivery: DeliveryPreference,
    },
    /// Hold and release a pointer button at one resolved logical screen point.
    PressPoint {
        /// Logical screen coordinate resolved from the guarded screenshot.
        point: ScreenPoint,
        /// Requested pointer button.
        button: PointerButton,
        /// Validated bounded hold duration.
        duration_ms: u32,
    },
    /// Replace one accessibility value.
    SetValue {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
        /// Sensitive value delivered only to the native driver.
        value: SensitiveText,
    },
    /// Set one native numeric range pattern.
    SetRangeValue {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
        /// Runtime-validated finite value within the observed range.
        value: f64,
    },
    /// Toggle one accessibility element.
    ToggleElement {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
    },
    /// Select one accessibility element.
    SelectElement {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
    },
    /// Expand or collapse one accessibility element.
    SetExpanded {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
        /// Desired expanded state.
        expanded: bool,
    },
    /// Move the pointer without clicking.
    MovePointer {
        /// Logical screen coordinate resolved from the guarded screenshot.
        point: ScreenPoint,
        /// Bounded movement duration.
        duration_ms: u32,
    },
    /// Type foreground text.
    TypeText {
        /// Sensitive text delivered only to the native driver.
        text: SensitiveText,
    },
    /// Press one normalized key chord.
    PressKeys {
        /// Normalized key names forming one chord or key sequence.
        keys: Vec<String>,
    },
    /// Scroll either a semantic element or the foreground surface.
    Scroll {
        /// Optional guarded semantic scroll target.
        element_key: Option<String>,
        /// Horizontal logical scroll delta.
        delta_x: f64,
        /// Vertical logical scroll delta.
        delta_y: f64,
    },
    /// Drag between runtime-resolved logical screen points.
    Drag {
        /// Logical starting screen coordinate.
        from: ScreenPoint,
        /// Logical ending screen coordinate.
        to: ScreenPoint,
        /// Validated drag duration.
        duration_ms: u32,
    },
}

impl DriverAction {
    /// Returns whether the action temporarily activates a target for input and
    /// can therefore restore a prior foreground owner afterwards.
    pub fn supports_foreground_restoration(&self) -> bool {
        matches!(
            self,
            Self::ClickPoint {
                delivery: DeliveryPreference::Foreground,
                ..
            } | Self::PressPoint { .. }
                | Self::MovePointer { .. }
                | Self::TypeText { .. }
                | Self::PressKeys { .. }
                | Self::Scroll { .. }
                | Self::Drag { .. }
        )
    }
}

/// Successful platform dispatch result.
#[derive(Clone, Copy, Debug)]
pub struct DriverActionOutput {
    /// Route actually used. It must not exceed runtime authorization.
    pub delivery_mode: DeliveryMode,
    /// Direct effect classification established by the owning native actor.
    pub effect_status: ActionEffectStatus,
    /// Closed evidence class supporting the effect classification.
    pub evidence_kind: ActionEvidenceKind,
    /// Outcome of the bounded prior-foreground restoration attempt.
    pub foreground_restoration: ForegroundRestorationStatus,
}

impl DriverActionOutput {
    /// Builds an output backed by bounded native state read-back.
    pub fn confirmed(delivery_mode: DeliveryMode) -> Self {
        Self {
            delivery_mode,
            effect_status: ActionEffectStatus::Confirmed,
            evidence_kind: ActionEvidenceKind::NativeReadback,
            foreground_restoration: ForegroundRestorationStatus::NotRequested,
        }
    }

    /// Builds an output that proves dispatch only; callers must observe effect.
    pub fn unverified(delivery_mode: DeliveryMode) -> Self {
        Self {
            delivery_mode,
            effect_status: ActionEffectStatus::Unverified,
            evidence_kind: ActionEvidenceKind::DispatchAcceptance,
            foreground_restoration: ForegroundRestorationStatus::NotRequested,
        }
    }

    /// Attaches the native actor's truthful restoration outcome.
    #[must_use]
    pub fn with_foreground_restoration(mut self, status: ForegroundRestorationStatus) -> Self {
        self.foreground_restoration = status;
        self
    }

    /// Returns whether the closed status/evidence pair is internally coherent.
    pub fn evidence_is_coherent(&self) -> bool {
        self.effect_status.is_supported_by(self.evidence_kind)
    }
}

/// Deterministic platform verification result.
#[derive(Clone, Debug)]
pub struct DriverVerification {
    /// Whether the predicate matched.
    pub matched: bool,
    /// Concise non-sensitive evidence.
    pub evidence: String,
}

/// Bounded native plain-text clipboard snapshot. The opaque change marker is
/// retained only behind a session-scoped public guard.
pub struct DriverClipboardSnapshot {
    /// Platform generation marker used for compare-and-replace.
    pub change_marker: String,
    /// Plain text, or `None` when no supported text format is available.
    pub text: Option<SensitiveText>,
}

/// Native result of a guarded clipboard replacement.
pub struct DriverClipboardWriteOutput {
    /// Platform generation marker after the replacement.
    pub change_marker: String,
    /// Truthful effect classification.
    pub effect_status: ActionEffectStatus,
    /// Evidence supporting `effect_status`.
    pub evidence_kind: ActionEvidenceKind,
}

/// Replaceable platform implementation beneath the authorization runtime.
#[async_trait]
pub trait DesktopDriver: Send + Sync {
    /// Returns immutable capability metadata.
    async fn capabilities(&self) -> Result<DriverCapabilities, DriverError>;

    /// Reads current host permission state without prompting.
    async fn permission_status(&self) -> Result<PermissionStatus, DriverError>;

    /// Lists running user applications.
    async fn list_applications(&self) -> Result<Vec<DriverApplication>, DriverError>;

    /// Lists installed application identities accepted by the native launcher.
    async fn list_launchable_applications(
        &self,
    ) -> Result<Vec<DriverLaunchableApplication>, DriverError> {
        Ok(Vec::new())
    }

    /// Starts one freshly revalidated installed application identity.
    async fn launch_application(
        &self,
        _application: &DriverLaunchableApplication,
        _allow_foreground_activation: bool,
    ) -> Result<DriverLaunchOutput, DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "installed application launch is unsupported",
        )
        .mutation_not_dispatched())
    }

    /// Lists top-level windows, optionally for one driver application key.
    async fn list_windows(
        &self,
        application_key: Option<&str>,
    ) -> Result<Vec<DriverWindow>, DriverError>;

    /// Lists physical displays available to the current interactive desktop.
    async fn list_displays(&self) -> Result<Vec<DriverDisplay>, DriverError> {
        Ok(Vec::new())
    }

    /// Captures one freshly revalidated physical display without creating
    /// action authority.
    async fn observe_display(
        &self,
        _display: &DriverDisplay,
    ) -> Result<(RgbaImage, ScreenRect), DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "physical display capture is unsupported",
        ))
    }

    /// Observes one exact driver window.
    async fn observe_window(
        &self,
        window: &DriverWindow,
        include_screenshot: bool,
        accessibility: AccessibilityMode,
    ) -> Result<DriverObservation, DriverError>;

    /// Reads bounded plain text from the current interactive desktop clipboard.
    async fn read_clipboard(
        &self,
        _max_text_bytes: usize,
    ) -> Result<DriverClipboardSnapshot, DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "plain-text clipboard reads are unsupported",
        ))
    }

    /// Replaces clipboard text only when the native generation still matches.
    async fn write_clipboard(
        &self,
        _expected_change_marker: &str,
        _text: &SensitiveText,
        _max_text_bytes: usize,
    ) -> Result<DriverClipboardWriteOutput, DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "plain-text clipboard writes are unsupported",
        )
        .mutation_not_dispatched())
    }

    /// Confirms that a prior observation fingerprint still describes the target.
    async fn observation_is_current(
        &self,
        window: &DriverWindow,
        fingerprint: &str,
    ) -> Result<bool, DriverError>;

    /// Executes one already-authorized action.
    ///
    /// `allow_foreground` is a hard upper bound. Implementations must return
    /// [`crate::DriverErrorKind::ForegroundRequired`] instead of silently
    /// activating the target when it is false.
    async fn perform_action(
        &self,
        window: &DriverWindow,
        action: DriverAction,
        allow_foreground: bool,
        restore_foreground: bool,
    ) -> Result<DriverActionOutput, DriverError>;

    /// Evaluates a deterministic predicate against fresh state.
    async fn verify_state(
        &self,
        window: &DriverWindow,
        predicate: &StatePredicate,
    ) -> Result<DriverVerification, DriverError>;
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::RgbaImage;

    #[test]
    fn recycled_pixels_are_zeroized_before_returning_to_the_driver() {
        let returned = Arc::new(Mutex::new(None));
        let output = Arc::clone(&returned);
        drop(RgbaImage::with_recycler(
            2,
            2,
            vec![0x5a; 16],
            move |pixels| *output.lock().unwrap() = Some(pixels),
        ));
        let pixels = returned.lock().unwrap().take().unwrap();
        assert!(pixels.is_empty());
        assert!(pixels.capacity() >= 16);
    }
}
