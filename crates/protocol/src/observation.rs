//! Application, window, accessibility, and screenshot observation types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AppRef, ArtifactRef, ElementRef, ObservationId, ScreenRect, ScreenshotMapping, SessionId,
    WindowRef,
};

/// One running application visible to the selected session.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSummary {
    /// Session-scoped opaque reference.
    pub app_ref: AppRef,
    /// Human-readable application name.
    pub name: String,
    /// Stable bundle identifier or executable identity for host policy display.
    pub application_id: String,
    /// Whether the application currently owns the foreground.
    pub foreground: bool,
}

/// One top-level application window.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSummary {
    /// Session-scoped opaque window reference.
    pub window_ref: WindowRef,
    /// Owning application reference.
    pub app_ref: AppRef,
    /// Current window title.
    pub title: String,
    /// Logical top-left screen bounds.
    pub screen_bounds: ScreenRect,
    /// Whether the window is minimized.
    pub minimized: bool,
    /// Whether the window is currently visible on a display.
    pub visible: bool,
    /// Whether the window owns foreground input.
    pub foreground: bool,
}

/// Input for listing windows in a session.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListWindowsInput {
    /// Runtime-issued session identity.
    pub session_id: SessionId,
    /// Optional application filter.
    pub app_ref: Option<AppRef>,
}

/// Input for capturing one exact window.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveWindowInput {
    /// Runtime-issued session identity.
    pub session_id: SessionId,
    /// Session-scoped target window.
    pub window_ref: WindowRef,
    /// Whether to produce a transient PNG artifact.
    pub include_screenshot: bool,
    /// Requested bounded semantic-tree view.
    pub accessibility: AccessibilityMode,
}

/// Requested semantic-tree detail for an observation.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessibilityMode {
    /// Do not query the platform accessibility provider.
    Disabled,
    /// Actionable elements plus their minimum ancestor hierarchy.
    Interactive,
    /// A bounded diagnostic control-view tree.
    Full,
}

/// Runtime-owned transient screenshot metadata.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotArtifact {
    /// Opaque artifact identity.
    pub artifact_ref: ArtifactRef,
    /// Runtime-chosen absolute local file path.
    pub path: String,
    /// Always `image/png` in v0.1.
    pub mime_type: String,
    /// Exact relationship between image pixels and logical screen points.
    pub mapping: ScreenshotMapping,
    /// Encoded byte length.
    pub byte_length: u64,
    /// Lowercase SHA-256 digest.
    pub sha256: String,
}

/// Normalized accessibility element from one immutable observation.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccessibilityElement {
    /// Reference valid only for the containing observation.
    pub element_ref: ElementRef,
    /// Optional parent element reference.
    pub parent_ref: Option<ElementRef>,
    /// Normalized semantic role.
    pub role: String,
    /// Accessible name or label.
    pub name: String,
    /// Non-sensitive display value when safely available.
    pub value: Option<String>,
    /// Element bounds in logical top-left screen coordinates.
    pub screen_bounds: Option<ScreenRect>,
    /// Whether the element can receive actions.
    pub enabled: bool,
    /// Whether the element currently owns keyboard focus.
    pub focused: bool,
    /// Normalized semantic actions supported by the element.
    pub actions: Vec<String>,
}

/// Stable reason a semantic snapshot is intentionally partial.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TruncationReason {
    /// Configured node budget was reached.
    NodeLimit,
    /// Configured hierarchy depth was reached.
    DepthLimit,
    /// Configured aggregate string-byte budget was reached.
    ByteLimit,
    /// Provider wall-time budget elapsed.
    Deadline,
    /// An application accessibility provider stopped responding.
    ProviderFailure,
}

/// Metadata explaining why a semantic snapshot is partial.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationTruncation {
    /// Stable bounded-resource or provider reason.
    pub reason: TruncationReason,
    /// Number of elements successfully included.
    pub emitted_elements: u32,
}

/// Immutable result of observing one exact window.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowObservation {
    /// Runtime identity for stale-action protection.
    pub observation_id: ObservationId,
    /// Exact target window.
    pub window_ref: WindowRef,
    /// RFC 3339 capture timestamp.
    pub captured_at: String,
    /// Logical window bounds at capture time.
    pub window_screen_bounds: ScreenRect,
    /// Optional transient screenshot.
    pub screenshot: Option<ScreenshotArtifact>,
    /// Normalized actionable accessibility elements.
    pub elements: Vec<AccessibilityElement>,
    /// False when the platform tree was truncated or partially unavailable.
    pub elements_complete: bool,
    /// Present exactly when the semantic tree is partial.
    pub elements_truncation: Option<ObservationTruncation>,
}
