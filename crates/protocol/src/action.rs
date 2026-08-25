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
    /// Move semantic keyboard focus to an accessibility element.
    FocusElement,
    /// Invoke an accessibility element's primary action.
    InvokeElement,
    /// Click at a physical pixel coordinate.
    ClickPoint,
    /// Replace an accessibility value.
    SetValue,
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

/// One authorized desktop mutation.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Brings the exact target window to the foreground.
    FocusWindow,
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
    },
    /// Replaces an accessibility element value.
    SetValue {
        /// Element scoped to the bound observation.
        element_ref: ElementRef,
        /// Sensitive value that must not enter logs.
        value: SensitiveText,
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
            Self::FocusElement { .. } => ActionKind::FocusElement,
            Self::InvokeElement { .. } => ActionKind::InvokeElement,
            Self::ClickPoint { .. } => ActionKind::ClickPoint,
            Self::SetValue { .. } => ActionKind::SetValue,
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
                | Self::ClickPoint { .. }
                | Self::TypeText { .. }
                | Self::PressKeys { .. }
                | Self::Scroll { .. }
                | Self::Drag { .. }
        )
    }
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
    /// System-wide foreground pointer or keyboard input.
    Foreground,
}
