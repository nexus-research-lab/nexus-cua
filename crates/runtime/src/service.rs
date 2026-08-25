//! Session runtime and public command execution.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use nexus_cua_protocol::{
    AccessibilityElement, Action, ActionOutput, AppRef, ApplicationSummary, CapabilityManifest,
    Command, CommandResult, CuaError, DeliveryMode, ElementRef, ErrorCode, ListWindowsInput,
    ObservationId, OpenSessionInput, OpenSessionOutput, PermissionMode, SessionId, SessionInput,
    VerificationOutput, VerifyStateInput, WindowObservation, WindowRef, WindowSummary,
};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::artifact_store::ArtifactStore;
use crate::driver::{
    DesktopDriver, DriverAction, DriverApplication, DriverElement, DriverObservation, DriverWindow,
};
use crate::error::public_error;
use crate::validation::{validate_action, validate_manifest};

/// Runtime limits selected by the embedding host.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Private directory for transient screenshot artifacts.
    pub artifact_root: PathBuf,
    /// Hard maximum session lifetime.
    pub max_session_ttl: StdDuration,
    /// Maximum age of an observation used for mutation.
    pub max_observation_age: StdDuration,
    /// Maximum retained observations per session.
    pub max_observations_per_session: usize,
    /// Maximum normalized elements in one observation.
    pub max_elements_per_observation: usize,
    /// Maximum decoded screenshot pixels.
    pub max_image_pixels: u64,
}

impl RuntimeConfig {
    /// Secure defaults suitable for a local desktop product.
    pub fn new(artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            artifact_root: artifact_root.into(),
            max_session_ttl: StdDuration::from_secs(60 * 60),
            max_observation_age: StdDuration::from_secs(30),
            max_observations_per_session: 32,
            max_elements_per_observation: 2_000,
            max_image_pixels: 100_000_000,
        }
    }
}

/// Capability-bounded runtime above one replaceable platform driver.
pub struct Runtime {
    driver: Arc<dyn DesktopDriver>,
    artifacts: ArtifactStore,
    config: RuntimeConfig,
    sessions: RwLock<HashMap<SessionId, Arc<Session>>>,
}

impl Runtime {
    /// Creates a runtime with no ambient authority.
    ///
    /// # Errors
    ///
    /// Returns an error when the private artifact root cannot be created or
    /// does not satisfy the runtime's filesystem safety requirements.
    pub fn new(driver: Arc<dyn DesktopDriver>, config: RuntimeConfig) -> Result<Self, CuaError> {
        let artifacts = ArtifactStore::new(&config.artifact_root, config.max_image_pixels)?;
        Ok(Self {
            driver,
            artifacts,
            config,
            sessions: RwLock::new(HashMap::new()),
        })
    }

    /// Executes one already transport-authenticated command.
    ///
    /// # Errors
    ///
    /// Returns a stable protocol error when validation, authorization, native
    /// driver execution, or artifact persistence fails.
    pub async fn execute(&self, command: Command) -> Result<CommandResult, CuaError> {
        self.reap_expired().await;
        match command {
            Command::GetCapabilities => self
                .driver
                .capabilities()
                .await
                .map(CommandResult::Capabilities)
                .map_err(Into::into),
            Command::GetPermissionStatus => self
                .driver
                .permission_status()
                .await
                .map(CommandResult::PermissionStatus)
                .map_err(Into::into),
            Command::OpenSession(input) => self.open_session(input).await,
            Command::CloseSession(input) => self.close_session(input).await,
            Command::ListApps(input) => self.list_apps(input).await,
            Command::ListWindows(input) => self.list_windows(input).await,
            Command::ObserveWindow(input) => self.observe_window(input).await,
            Command::PerformAction(input) => self.perform_action(input).await,
            Command::VerifyState(input) => self.verify_state(input).await,
        }
    }

    async fn open_session(&self, input: OpenSessionInput) -> Result<CommandResult, CuaError> {
        let max_ttl = u32::try_from(self.config.max_session_ttl.as_secs()).unwrap_or(u32::MAX);
        validate_manifest(&input.manifest, max_ttl)?;
        let session_id = SessionId::new(format!("session_{}", Uuid::new_v4().simple()));
        let ttl = StdDuration::from_secs(u64::from(input.manifest.ttl_seconds));
        let expires_at_monotonic = Instant::now() + ttl;
        let expires_at_wall =
            OffsetDateTime::now_utc() + Duration::seconds(i64::from(input.manifest.ttl_seconds));
        let expires_at = expires_at_wall.format(&Rfc3339).map_err(|_| {
            public_error(
                ErrorCode::Internal,
                "failed to format session expiry",
                false,
                None,
            )
        })?;
        let session = Arc::new(Session {
            id: session_id.clone(),
            manifest: input.manifest,
            expires_at: expires_at_monotonic,
            state: Mutex::new(SessionState::default()),
        });
        self.sessions
            .write()
            .await
            .insert(session_id.clone(), session);
        info!(session_id = %session_id, "computer-use session opened");
        Ok(CommandResult::SessionOpened(OpenSessionOutput {
            session_id,
            expires_at,
        }))
    }

    async fn close_session(&self, input: SessionInput) -> Result<CommandResult, CuaError> {
        let removed = self.sessions.write().await.remove(&input.session_id);
        if removed.is_none() {
            return Err(session_unavailable());
        }
        self.artifacts.remove_session(&input.session_id);
        info!(session_id = %input.session_id, "computer-use session closed");
        Ok(CommandResult::Acknowledged)
    }

    async fn list_apps(&self, input: SessionInput) -> Result<CommandResult, CuaError> {
        let session = self.session(&input.session_id).await?;
        let applications = self
            .driver
            .list_applications()
            .await
            .map_err(CuaError::from)?;
        let mut state = session.state.lock().await;
        let mut output = Vec::new();
        for application in applications
            .into_iter()
            .filter(|app| session.allows_application(&app.application_id))
        {
            let app_ref = state.project_application(&application);
            output.push(ApplicationSummary {
                app_ref,
                name: application.name,
                application_id: application.application_id,
                foreground: application.foreground,
            });
        }
        Ok(CommandResult::Apps(output))
    }

    async fn list_windows(&self, input: ListWindowsInput) -> Result<CommandResult, CuaError> {
        let session = self.session(&input.session_id).await?;
        let application_key = if let Some(app_ref) = &input.app_ref {
            let state = session.state.lock().await;
            Some(
                state
                    .applications
                    .get(app_ref)
                    .ok_or_else(reference_not_found)?
                    .key
                    .clone(),
            )
        } else {
            None
        };
        let windows = self
            .driver
            .list_windows(application_key.as_deref())
            .await
            .map_err(CuaError::from)?;
        let mut state = session.state.lock().await;
        let mut output = Vec::new();
        for window in windows
            .into_iter()
            .filter(|window| session.allows_application(&window.application.application_id))
        {
            let app_ref = state.project_application(&window.application);
            let window_ref = state.project_window(&window);
            output.push(WindowSummary {
                window_ref,
                app_ref,
                title: window.title,
                screen_bounds: window.screen_bounds,
                minimized: window.minimized,
                visible: window.visible,
                foreground: window.foreground,
            });
        }
        Ok(CommandResult::Windows(output))
    }

    async fn observe_window(
        &self,
        input: nexus_cua_protocol::ObserveWindowInput,
    ) -> Result<CommandResult, CuaError> {
        let session = self.session(&input.session_id).await?;
        let window = {
            let state = session.state.lock().await;
            state
                .windows
                .get(&input.window_ref)
                .cloned()
                .ok_or_else(reference_not_found)?
        };
        if !session.allows_application(&window.application.application_id) {
            return Err(capability_denied());
        }
        let mut observation = self
            .driver
            .observe_window(&window, input.include_screenshot, input.accessibility)
            .await
            .map_err(CuaError::from)?;
        if observation.window_key != window.key {
            return Err(public_error(
                ErrorCode::Internal,
                "driver observation target mismatch",
                false,
                None,
            ));
        }
        let screenshot = match (
            observation.screenshot.take(),
            observation.screenshot_screen_bounds,
        ) {
            (Some(image), Some(screen_bounds)) => Some(self.artifacts.write_image(
                &session.id,
                &image,
                screen_bounds,
            )?),
            (None, None) => None,
            _ => {
                return Err(public_error(
                    ErrorCode::Internal,
                    "driver returned an incomplete screenshot mapping",
                    false,
                    None,
                ));
            }
        };
        let captured_at = OffsetDateTime::now_utc().format(&Rfc3339).map_err(|_| {
            public_error(
                ErrorCode::Internal,
                "failed to format observation timestamp",
                false,
                None,
            )
        })?;

        let mut state = session.state.lock().await;
        state.invalidate_window_observations(&input.window_ref);
        state.trim_observations(self.config.max_observations_per_session);
        let (record, elements) = project_observation(
            &observation,
            &input.window_ref,
            screenshot.as_ref().map(|artifact| artifact.mapping),
            self.config.max_elements_per_observation,
        );
        let observation_id = record.id.clone();
        let elements_complete = observation.elements_complete
            && observation.elements.len() <= self.config.max_elements_per_observation;
        let elements_truncation = if elements_complete {
            None
        } else if observation.elements.len() > self.config.max_elements_per_observation {
            Some(nexus_cua_protocol::ObservationTruncation {
                reason: nexus_cua_protocol::TruncationReason::NodeLimit,
                emitted_elements: u32::try_from(elements.len()).unwrap_or(u32::MAX),
            })
        } else {
            observation.elements_truncation.clone()
        };
        state.observations.insert(observation_id.clone(), record);
        Ok(CommandResult::WindowObserved(Box::new(WindowObservation {
            observation_id,
            window_ref: input.window_ref,
            captured_at,
            window_screen_bounds: observation.window_screen_bounds,
            screenshot,
            elements,
            elements_complete,
            elements_truncation,
        })))
    }

    async fn perform_action(
        &self,
        input: nexus_cua_protocol::PerformActionInput,
    ) -> Result<CommandResult, CuaError> {
        let session = self.session(&input.session_id).await?;
        if session.manifest.mode != PermissionMode::Bounded
            || !session
                .manifest
                .allowed_actions
                .contains(&input.action.kind())
        {
            return Err(capability_denied());
        }
        if input.action.requires_foreground() && !session.manifest.allow_foreground_input {
            return Err(public_error(
                ErrorCode::ForegroundRequired,
                "action requires foreground input not granted by this session",
                false,
                Some("request_foreground_authorization"),
            ));
        }

        let (window, fingerprint, driver_action) = {
            let mut state = session.state.lock().await;
            let window = state
                .windows
                .get(&input.window_ref)
                .cloned()
                .ok_or_else(reference_not_found)?;
            if !session.allows_application(&window.application.application_id) {
                return Err(capability_denied());
            }
            let observation = state
                .observations
                .get_mut(&input.observation_id)
                .ok_or_else(stale_observation)?;
            if observation.window_ref != input.window_ref
                || observation.status != ObservationStatus::Valid
                || observation.created_at.elapsed() > self.config.max_observation_age
            {
                observation.status = ObservationStatus::Invalid;
                return Err(stale_observation());
            }
            validate_action(&input.action, observation.screenshot_mapping)?;
            let driver_action = resolve_action(&input.action, observation)?;
            observation.status = ObservationStatus::InFlight;
            (window, observation.fingerprint.clone(), driver_action)
        };

        let current = self
            .driver
            .observation_is_current(&window, &fingerprint)
            .await
            .map_err(CuaError::from)?;
        if !current {
            self.invalidate_observation(&session, &input.observation_id)
                .await;
            return Err(stale_observation());
        }

        let result = self
            .driver
            .perform_action(
                &window,
                driver_action,
                session.manifest.allow_foreground_input,
            )
            .await;
        {
            let mut state = session.state.lock().await;
            state.invalidate_window_observations(&input.window_ref);
        }
        let result = result.map_err(CuaError::from)?;
        if result.delivery_mode == DeliveryMode::Foreground
            && !session.manifest.allow_foreground_input
        {
            warn!(session_id = %session.id, "driver exceeded foreground authorization");
            return Err(public_error(
                ErrorCode::Internal,
                "driver exceeded the authorized delivery mode",
                false,
                None,
            ));
        }
        debug!(
            session_id = %session.id,
            action = ?input.action.kind(),
            delivery_mode = ?result.delivery_mode,
            "computer-use action dispatched"
        );
        Ok(CommandResult::ActionPerformed(ActionOutput {
            delivery_mode: result.delivery_mode,
            dispatched: true,
            observation_invalidated: true,
        }))
    }

    async fn verify_state(&self, input: VerifyStateInput) -> Result<CommandResult, CuaError> {
        let session = self.session(&input.session_id).await?;
        let window = {
            let state = session.state.lock().await;
            state
                .windows
                .get(&input.window_ref)
                .cloned()
                .ok_or_else(reference_not_found)?
        };
        if !session.allows_application(&window.application.application_id) {
            return Err(capability_denied());
        }
        let result = self
            .driver
            .verify_state(&window, &input.predicate)
            .await
            .map_err(CuaError::from)?;
        Ok(CommandResult::StateVerified(VerificationOutput {
            matched: result.matched,
            evidence: result.evidence,
        }))
    }

    async fn session(&self, session_id: &SessionId) -> Result<Arc<Session>, CuaError> {
        let mut sessions = self.sessions.write().await;
        let Some(session) = sessions.get(session_id).cloned() else {
            return Err(session_unavailable());
        };
        if session.expires_at <= Instant::now() {
            sessions.remove(session_id);
            drop(sessions);
            self.artifacts.remove_session(session_id);
            return Err(session_unavailable());
        }
        Ok(session)
    }

    async fn reap_expired(&self) {
        let now = Instant::now();
        let expired = {
            let mut sessions = self.sessions.write().await;
            let expired: Vec<_> = sessions
                .iter()
                .filter(|(_, session)| session.expires_at <= now)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &expired {
                sessions.remove(id);
            }
            expired
        };
        for id in expired {
            self.artifacts.remove_session(&id);
            debug!(session_id = %id, "expired computer-use session reaped");
        }
    }

    async fn invalidate_observation(&self, session: &Session, observation_id: &ObservationId) {
        if let Some(record) = session
            .state
            .lock()
            .await
            .observations
            .get_mut(observation_id)
        {
            record.status = ObservationStatus::Invalid;
        }
    }
}

struct Session {
    id: SessionId,
    manifest: CapabilityManifest,
    expires_at: Instant,
    state: Mutex<SessionState>,
}

impl Session {
    fn allows_application(&self, application_id: &str) -> bool {
        self.manifest
            .allowed_application_ids
            .iter()
            .any(|allowed| allowed == application_id)
    }
}

#[derive(Default)]
struct SessionState {
    applications: HashMap<AppRef, DriverApplication>,
    application_refs: HashMap<String, AppRef>,
    windows: HashMap<WindowRef, DriverWindow>,
    window_refs: HashMap<String, WindowRef>,
    observations: HashMap<ObservationId, ObservationRecord>,
}

impl SessionState {
    fn project_application(&mut self, application: &DriverApplication) -> AppRef {
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

    fn project_window(&mut self, window: &DriverWindow) -> WindowRef {
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

    fn invalidate_window_observations(&mut self, window_ref: &WindowRef) {
        for observation in self.observations.values_mut() {
            if &observation.window_ref == window_ref {
                observation.status = ObservationStatus::Invalid;
            }
        }
    }

    fn trim_observations(&mut self, limit: usize) {
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

struct ObservationRecord {
    id: ObservationId,
    window_ref: WindowRef,
    screenshot_mapping: Option<nexus_cua_protocol::ScreenshotMapping>,
    fingerprint: String,
    created_at: Instant,
    status: ObservationStatus,
    elements: HashMap<ElementRef, DriverElement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservationStatus {
    Valid,
    InFlight,
    Invalid,
}

fn project_observation(
    observation: &DriverObservation,
    window_ref: &WindowRef,
    screenshot_mapping: Option<nexus_cua_protocol::ScreenshotMapping>,
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

fn resolve_action(
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
    point: nexus_cua_protocol::ScreenshotPoint,
    observation: &ObservationRecord,
) -> Result<nexus_cua_protocol::ScreenPoint, CuaError> {
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

fn session_unavailable() -> CuaError {
    public_error(
        ErrorCode::SessionUnavailable,
        "session is missing, expired, or closed",
        false,
        Some("open_new_session"),
    )
}

fn reference_not_found() -> CuaError {
    public_error(
        ErrorCode::ReferenceNotFound,
        "reference does not exist in this session",
        false,
        Some("refresh_references"),
    )
}

fn stale_observation() -> CuaError {
    public_error(
        ErrorCode::StaleObservation,
        "observation is expired, invalidated, or no longer current",
        true,
        Some("observe_window"),
    )
}

fn capability_denied() -> CuaError {
    public_error(
        ErrorCode::CapabilityDenied,
        "session capability does not authorize this operation",
        false,
        Some("request_new_bounded_session"),
    )
}
