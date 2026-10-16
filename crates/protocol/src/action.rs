//! User-visible input actions and verification predicates.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ElementRef, ScreenRect, ScreenshotPoint, SensitiveText};

/// Stable action category used by bounded capability manifests.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Focus or activate a target window.
    FocusWindow,
    /// Move and resize a target window through an operating-system window API.
    SetWindowBounds,
    /// Minimize or restore a target window.
    SetWindowMinimized,
    /// Cooperatively terminate the exact application process generation.
    TerminateApplication,
    /// Move semantic keyboard focus to an accessibility element.
    FocusElement,
    /// Invoke an accessibility element's primary action.
    InvokeElement,
    /// Click at a physical pixel coordinate.
    ClickPoint,
    /// Press and release a pointer button after a bounded hold.
    PressPoint,
    /// Replace an accessibility value.
    SetValue,
    /// Set a bounded numeric accessibility range value.
    SetRangeValue,
    /// Toggle a semantic element.
    ToggleElement,
    /// Select a semantic element.
    SelectElement,
    /// Expand or collapse a semantic element.
    SetExpanded,
    /// Move the pointer without clicking.
    MovePointer,
    /// Type text through the foreground input path.
    TypeText,
    /// Press one keyboard chord.
    PressKeys,
    /// Scroll a semantic element or pixel surface.
    Scroll,
    /// Drag between two physical pixel coordinates.
    Drag,
}

/// Pointer button used by pixel actions.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    /// Primary button.
    Left,
    /// Auxiliary button.
    Middle,
    /// Secondary button.
    Right,
}

/// Caller-selected delivery ceiling for pixel actions that support both routes.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, Hash, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPreference {
    /// Route to the exact target without activating it, or fail explicitly.
    Background,
    /// Use the system foreground pointer route.
    #[default]
    Foreground,
}

/// One authorized desktop mutation.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Brings the exact target window to the foreground.
    FocusWindow,
    /// Moves and resizes the exact target window in logical screen coordinates.
    SetWindowBounds {
        /// Requested logical top-left screen bounds.
        bounds: ScreenRect,
    },
    /// Changes the minimized state of the exact target window.
    SetWindowMinimized {
        /// `true` minimizes and `false` restores the window without granting
        /// foreground keyboard or pointer input.
        minimized: bool,
    },
    /// Requests cooperative termination of the exact guarded application's
    /// current process generation.
    TerminateApplication,
    /// Moves semantic keyboard focus to an observed element.
    FocusElement {
        /// Element scoped to the bound observation.
        element_ref: ElementRef,
    },
    /// Invokes an element from the bound observation.
    InvokeElement {
        /// Element scoped to the bound observation.
        element_ref: ElementRef,
    },
    /// Clicks a point inside the observed window.
    ClickPoint {
        /// Integer coordinate in the guarded screenshot artifact.
        point: ScreenshotPoint,
        /// Pointer button.
        button: PointerButton,
        /// Number of clicks, limited by the runtime.
        count: u8,
        /// Requested delivery route. Omission preserves the foreground v0.1
        /// behavior for older trusted hosts.
        #[serde(default)]
        delivery: DeliveryPreference,
    },
    /// Holds one pointer button at a point, then always releases it in the same dispatch.
    PressPoint {
        /// Integer coordinate in the guarded screenshot artifact.
        point: ScreenshotPoint,
        /// Pointer button.
        button: PointerButton,
        /// Bounded hold duration in milliseconds.
        duration_ms: u32,
    },
    /// Replaces an accessibility element value.
    SetValue {
        /// Element scoped to the bound observation.
        element_ref: ElementRef,
        /// Sensitive value that must not enter logs.
        value: SensitiveText,
    },
    /// Sets a numeric range control to an absolute value advertised by the
    /// guarded accessibility observation.
    SetRangeValue {
        /// Element scoped to the bound observation.
        element_ref: ElementRef,
        /// Finite value within the observed inclusive range.
        value: f64,
    },
    /// Toggles an accessibility element through its semantic pattern.
    ToggleElement {
        /// Element scoped to the bound observation.
        element_ref: ElementRef,
    },
    /// Selects an accessibility element through its semantic pattern.
    SelectElement {
        /// Element scoped to the bound observation.
        element_ref: ElementRef,
    },
    /// Expands or collapses an accessibility element.
    SetExpanded {
        /// Element scoped to the bound observation.
        element_ref: ElementRef,
        /// Desired expanded state.
        expanded: bool,
    },
    /// Moves the system pointer to a screenshot pixel without clicking.
    MovePointer {
        /// Integer coordinate in the guarded screenshot artifact.
        point: ScreenshotPoint,
        /// Bounded movement duration in milliseconds.
        duration_ms: u32,
    },
    /// Types text into the current target through foreground input.
    TypeText {
        /// Sensitive text that must not enter logs.
        text: SensitiveText,
    },
    /// Presses a normalized keyboard chord such as `meta+a`.
    PressKeys {
        /// Normalized non-empty key chord.
        keys: Vec<String>,
    },
    /// Scrolls by physical pixel deltas.
    Scroll {
        /// Optional semantic target.
        element_ref: Option<ElementRef>,
        /// Horizontal delta.
        delta_x: f64,
        /// Vertical delta.
        delta_y: f64,
    },
    /// Drags between two points inside the observed window.
    Drag {
        /// Starting point.
        from: ScreenshotPoint,
        /// Ending point.
        to: ScreenshotPoint,
        /// Bounded duration in milliseconds.
        duration_ms: u32,
    },
}

impl Action {
    /// Returns the stable action category used for authorization.
    pub fn kind(&self) -> ActionKind {
        match self {
            Self::FocusWindow => ActionKind::FocusWindow,
            Self::SetWindowBounds { .. } => ActionKind::SetWindowBounds,
            Self::SetWindowMinimized { .. } => ActionKind::SetWindowMinimized,
            Self::TerminateApplication => ActionKind::TerminateApplication,
            Self::FocusElement { .. } => ActionKind::FocusElement,
            Self::InvokeElement { .. } => ActionKind::InvokeElement,
            Self::ClickPoint { .. } => ActionKind::ClickPoint,
            Self::PressPoint { .. } => ActionKind::PressPoint,
            Self::SetValue { .. } => ActionKind::SetValue,
            Self::SetRangeValue { .. } => ActionKind::SetRangeValue,
            Self::ToggleElement { .. } => ActionKind::ToggleElement,
            Self::SelectElement { .. } => ActionKind::SelectElement,
            Self::SetExpanded { .. } => ActionKind::SetExpanded,
            Self::MovePointer { .. } => ActionKind::MovePointer,
            Self::TypeText { .. } => ActionKind::TypeText,
            Self::PressKeys { .. } => ActionKind::PressKeys,
            Self::Scroll { .. } => ActionKind::Scroll,
            Self::Drag { .. } => ActionKind::Drag,
        }
    }

    /// Returns whether the action necessarily uses foreground input in v0.1.
    pub fn requires_foreground(&self) -> bool {
        matches!(
            self,
            Self::FocusWindow
                | Self::MovePointer { .. }
                | Self::ClickPoint {
                    delivery: DeliveryPreference::Foreground,
                    ..
                }
                | Self::PressPoint { .. }
                | Self::TypeText { .. }
                | Self::PressKeys { .. }
                | Self::Scroll { .. }
                | Self::Drag { .. }
        )
    }
}

/// A deterministic predicate evaluated against fresh platform state.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementState {
    /// Whether the element accepts actions.
    Enabled,
    /// Whether the element owns keyboard focus.
    Focused,
    /// Whether the platform reports the element as selected.
    Selected,
    /// Whether the platform reports the element as checked.
    Checked,
    /// Whether the platform reports the element as expanded.
    Expanded,
}

/// A deterministic predicate evaluated against fresh platform state.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StatePredicate {
    /// Window title contains text using a case-sensitive comparison.
    WindowTitleContains {
        /// Required title fragment.
        text: String,
    },
    /// At least one element matches the supplied semantic fields.
    ElementExists {
        /// Optional role constraint.
        role: Option<String>,
        /// Optional accessible-name constraint.
        name: Option<String>,
    },
    /// At least one matching element has the requested semantic state.
    ElementState {
        /// Optional role constraint.
        role: Option<String>,
        /// Optional accessible-name constraint.
        name: Option<String>,
        /// Closed semantic state to compare.
        state: ElementState,
        /// Expected state value.
        value: bool,
    },
    /// One observed rectangle remains within another.
    BoundsContained {
        /// Inner bounds.
        inner: ScreenRect,
        /// Outer bounds.
        outer: ScreenRect,
    },
}

/// How the platform delivered an input action.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    /// Accessibility or application-specific semantic action.
    Semantic,
    /// An operating-system window or application lifecycle API.
    System,
    /// Routed directly to an exact background target without activating it.
    Background,
    /// System-wide foreground pointer or keyboard input.
    Foreground,
}

/// Whether the requested direct action effect was established before success.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionEffectStatus {
    /// The native route read back the requested direct operating-system state.
    Confirmed,
    /// Dispatch was accepted, but application-visible effect requires a fresh observation.
    Unverified,
}

/// Closed evidence class supporting a successful action result.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionEvidenceKind {
    /// The native route performed a bounded read-back or exact exit confirmation.
    NativeReadback,
    /// The native route accepted delivery without proving downstream application effect.
    DispatchAcceptance,
}

impl ActionEffectStatus {
    /// Returns whether a closed evidence class can support this effect status.
    pub fn is_supported_by(self, evidence: ActionEvidenceKind) -> bool {
        matches!(
            (self, evidence),
            (Self::Confirmed, ActionEvidenceKind::NativeReadback)
                | (Self::Unverified, ActionEvidenceKind::DispatchAcceptance)
        )
    }
}

/// Outcome of the host-authorized attempt to return foreground ownership after
/// temporary pointer or keyboard input.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForegroundRestorationStatus {
    /// Restoration was not authorized or the action intentionally owns final focus.
    NotRequested,
    /// The exact target already owned foreground before the action.
    NotNeeded,
    /// The exact previous process generation was restored and read back.
    Restored,
    /// Another owner appeared after dispatch, so the runtime did not steal focus.
    SkippedOwnerChanged,
    /// The previous process generation or native window no longer existed.
    TargetUnavailable,
    /// The operating system rejected restoration or read-back did not confirm it.
    Failed,
}
