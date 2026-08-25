//! Closed validation for manifests, observations, and actions.

use std::collections::HashSet;

use nexus_cua_protocol::{
    Action, CapabilityManifest, CuaError, ErrorCode, PermissionMode, ScreenshotMapping,
    ScreenshotPoint,
};

use crate::error::public_error;

const MAX_ALLOWED_APPLICATIONS: usize = 128;
const MAX_ACTIONS: usize = 16;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_KEYS: usize = 8;
const MAX_SCROLL_DELTA: f64 = 100_000.0;
const MAX_DRAG_DURATION_MS: u32 = 10_000;

pub(crate) fn validate_manifest(
    manifest: &CapabilityManifest,
    max_ttl_seconds: u32,
) -> Result<(), CuaError> {
    if manifest.ttl_seconds == 0 || manifest.ttl_seconds > max_ttl_seconds {
        return Err(invalid("session ttl is outside the supported range"));
    }
    if manifest.allowed_application_ids.len() > MAX_ALLOWED_APPLICATIONS {
        return Err(invalid("application allowlist is too large"));
    }
    if manifest.allowed_actions.len() > MAX_ACTIONS {
        return Err(invalid("action allowlist is too large"));
    }
    if manifest
        .allowed_application_ids
        .iter()
        .any(|value| value.trim().is_empty() || value != value.trim())
    {
        return Err(invalid(
            "application identifiers must be non-empty and normalized",
        ));
    }
    let unique_apps: HashSet<_> = manifest.allowed_application_ids.iter().collect();
    if unique_apps.len() != manifest.allowed_application_ids.len() {
        return Err(invalid("application allowlist contains duplicates"));
    }
    let unique_actions: HashSet<_> = manifest.allowed_actions.iter().collect();
    if unique_actions.len() != manifest.allowed_actions.len() {
        return Err(invalid("action allowlist contains duplicates"));
    }
    if manifest.mode == PermissionMode::ReadOnly
        && (!manifest.allowed_actions.is_empty() || manifest.allow_foreground_input)
    {
        return Err(invalid(
            "read-only sessions cannot authorize actions or foreground input",
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
        | Action::FocusElement { .. }
        | Action::InvokeElement { .. }
        | Action::ToggleElement { .. }
        | Action::SelectElement { .. }
        | Action::SetExpanded { .. } => Ok(()),
        Action::ClickPoint { point, count, .. } => {
            if !(1..=3).contains(count) {
                return Err(invalid("click count must be between one and three"));
            }
            validate_screenshot_point(*point, screenshot_mapping)
        }
        Action::SetValue { value, .. } => validate_text(value.expose()),
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
