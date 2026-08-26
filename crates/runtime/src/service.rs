//! Session runtime and public command execution.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration as StdDuration;

use nexus_cua_protocol::{
    ActionOutput, ApplicationSummary, Command, CommandResult, CuaError, DeliveryMode,
    DiscoverApplicationsOutput, DiscoveredApplication, DiscoveryRef, ErrorCode, ListWindowsInput,
    ObservationId, OpenSessionInput, OpenSessionOutput, PermissionMode, SessionId, SessionInput,
    VerificationOutput, VerifyStateInput, WindowObservation, WindowSummary,
};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, RwLock, Semaphore, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::artifact_store::ArtifactStore;
use crate::driver::{DesktopDriver, DriverObservation, DriverWindow};
use crate::error::{DriverErrorKind, public_error};
use crate::session::{
    ObservationStatus, Session, SessionState, project_observation, resolve_action,
    stale_observation,
};
use crate::validation::{validate_action, validate_manifest};

/// Runtime limits selected by the embedding host.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Private directory for transient screenshot artifacts.
    pub artifact_root: PathBuf,
    /// Hard maximum session lifetime.
    pub max_session_ttl: StdDuration,
    /// Lifetime of one runtime-local discovery reference. Must not exceed 30 seconds.
    pub discovery_ttl: StdDuration,
    /// Maximum age of an observation used for mutation.
    pub max_observation_age: StdDuration,
    /// Maximum retained observations per session.
    pub max_observations_per_session: usize,
    /// Maximum simultaneously live capability sessions.
    pub max_active_sessions: usize,
    /// Maximum applications returned by one discovery snapshot.
    pub max_discovered_applications: usize,
    /// Maximum unexpired discovery references retained by the runtime.
    pub max_discovery_records: usize,
    /// Maximum normalized elements in one observation.
    pub max_elements_per_observation: usize,
    /// Maximum decoded screenshot pixels.
    pub max_image_pixels: u64,
    /// Maximum screenshot files retained for one live session.
    pub max_artifacts_per_session: usize,
    /// Maximum concurrent PNG encoding and persistence workers.
    pub max_artifact_workers: usize,
}

impl RuntimeConfig {
    /// Secure defaults suitable for a local desktop product.
    pub fn new(artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            artifact_root: artifact_root.into(),
            max_session_ttl: StdDuration::from_secs(60 * 60),
            discovery_ttl: StdDuration::from_secs(30),
            max_observation_age: StdDuration::from_secs(30),
            max_observations_per_session: 32,
            max_active_sessions: 64,
            max_discovered_applications: 256,
            max_discovery_records: 2_048,
            max_elements_per_observation: 2_000,
            max_image_pixels: 100_000_000,
            max_artifacts_per_session: 32,
            max_artifact_workers: 2,
        }
    }
}

/// Capability-bounded runtime above one replaceable platform driver.
pub struct Runtime {
    driver: Arc<dyn DesktopDriver>,
    artifacts: Arc<ArtifactStore>,
    artifact_workers: Arc<Semaphore>,
    config: RuntimeConfig,
    epoch: String,
    discovery: Arc<RwLock<HashMap<DiscoveryRef, DiscoveryRecord>>>,
    sessions: Arc<RwLock<HashMap<SessionId, Arc<Session>>>>,
    scheduler: StdMutex<Option<ExpirationScheduler>>,
    lifecycle: RwLock<()>,
    shutdown_guard: Mutex<()>,
    closed: AtomicBool,
}

struct DiscoveryRecord {
    runtime_epoch: String,
    application: crate::DriverApplication,
    expires_at: Instant,
}

struct ExpirationScheduler {
    signal: watch::Sender<SchedulerSignal>,
    task: JoinHandle<()>,
}

#[derive(Clone, Copy, Debug)]
enum SchedulerSignal {
    Reschedule,
    Shutdown,
}

impl Runtime {
    /// Creates a runtime with no ambient authority.
    ///
    /// # Errors
    ///
    /// Returns an error when the private artifact root cannot be created or
    /// does not satisfy the runtime's filesystem safety requirements.
    pub fn new(driver: Arc<dyn DesktopDriver>, config: RuntimeConfig) -> Result<Self, CuaError> {
        if config.max_session_ttl.is_zero()
            || config.discovery_ttl.is_zero()
            || config.discovery_ttl > StdDuration::from_secs(30)
            || config.max_observation_age.is_zero()
            || config.max_observations_per_session == 0
            || config.max_active_sessions == 0
            || config.max_discovered_applications == 0
            || config.max_discovery_records == 0
            || config.max_discovered_applications > config.max_discovery_records
            || config.max_elements_per_observation == 0
            || config.max_image_pixels == 0
            || config.max_artifacts_per_session == 0
            || config.max_artifact_workers == 0
        {
            return Err(public_error(
                ErrorCode::InvalidRequest,
                "runtime resource and lifetime bounds must be non-zero",
                false,
                None,
            ));
        }
        let artifacts = Arc::new(ArtifactStore::new(
            &config.artifact_root,
            config.max_image_pixels,
            config.max_artifacts_per_session,
        )?);
        let artifact_workers = Arc::new(Semaphore::new(config.max_artifact_workers));
        Ok(Self {
            driver,
            artifacts,
            artifact_workers,
            config,
            epoch: format!("runtime_{}", Uuid::new_v4().simple()),
            discovery: Arc::new(RwLock::new(HashMap::new())),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            scheduler: StdMutex::new(None),
            lifecycle: RwLock::new(()),
            shutdown_guard: Mutex::new(()),
            closed: AtomicBool::new(false),
        })
    }

    /// Executes one already transport-authenticated command.
    ///
    /// # Errors
    ///
    /// Returns a stable protocol error when validation, authorization, native
    /// driver execution, or artifact persistence fails.
    pub async fn execute(&self, command: Command) -> Result<CommandResult, CuaError> {
        let _lifecycle = self.lifecycle.read().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(runtime_unavailable());
        }
        self.ensure_scheduler()?;
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
            Command::DiscoverApplications => self.discover_applications().await,
            Command::OpenSession(input) => self.open_session(input).await,
            Command::CloseSession(input) => self.close_session(input).await,
            Command::ListApps(input) => self.list_apps(input).await,
            Command::ListWindows(input) => self.list_windows(input).await,
            Command::ObserveWindow(input) => self.observe_window(input).await,
            Command::PerformAction(input) => self.perform_action(input).await.map_err(|error| {
                if error.mutation_status == nexus_cua_protocol::MutationStatus::NotApplicable {
                    error.with_mutation_status(nexus_cua_protocol::MutationStatus::NotDispatched)
                } else {
                    error
                }
            }),
            Command::VerifyState(input) => self.verify_state(input).await,
        }
    }

    /// Stops new command admission, waits for admitted commands, and removes
    /// all session artifacts before returning.
    pub async fn shutdown(&self) {
        let _shutdown = self.shutdown_guard.lock().await;
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let _lifecycle = self.lifecycle.write().await;
        let scheduler = self
            .scheduler
            .lock()
            .ok()
            .and_then(|mut scheduler| scheduler.take());
        if let Some(scheduler) = scheduler {
            scheduler.signal.send_replace(SchedulerSignal::Shutdown);
            let _ = scheduler.task.await;
        }
        self.discovery.write().await.clear();
        let session_ids = {
            let mut sessions = self.sessions.write().await;
            sessions.drain().map(|(id, _)| id).collect::<Vec<_>>()
        };
        for session_id in session_ids {
            self.artifacts.remove_session(&session_id);
        }
    }

    async fn discover_applications(&self) -> Result<CommandResult, CuaError> {
        let mut applications = self
            .driver
            .list_applications()
            .await
            .map_err(CuaError::from)?;
        applications.sort_by(|left, right| {
            (&left.application_id, &left.name, &left.key).cmp(&(
                &right.application_id,
                &right.name,
                &right.key,
            ))
        });
        applications.dedup_by(|left, right| left.key == right.key);
        let complete = applications.len() <= self.config.max_discovered_applications;
        applications.truncate(self.config.max_discovered_applications);

        let expires_at = Instant::now() + self.config.discovery_ttl;
        let expires_at_wall = OffsetDateTime::now_utc()
            + Duration::try_from(self.config.discovery_ttl).map_err(|_| {
                public_error(
                    ErrorCode::Internal,
                    "failed to calculate discovery expiry",
                    false,
                    None,
                )
            })?;
        let expires_at_text = expires_at_wall.format(&Rfc3339).map_err(|_| {
            public_error(
                ErrorCode::Internal,
                "failed to format discovery expiry",
                false,
                None,
            )
        })?;

        let mut discovery = self.discovery.write().await;
        discovery.retain(|_, record| record.expires_at > Instant::now());
        if discovery.len().saturating_add(applications.len()) > self.config.max_discovery_records {
            return Err(public_error(
                ErrorCode::Busy,
                "unexpired discovery reference capacity is exhausted",
                true,
                Some("retry_after_discovery_expiry"),
            ));
        }
        let mut output = Vec::with_capacity(applications.len());
        for application in applications {
            let discovery_ref = DiscoveryRef::new(format!("discovery_{}", Uuid::new_v4().simple()));
            output.push(DiscoveredApplication {
                discovery_ref: discovery_ref.clone(),
                name: application.name.clone(),
                application_id: application.application_id.clone(),
                foreground: application.foreground,
                provenance: application.provenance.clone(),
                expires_at: expires_at_text.clone(),
            });
            discovery.insert(
                discovery_ref,
                DiscoveryRecord {
                    runtime_epoch: self.epoch.clone(),
                    application,
                    expires_at,
                },
            );
        }
        drop(discovery);
        self.signal_reschedule()?;
        Ok(CommandResult::ApplicationsDiscovered(
            DiscoverApplicationsOutput {
                applications: output,
                complete,
            },
        ))
    }

    async fn resolve_discovery_refs(
        &self,
        requested: &[DiscoveryRef],
    ) -> Result<Vec<crate::DriverApplication>, CuaError> {
        let now = Instant::now();
        let expected = {
            let discovery = self.discovery.read().await;
            requested
                .iter()
                .map(|discovery_ref| {
                    discovery
                        .get(discovery_ref)
                        .filter(|record| {
                            record.runtime_epoch == self.epoch && record.expires_at > now
                        })
                        .map(|record| record.application.clone())
                        .ok_or_else(stale_discovery)
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        let current = self
            .driver
            .list_applications()
            .await
            .map_err(CuaError::from)?;
        let resolved = expected
            .into_iter()
            .map(|expected| {
                current
                    .iter()
                    .find(|candidate| expected.matches_generation(candidate))
                    .cloned()
                    .ok_or_else(stale_discovery)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let unique = resolved
            .iter()
            .map(|application| &application.key)
            .collect::<HashSet<_>>();
        if unique.len() != resolved.len() {
            return Err(public_error(
                ErrorCode::InvalidRequest,
                "application references resolve to duplicate process generations",
                false,
                Some("select_unique_applications"),
            ));
        }
        Ok(resolved)
    }

    async fn open_session(&self, input: OpenSessionInput) -> Result<CommandResult, CuaError> {
        let max_ttl = u32::try_from(self.config.max_session_ttl.as_secs()).unwrap_or(u32::MAX);
        validate_manifest(&input.manifest, max_ttl)?;
        let allowed_applications = self
            .resolve_discovery_refs(&input.manifest.application_refs)
            .await?;
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
            allowed_applications,
            expires_at: expires_at_monotonic,
            state: Mutex::new(SessionState::default()),
        });
        let mut sessions = self.sessions.write().await;
        if sessions.len() >= self.config.max_active_sessions {
            return Err(public_error(
                ErrorCode::Busy,
                "active session capacity is exhausted",
                true,
                Some("close_unused_session"),
            ));
        }
        sessions.insert(session_id.clone(), session);
        drop(sessions);
        self.signal_reschedule()?;
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
        self.signal_reschedule()?;
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
            .filter(|app| session.allows_application(app))
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
            .filter(|window| session.allows_application(&window.application))
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
        if !session.allows_application(&window.application) {
            return Err(capability_denied());
        }
        let mut observation = self
            .observe_driver_window(&window, input.include_screenshot, input.accessibility)
            .await?;
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
            (Some(image), Some(screen_bounds)) => Some(
                self.write_screenshot(session.id.clone(), image, screen_bounds)
                    .await?,
            ),
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

    async fn observe_driver_window(
        &self,
        window: &DriverWindow,
        include_screenshot: bool,
        accessibility: nexus_cua_protocol::AccessibilityMode,
    ) -> Result<DriverObservation, CuaError> {
        let first = self
            .driver
            .observe_window(window, include_screenshot, accessibility)
            .await;
        if matches!(
            &first,
            Err(error) if error.kind == DriverErrorKind::StaleObservation
        ) {
            return self
                .driver
                .observe_window(window, include_screenshot, accessibility)
                .await
                .map_err(Into::into);
        }
        first.map_err(Into::into)
    }

    async fn write_screenshot(
        &self,
        session_id: SessionId,
        image: crate::RgbaImage,
        screen_bounds: nexus_cua_protocol::ScreenRect,
    ) -> Result<nexus_cua_protocol::ScreenshotArtifact, CuaError> {
        let permit = Arc::clone(&self.artifact_workers)
            .acquire_owned()
            .await
            .map_err(|_| {
                public_error(
                    ErrorCode::Internal,
                    "artifact worker pool is unavailable",
                    true,
                    Some("restart_runtime"),
                )
            })?;
        let artifacts = Arc::clone(&self.artifacts);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            artifacts.write_image(&session_id, &image, screen_bounds)
        })
        .await
        .map_err(|_| {
            public_error(
                ErrorCode::Internal,
                "artifact worker stopped unexpectedly",
                true,
                Some("restart_runtime"),
            )
        })?
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
            if !session.allows_application(&window.application) {
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
        let result = result.map_err(|error| {
            let error = CuaError::from(error);
            if error.mutation_status == nexus_cua_protocol::MutationStatus::NotApplicable {
                error.with_mutation_status(nexus_cua_protocol::MutationStatus::Indeterminate)
            } else {
                error
            }
        })?;
        if result.delivery_mode == DeliveryMode::Foreground
            && !session.manifest.allow_foreground_input
        {
            warn!(session_id = %session.id, "driver exceeded foreground authorization");
            return Err(public_error(
                ErrorCode::Internal,
                "driver exceeded the authorized delivery mode",
                false,
                None,
            )
            .with_mutation_status(nexus_cua_protocol::MutationStatus::Indeterminate));
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
        if !session.allows_application(&window.application) {
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
            self.signal_reschedule()?;
            return Err(session_unavailable());
        }
        Ok(session)
    }

    fn ensure_scheduler(&self) -> Result<(), CuaError> {
        let mut scheduler = self.scheduler.lock().map_err(|_| runtime_unavailable())?;
        if scheduler.is_some() {
            return Ok(());
        }
        let (signal, receiver) = watch::channel(SchedulerSignal::Reschedule);
        let sessions = Arc::downgrade(&self.sessions);
        let discovery = Arc::downgrade(&self.discovery);
        let artifacts = Arc::downgrade(&self.artifacts);
        let task = tokio::spawn(expiration_loop(sessions, discovery, artifacts, receiver));
        *scheduler = Some(ExpirationScheduler { signal, task });
        Ok(())
    }

    fn signal_reschedule(&self) -> Result<(), CuaError> {
        let scheduler = self.scheduler.lock().map_err(|_| runtime_unavailable())?;
        if let Some(scheduler) = scheduler.as_ref() {
            scheduler.signal.send_replace(SchedulerSignal::Reschedule);
        }
        Ok(())
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

impl Drop for Runtime {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut scheduler) = self.scheduler.lock()
            && let Some(scheduler) = scheduler.take()
        {
            scheduler.signal.send_replace(SchedulerSignal::Shutdown);
        }
    }
}

async fn expiration_loop(
    sessions: Weak<RwLock<HashMap<SessionId, Arc<Session>>>>,
    discovery: Weak<RwLock<HashMap<DiscoveryRef, DiscoveryRecord>>>,
    artifacts: Weak<ArtifactStore>,
    mut receiver: watch::Receiver<SchedulerSignal>,
) {
    loop {
        if matches!(*receiver.borrow(), SchedulerSignal::Shutdown) {
            return;
        }
        let Some((sessions_live, discovery_live)) = sessions.upgrade().zip(discovery.upgrade())
        else {
            return;
        };
        let deadline = nearest_deadline(&sessions_live, &discovery_live).await;
        drop(sessions_live);
        drop(discovery_live);
        match deadline {
            Some(deadline) => {
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => {
                        let Some((sessions_live, discovery_live, artifacts_live)) = sessions
                            .upgrade()
                            .zip(discovery.upgrade())
                            .zip(artifacts.upgrade())
                            .map(|((sessions, discovery), artifacts)| (sessions, discovery, artifacts))
                        else {
                            return;
                        };
                        reap_due(
                            &sessions_live,
                            &discovery_live,
                            &artifacts_live,
                            Instant::now(),
                        ).await;
                    }
                    changed = receiver.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
            }
            None => {
                if receiver.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

async fn nearest_deadline(
    sessions: &RwLock<HashMap<SessionId, Arc<Session>>>,
    discovery: &RwLock<HashMap<DiscoveryRef, DiscoveryRecord>>,
) -> Option<Instant> {
    let session_deadline = sessions
        .read()
        .await
        .values()
        .map(|session| session.expires_at)
        .min();
    let discovery_deadline = discovery
        .read()
        .await
        .values()
        .map(|record| record.expires_at)
        .min();
    session_deadline.into_iter().chain(discovery_deadline).min()
}

async fn reap_due(
    sessions: &RwLock<HashMap<SessionId, Arc<Session>>>,
    discovery: &RwLock<HashMap<DiscoveryRef, DiscoveryRecord>>,
    artifacts: &ArtifactStore,
    now: Instant,
) {
    let expired = {
        let mut sessions = sessions.write().await;
        let expired = sessions
            .iter()
            .filter(|(_, session)| session.expires_at <= now)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for session_id in &expired {
            sessions.remove(session_id);
        }
        expired
    };
    discovery
        .write()
        .await
        .retain(|_, record| record.expires_at > now);
    for session_id in expired {
        artifacts.remove_session(&session_id);
        debug!(session_id = %session_id, "expired computer-use session reaped");
    }
}

fn session_unavailable() -> CuaError {
    public_error(
        ErrorCode::SessionUnavailable,
        "session is missing, expired, or closed",
        false,
        Some("open_new_session"),
    )
}

fn stale_discovery() -> CuaError {
    public_error(
        ErrorCode::StaleDiscovery,
        "discovery reference expired or no longer identifies the same process generation",
        true,
        Some("discover_applications"),
    )
}

fn runtime_unavailable() -> CuaError {
    public_error(
        ErrorCode::SessionUnavailable,
        "runtime is shutting down or unavailable",
        true,
        Some("restart_runtime"),
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

fn capability_denied() -> CuaError {
    public_error(
        ErrorCode::CapabilityDenied,
        "session capability does not authorize this operation",
        false,
        Some("request_new_bounded_session"),
    )
}
