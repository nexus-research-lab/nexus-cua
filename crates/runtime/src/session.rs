//! Authorized session state and public-reference projection.

use std::collections::HashMap;
use std::time::Instant;

use nexus_cua_protocol::{
    AccessibilityElement, Action, AppRef, CapabilityManifest, CuaError, ElementRef, ErrorCode,
    ObservationId, ScreenPoint, ScreenshotMapping, ScreenshotPoint, SessionId, WindowRef,
};
use tokio::sync::Mutex;
use tokio::time::Instant as TokioInstant;
use uuid::Uuid;

use crate::driver::{
    DriverAction, DriverApplication, DriverElement, DriverObservation, DriverWindow,
};
use crate::error::public_error;

pub(crate) struct Session {
    pub(crate) id: SessionId,
    pub(crate) manifest: CapabilityManifest,
    pub(crate) allowed_applications: Vec<DriverApplication>,
    pub(crate) expires_at: TokioInstant,
    pub(crate) state: Mutex<SessionState>,
}

impl Session {
    pub(crate) fn allows_application(&self, application: &DriverApplication) -> bool {
        self.allowed_applications
            .iter()
            .any(|allowed| allowed.matches_generation(application))
    }
}

#[derive(Default)]
pub(crate) struct SessionState {
    pub(crate) applications: HashMap<AppRef, DriverApplication>,
    pub(crate) application_refs: HashMap<String, AppRef>,
    pub(crate) windows: HashMap<WindowRef, DriverWindow>,
    pub(crate) window_refs: HashMap<String, WindowRef>,
    pub(crate) observations: HashMap<ObservationId, ObservationRecord>,
}

impl SessionState {
    pub(crate) fn project_application(&mut self, application: &DriverApplication) -> AppRef {
        if let Some(existing) = self.application_refs.get(&application.key) {
            self.applications
                .insert(existing.clone(), application.clone());
            return existing.clone();
        }
        let app_ref = AppRef::new(format!("app_{}", Uuid::new_v4().simple()));
        self.application_refs
            .insert(application.key.clone(), app_ref.clone());
        self.applications
            .insert(app_ref.clone(), application.clone());
        app_ref
    }

    pub(crate) fn project_window(&mut self, window: &DriverWindow) -> WindowRef {
        if let Some(existing) = self.window_refs.get(&window.key) {
            self.windows.insert(existing.clone(), window.clone());
            return existing.clone();
        }
        let window_ref = WindowRef::new(format!("window_{}", Uuid::new_v4().simple()));
        self.window_refs
            .insert(window.key.clone(), window_ref.clone());
        self.windows.insert(window_ref.clone(), window.clone());
        window_ref
    }

    pub(crate) fn invalidate_window_observations(&mut self, window_ref: &WindowRef) {
        for observation in self.observations.values_mut() {
            if &observation.window_ref == window_ref {
                observation.status = ObservationStatus::Invalid;
            }
        }
    }

    pub(crate) fn trim_observations(&mut self, limit: usize) {
        if self.observations.len() < limit {
            return;
        }
        let remove_count = self.observations.len().saturating_sub(limit) + 1;
        let mut ordered: Vec<_> = self
            .observations
            .iter()
            .map(|(id, value)| (id.clone(), value.created_at))
            .collect();
        ordered.sort_by_key(|(_, created_at)| *created_at);
        for (id, _) in ordered.into_iter().take(remove_count) {
            self.observations.remove(&id);
        }
    }
}

pub(crate) struct ObservationRecord {
    pub(crate) id: ObservationId,
    pub(crate) window_ref: WindowRef,
    pub(crate) screenshot_mapping: Option<ScreenshotMapping>,
    pub(crate) fingerprint: String,
    pub(crate) created_at: Instant,
    pub(crate) status: ObservationStatus,
    elements: HashMap<ElementRef, DriverElement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationStatus {
    Valid,
    InFlight,
    Invalid,
}

pub(crate) fn project_observation(
    observation: &DriverObservation,
    window_ref: &WindowRef,
    screenshot_mapping: Option<ScreenshotMapping>,
    max_elements: usize,
) -> (ObservationRecord, Vec<AccessibilityElement>) {
    let observation_id = ObservationId::new(format!("observation_{}", Uuid::new_v4().simple()));
    let selected: Vec<_> = observation.elements.iter().take(max_elements).collect();
    let refs_by_key: HashMap<_, _> = selected
        .iter()
        .map(|element| {
            (
                element.key.clone(),
                ElementRef::new(format!("element_{}", Uuid::new_v4().simple())),
            )
        })
        .collect();
    let mut internal = HashMap::new();
    let mut public = Vec::with_capacity(selected.len());
    for element in selected {
        let element_ref = refs_by_key
            .get(&element.key)
            .expect("every selected element has a projected reference")
            .clone();
        internal.insert(element_ref.clone(), element.clone());
        public.push(AccessibilityElement {
            element_ref,
            parent_ref: element
                .parent_key
                .as_ref()
                .and_then(|key| refs_by_key.get(key).cloned()),
            role: element.role.clone(),
            name: element.name.clone(),
            value: element.value.clone(),
            screen_bounds: element.screen_bounds,
            enabled: element.enabled,
            focused: element.focused,
            actions: element.actions.clone(),
        });
    }
    (
        ObservationRecord {
            id: observation_id,
            window_ref: window_ref.clone(),
            screenshot_mapping,
            fingerprint: observation.fingerprint.clone(),
            created_at: Instant::now(),
            status: ObservationStatus::Valid,
            elements: internal,
        },
        public,
    )
}

pub(crate) fn resolve_action(
    action: &Action,
    observation: &ObservationRecord,
) -> Result<DriverAction, CuaError> {
    let element_key = |element_ref: &ElementRef| {
        observation
            .elements
            .get(element_ref)
            .map(|element| element.key.clone())
            .ok_or_else(stale_observation)
    };
    Ok(match action {
        Action::FocusWindow => DriverAction::FocusWindow,
        Action::FocusElement { element_ref } => DriverAction::FocusElement {
            element_key: element_key(element_ref)?,
        },
        Action::InvokeElement { element_ref } => DriverAction::InvokeElement {
            element_key: element_key(element_ref)?,
        },
        Action::ClickPoint {
            point,
            button,
            count,
        } => DriverAction::ClickPoint {
            point: resolve_screenshot_point(*point, observation)?,
            button: *button,
            count: *count,
        },
        Action::SetValue { element_ref, value } => DriverAction::SetValue {
            element_key: element_key(element_ref)?,
            value: value.clone(),
        },
        Action::ToggleElement { element_ref } => DriverAction::ToggleElement {
            element_key: element_key(element_ref)?,
        },
        Action::SelectElement { element_ref } => DriverAction::SelectElement {
            element_key: element_key(element_ref)?,
        },
        Action::SetExpanded {
            element_ref,
            expanded,
        } => DriverAction::SetExpanded {
            element_key: element_key(element_ref)?,
            expanded: *expanded,
        },
        Action::MovePointer { point, duration_ms } => DriverAction::MovePointer {
            point: resolve_screenshot_point(*point, observation)?,
            duration_ms: *duration_ms,
        },
        Action::TypeText { text } => DriverAction::TypeText { text: text.clone() },
        Action::PressKeys { keys } => DriverAction::PressKeys { keys: keys.clone() },
        Action::Scroll {
            element_ref,
            delta_x,
            delta_y,
        } => DriverAction::Scroll {
            element_key: element_ref.as_ref().map(element_key).transpose()?,
            delta_x: *delta_x,
            delta_y: *delta_y,
        },
        Action::Drag {
            from,
            to,
            duration_ms,
        } => DriverAction::Drag {
            from: resolve_screenshot_point(*from, observation)?,
            to: resolve_screenshot_point(*to, observation)?,
            duration_ms: *duration_ms,
        },
    })
}

fn resolve_screenshot_point(
    point: ScreenshotPoint,
    observation: &ObservationRecord,
) -> Result<ScreenPoint, CuaError> {
    observation
        .screenshot_mapping
        .and_then(|mapping| mapping.to_screen(point))
        .ok_or_else(|| {
            public_error(
                ErrorCode::InvalidRequest,
                "point cannot be resolved through the guarded screenshot mapping",
                false,
                None,
            )
        })
}

pub(crate) fn stale_observation() -> CuaError {
    public_error(
        ErrorCode::StaleObservation,
        "observation is expired, invalidated, or no longer current",
        true,
        Some("observe_window"),
    )
}
