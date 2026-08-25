//! Driver capabilities and operating-system permission state.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ActionKind;

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
    /// One exact top-level application window.
    Window,
}

/// Input delivery route implemented by the selected driver.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRoute {
    /// Accessibility or UI Automation pattern delivery.
    Semantic,
    /// User-visible system foreground input.
    Foreground,
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

/// Capabilities advertised by the selected platform driver.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriverCapabilities {
    /// Stable protocol identifier.
    pub protocol_version: String,
    /// Nexus Computer Use Runtime implementation version.
    pub runtime_version: String,
    /// Host operating system.
    pub platform: Platform,
    /// Exact capture surfaces implemented by the driver.
    pub capture_modes: Vec<CaptureMode>,
    /// Whether accessibility elements can be observed.
    pub accessibility_tree: bool,
    /// Exact native delivery routes implemented by the driver.
    pub input_routes: Vec<InputRoute>,
    /// Exact action kinds implemented by the driver.
    pub actions: Vec<ActionKind>,
}
