//! Closed validation for manifests, observations, and actions.

use std::collections::HashSet;

use nexus_cua_protocol::{
    Action, CapabilityManifest, CuaError, ErrorCode, PermissionMode, ScreenshotMapping,
    ScreenshotPoint, SensitiveText,
};

use crate::error::public_error;

const MAX_ALLOWED_APPLICATIONS: usize = 128;
const MAX_DISCOVERY_REF_BYTES: usize = 256;
const MAX_APPLICATION_ALLOWLIST_BYTES: usize = 32 * 1024;
const MAX_ACTIONS: usize = 32;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_KEYS: usize = 8;
const MAX_SCROLL_DELTA: f64 = 100_000.0;
const MAX_DRAG_DURATION_MS: u32 = 10_000;
const MAX_WINDOW_COORDINATE: f64 = 1_000_000.0;
const MAX_WINDOW_DIMENSION: f64 = 100_000.0;
const MAX_RANGE_VALUE: f64 = 1_000_000_000_000.0;

pub(crate) fn validate_manifest(
    manifest: &CapabilityManifest,
    max_ttl_seconds: u32,
) -> Result<(), CuaError> {
    if manifest.ttl_seconds == 0 || manifest.ttl_seconds > max_ttl_seconds {
        return Err(invalid("session ttl is outside the supported range"));
    }
    if (!manifest.allow_display_capture
        && !manifest.allow_clipboard_read
        && !manifest.allow_clipboard_write
        && manifest.application_refs.is_empty())
        || manifest.application_refs.len() > MAX_ALLOWED_APPLICATIONS
    {
        return Err(invalid(
            "application reference allowlist is empty without display-capture or clipboard authority, or is too large",
        ));
    }
    let allowlist_bytes = manifest
        .application_refs
        .iter()
        .try_fold(0_usize, |total, value| {
            total.checked_add(value.as_str().len())
        })
        .ok_or_else(|| invalid("application allowlist is too large"))?;
    if allowlist_bytes > MAX_APPLICATION_ALLOWLIST_BYTES
        || manifest
            .application_refs
            .iter()
            .any(|value| value.as_str().len() > MAX_DISCOVERY_REF_BYTES)
    {
        return Err(invalid("application allowlist is too large"));
    }
    if manifest.allowed_actions.len() > MAX_ACTIONS {
        return Err(invalid("action allowlist is too large"));
    }
    if manifest
        .application_refs
        .iter()
        .any(|value| value.as_str().trim().is_empty() || value.as_str() != value.as_str().trim())
    {
        return Err(invalid(
            "discovery references must be non-empty and normalized",
        ));
    }
    let unique_apps: HashSet<_> = manifest.application_refs.iter().collect();
    if unique_apps.len() != manifest.application_refs.len() {
        return Err(invalid("application allowlist contains duplicates"));
    }
    let unique_actions: HashSet<_> = manifest.allowed_actions.iter().collect();
    if unique_actions.len() != manifest.allowed_actions.len() {
        return Err(invalid("action allowlist contains duplicates"));
    }
    if manifest.mode == PermissionMode::ReadOnly
        && (!manifest.allowed_actions.is_empty()
            || manifest.allow_foreground_input
            || manifest.allow_clipboard_write
            || manifest.restore_foreground_after_input)
    {
        return Err(invalid(
            "read-only sessions cannot authorize actions or foreground input",
        ));
    }
    if manifest.restore_foreground_after_input && !manifest.allow_foreground_input {
        return Err(invalid(
            "foreground restoration requires foreground input authority",
        ));
    }
    if manifest.allow_clipboard_write && !manifest.allow_clipboard_read {
        return Err(invalid(
            "clipboard writes require clipboard-read authority for a fresh change guard",
        ));
    }
    Ok(())
}

pub(crate) fn validate_clipboard_text(
    text: &SensitiveText,
    max_text_bytes: usize,
) -> Result<(), CuaError> {
    if text.expose().len() > max_text_bytes || text.expose().contains('\0') {
        return Err(invalid(
            "clipboard text is too large or contains an embedded NUL",
        ));
    }
    Ok(())
}

pub(crate) fn validate_action(
    action: &Action,
    screenshot_mapping: Option<ScreenshotMapping>,
) -> Result<(), CuaError> {
    match action {
        Action::FocusWindow
        | Action::SetWindowMinimized { .. }
        | Action::TerminateApplication
        | Action::FocusElement { .. }
        | Action::InvokeElement { .. }
        | Action::ToggleElement { .. }
        | Action::SelectElement { .. }
        | Action::SetExpanded { .. } => Ok(()),
        Action::SetWindowBounds { bounds } => {
            if !bounds.x.is_finite()
                || !bounds.y.is_finite()
                || !bounds.width.is_finite()
                || !bounds.height.is_finite()
                || bounds.x.abs() > MAX_WINDOW_COORDINATE
                || bounds.y.abs() > MAX_WINDOW_COORDINATE
                || !(1.0..=MAX_WINDOW_DIMENSION).contains(&bounds.width)
                || !(1.0..=MAX_WINDOW_DIMENSION).contains(&bounds.height)
            {
                return Err(invalid("window bounds are outside the supported range"));
            }
            Ok(())
        }
        Action::ClickPoint { point, count, .. } => {
            if !(1..=3).contains(count) {
                return Err(invalid("click count must be between one and three"));
            }
            validate_screenshot_point(*point, screenshot_mapping)
        }
        Action::PressPoint {
            point, duration_ms, ..
        } => {
            if *duration_ms > MAX_DRAG_DURATION_MS {
                return Err(invalid(
                    "pointer hold duration is outside the supported range",
                ));
            }
            validate_screenshot_point(*point, screenshot_mapping)
        }
        Action::SetValue { value, .. } => validate_text(value.expose()),
        Action::SetRangeValue { value, .. } => {
            if !value.is_finite() || value.abs() > MAX_RANGE_VALUE {
                return Err(invalid(
                    "numeric range value is outside the supported range",
                ));
            }
            Ok(())
        }
        Action::TypeText { text } => validate_text(text.expose()),
        Action::PressKeys { keys } => {
            if keys.is_empty()
                || keys.len() > MAX_KEYS
                || keys.iter().any(|key| {
                    key.trim().is_empty()
                        || key != key.trim()
                        || key != &key.to_ascii_lowercase()
                        || !is_supported_key(key)
                })
            {
                return Err(invalid(
                    "key chord is empty, too large, unsupported, or not normalized",
                ));
            }
            Ok(())
        }
        Action::MovePointer { point, duration_ms } => {
            if *duration_ms > MAX_DRAG_DURATION_MS {
                return Err(invalid(
                    "pointer movement duration is outside the supported range",
                ));
            }
            validate_screenshot_point(*point, screenshot_mapping)
        }
        Action::Scroll {
            delta_x, delta_y, ..
        } => {
            if !delta_x.is_finite()
                || !delta_y.is_finite()
                || delta_x.abs() > MAX_SCROLL_DELTA
                || delta_y.abs() > MAX_SCROLL_DELTA
            {
                return Err(invalid("scroll delta is outside the supported range"));
            }
            Ok(())
        }
        Action::Drag {
            from,
            to,
            duration_ms,
        } => {
            if *duration_ms == 0 || *duration_ms > MAX_DRAG_DURATION_MS {
                return Err(invalid("drag duration is outside the supported range"));
            }
            validate_screenshot_point(*from, screenshot_mapping)?;
            validate_screenshot_point(*to, screenshot_mapping)
        }
    }
}

fn is_supported_key(key: &str) -> bool {
    matches!(
        key,
        "meta"
            | "command"
            | "control"
            | "alt"
            | "option"
            | "shift"
            | "enter"
            | "return"
            | "tab"
            | "space"
            | "backspace"
            | "delete"
            | "escape"
            | "left"
            | "right"
            | "up"
            | "down"
            | "home"
            | "end"
            | "page_up"
            | "page_down"
            | "f1"
            | "f2"
            | "f3"
            | "f4"
            | "f5"
            | "f6"
            | "f7"
            | "f8"
            | "f9"
            | "f10"
            | "f11"
            | "f12"
            | "a"
            | "b"
            | "c"
            | "d"
            | "e"
            | "f"
            | "g"
            | "h"
            | "i"
            | "j"
            | "k"
            | "l"
            | "m"
            | "n"
            | "o"
            | "p"
            | "q"
            | "r"
            | "s"
            | "t"
            | "u"
            | "v"
            | "w"
            | "x"
            | "y"
            | "z"
            | "0"
            | "1"
            | "2"
            | "3"
            | "4"
            | "5"
            | "6"
            | "7"
            | "8"
            | "9"
    )
}

fn validate_text(text: &str) -> Result<(), CuaError> {
    if text.len() > MAX_TEXT_BYTES || text.contains('\0') {
        return Err(invalid("text input is too large or contains NUL"));
    }
    Ok(())
}

fn validate_screenshot_point(
    point: ScreenshotPoint,
    screenshot_mapping: Option<ScreenshotMapping>,
) -> Result<(), CuaError> {
    let mapping = screenshot_mapping
        .ok_or_else(|| invalid("pixel actions require a screenshot in the guarded observation"))?;
    if !mapping.pixel_size.contains(point) {
        return Err(invalid("point lies outside the guarded screenshot"));
    }
    Ok(())
}

fn invalid(message: &str) -> CuaError {
    public_error(ErrorCode::InvalidRequest, message, false, None)
}

#[cfg(test)]
mod tests {
    use nexus_cua_protocol::{PixelSize, PointerButton, ScreenRect};

    use super::*;

    fn mapping() -> ScreenshotMapping {
        ScreenshotMapping {
            screen_bounds: ScreenRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            pixel_size: PixelSize {
                width: 100,
                height: 100,
            },
        }
    }

    #[test]
    fn pointer_hold_is_bounded_and_requires_a_guarded_point() {
        let valid = Action::PressPoint {
            point: ScreenshotPoint { x: 50, y: 50 },
            button: PointerButton::Left,
            duration_ms: MAX_DRAG_DURATION_MS,
        };
        assert!(validate_action(&valid, Some(mapping())).is_ok());

        let too_long = Action::PressPoint {
            point: ScreenshotPoint { x: 50, y: 50 },
            button: PointerButton::Left,
            duration_ms: MAX_DRAG_DURATION_MS + 1,
        };
        assert!(validate_action(&too_long, Some(mapping())).is_err());
        assert!(validate_action(&valid, None).is_err());
    }

    #[test]
    fn window_bounds_are_finite_positive_and_bounded() {
        let valid = Action::SetWindowBounds {
            bounds: ScreenRect {
                x: -500.0,
                y: 10.0,
                width: 1_200.0,
                height: 800.0,
            },
        };
        assert!(validate_action(&valid, None).is_ok());
        let invalid = Action::SetWindowBounds {
            bounds: ScreenRect {
                width: 0.0,
                ..ScreenRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }
            },
        };
        assert!(validate_action(&invalid, None).is_err());
    }
}
