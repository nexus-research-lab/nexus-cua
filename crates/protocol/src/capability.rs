//! Driver capabilities and operating-system permission state.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ActionKind, ClipboardOperation};

/// Operating system hosting the driver.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    /// Apple macOS.
    Macos,
    /// Microsoft Windows.
    Windows,
    /// A build without a supported desktop backend.
    Unsupported,
}

/// Capture surface implemented by the selected driver.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Display-sized scene containing only the authorized application's surfaces.
    ApplicationScene,
    /// One exact top-level application window.
    Window,
    /// One exact physical display for read-only desktop overview.
    Display,
}

/// Input delivery route implemented by the selected driver.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRoute {
    /// Accessibility or UI Automation pattern delivery.
    Semantic,
    /// Operating-system window or application lifecycle APIs.
    System,
    /// Best-effort exact-window input that does not activate the target.
    Background,
    /// User-visible system foreground input.
    Foreground,
}

/// Installed-application lifecycle operations implemented by a driver.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationLifecycleOperation {
    /// Discover reviewed installed applications and start one exact identity.
    Launch,
}

/// One system permission's current state.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    /// The responsible signed application has permission.
    Granted,
    /// The user or system denied permission.
    Denied,
    /// The user has not made a decision yet.
    NotDetermined,
    /// The platform does not expose this permission separately.
    NotApplicable,
    /// The driver cannot determine the state safely.
    Unknown,
}

/// Current permission snapshot for the responsible desktop host.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionStatus {
    /// Permission to capture windows or displays.
    pub screen_capture: PermissionState,
    /// Permission to inspect and semantically operate accessibility elements.
    pub accessibility: PermissionState,
    /// Permission to synthesize foreground keyboard or pointer input.
    pub input_control: PermissionState,
}

/// Runtime-enforced bounds for host-generated action inputs.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionLimits {
    /// Maximum session lifetime in seconds.
    pub max_session_ttl_seconds: u32,
    /// Maximum UTF-8 bytes in one text action.
    pub max_text_bytes: u32,
    /// Maximum keys in one chord.
    pub max_keys: u32,
    /// Maximum absolute scroll delta.
    pub max_scroll_delta: u32,
    /// Maximum movement, hold, or drag duration.
    pub max_pointer_duration_ms: u32,
    /// Maximum absolute logical window origin.
    pub max_window_coordinate: u32,
    /// Maximum logical window dimension.
    pub max_window_dimension: u32,
    /// Maximum absolute numeric control value.
    pub max_range_value: u64,
    /// Maximum clicks in one action.
    pub max_click_count: u32,
}

impl ActionLimits {
    /// Default hard limits, also used by runtime validation.
    pub const DEFAULT: Self = Self {
        max_session_ttl_seconds: 3600,
        max_text_bytes: 65_536,
        max_keys: 8,
        max_scroll_delta: 100_000,
        max_pointer_duration_ms: 10_000,
        max_window_coordinate: 1_000_000,
        max_window_dimension: 100_000,
        max_range_value: 1_000_000_000_000,
        max_click_count: 3,
    };
}

impl Default for ActionLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Capabilities advertised by the selected platform driver.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriverCapabilities {
    /// Whether action requests may include bounded post-action observation.
    #[serde(default)]
    pub observe_after: bool,
    /// Effective runtime limits; hosts must not invent larger input bounds.
    pub action_limits: ActionLimits,
    /// Stable protocol identifier.
    pub protocol_version: String,
    /// Nexus Computer Use Runtime implementation version.
    pub runtime_version: String,
    /// Host operating system.
    pub platform: Platform,
    /// Platform shortcut modifier used by the portable `mod` key.
    pub primary_modifier: String,
    /// Exact capture surfaces implemented by the driver.
    pub capture_modes: Vec<CaptureMode>,
    /// Whether accessibility elements can be observed.
    pub accessibility_tree: bool,
    /// Exact native delivery routes implemented by the driver.
    pub input_routes: Vec<InputRoute>,
    /// Exact installed-application lifecycle operations implemented by the driver.
    pub application_lifecycle: Vec<ApplicationLifecycleOperation>,
    /// Separately authorized clipboard operations implemented by a driver.
    #[serde(default)]
    pub clipboard: Vec<ClipboardOperation>,
    /// Exact action kinds implemented by the driver.
    pub actions: Vec<ActionKind>,
}
