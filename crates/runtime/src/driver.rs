//! Narrow internal contract implemented by platform drivers.

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, ApplicationProvenance, DeliveryMode, DriverCapabilities,
    ObservationTruncation, PermissionStatus, PointerButton, ScreenPoint, ScreenRect, SensitiveText,
    StatePredicate,
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
    /// Logical top-left screen bounds.
    pub screen_bounds: Option<ScreenRect>,
    /// Whether actions are currently accepted.
    pub enabled: bool,
    /// Whether the element owns keyboard focus.
    pub focused: bool,
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
    },
    /// Replace one accessibility value.
    SetValue {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
        /// Sensitive value delivered only to the native driver.
        value: SensitiveText,
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

/// Successful platform dispatch result.
#[derive(Clone, Copy, Debug)]
pub struct DriverActionOutput {
    /// Route actually used. It must not exceed runtime authorization.
    pub delivery_mode: DeliveryMode,
}

/// Deterministic platform verification result.
#[derive(Clone, Debug)]
pub struct DriverVerification {
    /// Whether the predicate matched.
    pub matched: bool,
    /// Concise non-sensitive evidence.
    pub evidence: String,
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

    /// Lists top-level windows, optionally for one driver application key.
    async fn list_windows(
        &self,
        application_key: Option<&str>,
    ) -> Result<Vec<DriverWindow>, DriverError>;

    /// Observes one exact driver window.
    async fn observe_window(
        &self,
        window: &DriverWindow,
        include_screenshot: bool,
        accessibility: AccessibilityMode,
    ) -> Result<DriverObservation, DriverError>;

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
