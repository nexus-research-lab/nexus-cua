//! Narrow internal contract implemented by platform drivers.

use async_trait::async_trait;
use nexus_cua_protocol::{
    AccessibilityMode, AccessibilityRangeValue, ActionEffectStatus, ActionEvidenceKind,
    ApplicationProvenance, DeliveryMode, DeliveryPreference, DriverCapabilities,
    ForegroundRestorationStatus, ObservationTruncation, PermissionStatus, PointerButton,
    ScreenPoint, ScreenRect, SensitiveText, StatePredicate,
};
use zeroize::Zeroize;

use crate::DriverError;

/// Revocable, finite authority checked again on the native dispatch actor.
/// This is an internal runtime-issued guard, never a public caller credential.
#[derive(Clone)]
pub struct DriverActionGuard {
    pub(crate) interruption: std::sync::Arc<std::sync::atomic::AtomicU8>,
    pub(crate) closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) expires_at: std::time::Instant,
    pub(crate) dispatch: std::sync::Arc<crate::activity::DispatchLease>,
    pub(crate) observed_at: Option<std::time::Instant>,
}

impl DriverActionGuard {
    pub(crate) fn new(
        interruption: std::sync::Arc<std::sync::atomic::AtomicU8>,
        closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        expires_at: std::time::Instant,
        activity: std::sync::Arc<std::sync::Mutex<crate::activity::InputActivity>>,
    ) -> Self {
        Self {
            interruption,
            closed,
            expires_at,
            observed_at: None,
            dispatch: std::sync::Arc::new(crate::activity::DispatchLease {
                activity,
                dispatched: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// Records outside input without retaining its content. Between dispatches
    /// this only delays the next action and invalidates older observations.
    /// During native dispatch it latches takeover and stops partial actions.
    pub fn record_user_input(&self) {
        self.record_user_input_state(false);
    }

    /// Records outside input and whether its keys, modifiers or buttons remain
    /// held. A held input is never treated as idle merely because events stop.
    ///
    /// # Panics
    /// Panics if internal activity bookkeeping was poisoned by another panic.
    pub fn record_user_input_state(&self, input_held: bool) {
        let mut activity = self
            .dispatch
            .activity
            .lock()
            .expect("activity state contains no native objects");
        activity.last_input = Some(std::time::Instant::now());
        activity.input_held = input_held;
        if activity.active_dispatches != 0 {
            self.user_took_over();
        }
    }

    pub(crate) async fn wait_until_idle(&self) -> Result<(), DriverError> {
        self.wait_for_idle(
            crate::activity::INPUT_QUIET_PERIOD,
            crate::activity::INPUT_WAIT_LIMIT,
        )
        .await
    }

    pub(crate) fn check_observation(&self) -> Result<(), DriverError> {
        self.dispatch
            .activity
            .lock()
            .expect("activity state contains no native objects")
            .check_observation(self.observed_at)
    }

    async fn wait_for_idle(
        &self,
        quiet_period: std::time::Duration,
        limit: std::time::Duration,
    ) -> Result<(), DriverError> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            self.check()?;
            let remaining = self
                .dispatch
                .activity
                .lock()
                .expect("activity state contains no native objects")
                .remaining_quiet(quiet_period);
            if remaining.is_zero() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(DriverError::new(
                    crate::DriverErrorKind::Busy,
                    "user input is still active; wait for idle and obtain a fresh observation",
                )
                .retryable("wait_for_idle_then_observe")
                .mutation_not_dispatched());
            }
            tokio::time::sleep(remaining.min(std::time::Duration::from_millis(25))).await;
        }
    }

    /// Latches user takeover for this session, including later actions.
    pub fn user_took_over(&self) {
        let _ = self.interruption.compare_exchange(
            0,
            1,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    /// Stops session mutations when the native activity monitor loses coverage.
    pub fn activity_monitor_failed(&self) {
        let _ = self.interruption.compare_exchange(
            0,
            2,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        );
    }

    /// Fails before a new dispatch when its session was closed or expired.
    ///
    /// # Errors
    /// Returns session unavailable with the action's dispatch status when revoked.
    pub fn check(&self) -> Result<(), DriverError> {
        if self.closed.load(std::sync::atomic::Ordering::Acquire)
            || std::time::Instant::now() >= self.expires_at
        {
            let error = DriverError::new(
                crate::DriverErrorKind::SessionUnavailable,
                "action authority was closed or expired",
            );
            return Err(if self.was_dispatched() {
                error.mutation_indeterminate()
            } else {
                error.mutation_not_dispatched()
            });
        }
        let interruption = self.interruption.load(std::sync::atomic::Ordering::Acquire);
        if interruption != 0 {
            let mut error = if interruption == 1 {
                DriverError::new(
                    crate::DriverErrorKind::UserActive,
                    "user input took over this session; host authorization is required to resume",
                )
            } else {
                DriverError::new(
                    crate::DriverErrorKind::Platform,
                    "user activity monitoring lost coverage; this session cannot resume",
                )
            };
            error.recovery_action = Some("request_new_bounded_session".to_owned());
            return Err(if self.was_dispatched() {
                error.mutation_indeterminate()
            } else {
                error.mutation_not_dispatched()
            });
        }
        Ok(())
    }

    /// Checks authority immediately before a native mutation attempt. Later
    /// cancellation is indeterminate because part of the action may have happened.
    ///
    /// # Errors
    /// Returns revoked authority, takeover, recent activity or stale observation.
    ///
    /// # Panics
    /// Panics if internal activity bookkeeping was poisoned by another panic.
    pub fn begin_dispatch(&self) -> Result<(), DriverError> {
        let mut activity = self
            .dispatch
            .activity
            .lock()
            .expect("activity state contains no native objects");
        self.check()?;
        if !self.was_dispatched() {
            activity.check_observation(self.observed_at)?;
            if !activity
                .remaining_quiet(crate::activity::INPUT_QUIET_PERIOD)
                .is_zero()
            {
                return Err(DriverError::new(
                    crate::DriverErrorKind::Busy,
                    "user input resumed before dispatch; wait for idle and observe again",
                )
                .retryable("wait_for_idle_then_observe")
                .mutation_not_dispatched());
            }
            activity.active_dispatches += 1;
            self.dispatch
                .dispatched
                .store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }

    /// Whether any part of this action has reached a native dispatch boundary.
    pub fn was_dispatched(&self) -> bool {
        self.dispatch
            .dispatched
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Internal application identity owned by a platform driver.
#[derive(Clone, Debug)]
pub struct DriverApplication {
    /// Stable key within the lifetime of one driver process.
    pub key: String,
    /// Platform process generation, such as launch time plus process identity.
    pub process_generation: String,
    /// Normalized executable or bundle identity used for TOCTOU revalidation.
    pub identity: String,
    /// Human-readable display name.
    pub name: String,
    /// Bundle identifier or executable identity matched by manifests.
    pub application_id: String,
    /// Whether the application currently owns foreground input.
    pub foreground: bool,
    /// Public best-effort provenance summary without native authority handles.
    pub provenance: ApplicationProvenance,
}

/// Internal installed-application identity owned by a platform driver.
#[derive(Clone, Debug)]
pub struct DriverLaunchableApplication {
    /// Stable native registration key, never exposed as public authority.
    pub key: String,
    /// Driver-owned native launch target; public callers never supply it.
    pub launch_target: String,
    /// Normalized executable/bundle identity used for revalidation.
    pub identity: String,
    /// Human-readable display name.
    pub name: String,
    /// Stable bundle, package, or executable identity.
    pub application_id: String,
    /// Public best-effort provenance summary.
    pub provenance: ApplicationProvenance,
}

impl DriverLaunchableApplication {
    /// Returns true only when native rediscovery produced the same installed identity.
    pub fn matches_identity(&self, current: &Self) -> bool {
        self.key == current.key
            && self.launch_target == current.launch_target
            && self.identity == current.identity
            && self.application_id == current.application_id
            && self.provenance == current.provenance
    }
}

/// Native cold-launch result.
pub struct DriverLaunchOutput {
    /// Newly started process generation.
    pub application: DriverApplication,
    /// Native delivery route used by launch.
    pub delivery_mode: DeliveryMode,
}

impl DriverApplication {
    /// Returns true only for the same running process generation and identity.
    pub fn matches_generation(&self, current: &Self) -> bool {
        self.key == current.key
            && self.process_generation == current.process_generation
            && self.identity == current.identity
            && self.application_id == current.application_id
    }
}

/// Internal top-level window identity owned by a platform driver.
#[derive(Clone, Debug)]
pub struct DriverWindow {
    /// Stable key within the lifetime of one driver process.
    pub key: String,
    /// Owning application.
    pub application: DriverApplication,
    /// Current title.
    pub title: String,
    /// Logical top-left screen bounds.
    pub screen_bounds: ScreenRect,
    /// Best-effort state; state-query failures must not remove discovered windows.
    pub state: nexus_cua_protocol::WindowState,
    /// Whether the window is visible on a display.
    pub visible: bool,
    /// Whether the window owns foreground input.
    pub foreground: bool,
}

/// Internal physical-display identity owned by a platform driver.
#[derive(Clone, Debug)]
pub struct DriverDisplay {
    /// Stable key within the lifetime of one driver process.
    pub key: String,
    /// Logical top-left bounds in the multi-display desktop coordinate space.
    pub screen_bounds: ScreenRect,
    /// Whether this display owns the operating-system primary origin.
    pub primary: bool,
}

/// Raw RGBA screenshot returned to the runtime for validated PNG encoding.
pub struct RgbaImage {
    /// Physical pixel width.
    pub width: u32,
    /// Physical pixel height.
    pub height: u32,
    /// Row-major RGBA8 pixels.
    pub pixels: Vec<u8>,
    recycler: Option<Box<dyn FnOnce(Vec<u8>) + Send + 'static>>,
}

impl RgbaImage {
    /// Creates an image whose pixel storage is released normally.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels,
            recycler: None,
        }
    }

    /// Creates an image whose storage returns to a bounded driver-owned pool.
    pub fn with_recycler(
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        recycler: impl FnOnce(Vec<u8>) + Send + 'static,
    ) -> Self {
        Self {
            width,
            height,
            pixels,
            recycler: Some(Box::new(recycler)),
        }
    }
}

impl Drop for RgbaImage {
    fn drop(&mut self) {
        self.pixels.zeroize();
        if let Some(recycler) = self.recycler.take() {
            recycler(std::mem::take(&mut self.pixels));
        }
    }
}

/// Internal accessibility element with a driver-owned key.
#[derive(Clone, Debug)]
pub struct DriverElement {
    /// Stable only within the containing driver observation.
    pub key: String,
    /// Optional parent key from the same observation.
    pub parent_key: Option<String>,
    /// Normalized role.
    pub role: String,
    /// Accessible label.
    pub name: String,
    /// Non-sensitive display value when available.
    pub value: Option<String>,
    /// Numeric range metadata when the native provider exposes it coherently.
    pub range_value: Option<AccessibilityRangeValue>,
    /// Logical top-left screen bounds.
    pub screen_bounds: Option<ScreenRect>,
    /// Whether actions are currently accepted.
    pub enabled: bool,
    /// Whether the element owns keyboard focus.
    pub focused: bool,
    /// Whether the platform reports the element as selected.
    pub selected: Option<bool>,
    /// Whether the platform reports the element as checked.
    pub checked: Option<bool>,
    /// Whether the platform reports the element as expanded.
    pub expanded: Option<bool>,
    /// Normalized semantic actions.
    pub actions: Vec<String>,
}

/// Immutable platform observation before public reference projection.
pub struct DriverObservation {
    /// Exact driver window key.
    pub window_key: String,
    /// Logical window bounds.
    pub window_screen_bounds: ScreenRect,
    /// Optional raw image.
    pub screenshot: Option<RgbaImage>,
    /// Logical screen rectangle represented by the raw image.
    pub screenshot_screen_bounds: Option<ScreenRect>,
    /// Normalized elements.
    pub elements: Vec<DriverElement>,
    /// False when the platform tree is partial.
    pub elements_complete: bool,
    /// Stable reason when the semantic tree is partial.
    pub elements_truncation: Option<ObservationTruncation>,
    /// Driver-computed opaque state fingerprint.
    pub fingerprint: String,
}

/// Resolved action containing only driver-owned target keys.
pub enum DriverAction {
    /// Bring the target window to the foreground.
    FocusWindow,
    /// Move and resize the exact target window.
    SetWindowBounds {
        /// Requested logical top-left screen bounds.
        bounds: ScreenRect,
    },
    /// Minimize or restore the exact target window.
    SetWindowMinimized {
        /// Requested minimized state.
        minimized: bool,
    },
    /// Cooperatively terminate the exact application process generation.
    TerminateApplication,
    /// Move semantic focus to one accessibility element.
    FocusElement {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
    },
    /// Invoke one accessibility element.
    InvokeElement {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
    },
    /// Click a runtime-resolved logical screen point.
    ClickPoint {
        /// Logical screen coordinate resolved from the guarded screenshot.
        point: ScreenPoint,
        /// Requested pointer button.
        button: PointerButton,
        /// Validated click count.
        count: u8,
        /// Requested foreground or exact-target background route.
        delivery: DeliveryPreference,
    },
    /// Hold and release a pointer button at one resolved logical screen point.
    PressPoint {
        /// Logical screen coordinate resolved from the guarded screenshot.
        point: ScreenPoint,
        /// Requested pointer button.
        button: PointerButton,
        /// Validated bounded hold duration.
        duration_ms: u32,
    },
    /// Replace one accessibility value.
    SetValue {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
        /// Sensitive value delivered only to the native driver.
        value: SensitiveText,
    },
    /// Set one native numeric range pattern.
    SetRangeValue {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
        /// Runtime-validated finite value within the observed range.
        value: f64,
    },
    /// Toggle one accessibility element.
    ToggleElement {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
    },
    /// Select one accessibility element.
    SelectElement {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
    },
    /// Expand or collapse one accessibility element.
    SetExpanded {
        /// Driver-owned element identity from the guarded observation.
        element_key: String,
        /// Desired expanded state.
        expanded: bool,
    },
    /// Move the pointer without clicking.
    MovePointer {
        /// Logical screen coordinate resolved from the guarded screenshot.
        point: ScreenPoint,
        /// Bounded movement duration.
        duration_ms: u32,
    },
    /// Type foreground text.
    TypeText {
        /// Sensitive text delivered only to the native driver.
        text: SensitiveText,
    },
    /// Press one normalized key chord.
    PressKeys {
        /// Normalized key names forming one chord or key sequence.
        keys: Vec<String>,
    },
    /// Scroll the foreground surface at the explicit target point.
    Scroll {
        /// Resolved target point in logical screen coordinates.
        point: ScreenPoint,
        /// Horizontal logical scroll delta.
        delta_x: f64,
        /// Vertical logical scroll delta.
        delta_y: f64,
    },
    /// Drag between runtime-resolved logical screen points.
    Drag {
        /// Logical starting screen coordinate.
        from: ScreenPoint,
        /// Logical ending screen coordinate.
        to: ScreenPoint,
        /// Validated drag duration.
        duration_ms: u32,
    },
}

impl DriverAction {
    /// Returns whether the action temporarily activates a target for input and
    /// can therefore restore a prior foreground owner afterwards.
    pub fn supports_foreground_restoration(&self) -> bool {
        matches!(
            self,
            Self::ClickPoint {
                delivery: DeliveryPreference::Foreground,
                ..
            } | Self::PressPoint { .. }
                | Self::MovePointer { .. }
                | Self::TypeText { .. }
                | Self::PressKeys { .. }
                | Self::Scroll { .. }
                | Self::Drag { .. }
        )
    }
}

/// Successful platform dispatch result.
#[derive(Clone, Copy, Debug)]
pub struct DriverActionOutput {
    /// Route actually used. It must not exceed runtime authorization.
    pub delivery_mode: DeliveryMode,
    /// Direct effect classification established by the owning native actor.
    pub effect_status: ActionEffectStatus,
    /// Closed evidence class supporting the effect classification.
    pub evidence_kind: ActionEvidenceKind,
    /// Outcome of the bounded prior-foreground restoration attempt.
    pub foreground_restoration: ForegroundRestorationStatus,
}

impl DriverActionOutput {
    /// Builds an output backed by bounded native state read-back.
    pub fn confirmed(delivery_mode: DeliveryMode) -> Self {
        Self {
            delivery_mode,
            effect_status: ActionEffectStatus::Confirmed,
            evidence_kind: ActionEvidenceKind::NativeReadback,
            foreground_restoration: ForegroundRestorationStatus::NotRequested,
        }
    }

    /// Builds an output that proves dispatch only; callers must observe effect.
    pub fn unverified(delivery_mode: DeliveryMode) -> Self {
        Self {
            delivery_mode,
            effect_status: ActionEffectStatus::Unverified,
            evidence_kind: ActionEvidenceKind::DispatchAcceptance,
            foreground_restoration: ForegroundRestorationStatus::NotRequested,
        }
    }

    /// Attaches the native actor's truthful restoration outcome.
    #[must_use]
    pub fn with_foreground_restoration(mut self, status: ForegroundRestorationStatus) -> Self {
        self.foreground_restoration = status;
        self
    }

    /// Returns whether the closed status/evidence pair is internally coherent.
    pub fn evidence_is_coherent(&self) -> bool {
        self.effect_status.is_supported_by(self.evidence_kind)
    }
}

/// Deterministic platform verification result.
#[derive(Clone, Debug)]
pub struct DriverVerification {
    /// Whether the predicate matched.
    pub matched: bool,
    /// Concise non-sensitive evidence.
    pub evidence: String,
}

/// Bounded native plain-text clipboard snapshot. The opaque change marker is
/// retained only behind a session-scoped public guard.
pub struct DriverClipboardSnapshot {
    /// Platform generation marker used for compare-and-replace.
    pub change_marker: String,
    /// Plain text, or `None` when no supported text format is available.
    pub text: Option<SensitiveText>,
}

/// Native result of a guarded clipboard replacement.
pub struct DriverClipboardWriteOutput {
    /// Platform generation marker after the replacement.
    pub change_marker: String,
    /// Truthful effect classification.
    pub effect_status: ActionEffectStatus,
    /// Evidence supporting `effect_status`.
    pub evidence_kind: ActionEvidenceKind,
}

/// Replaceable platform implementation beneath the authorization runtime.
#[async_trait]
pub trait DesktopDriver: Send + Sync {
    /// Fixes the model capture size before sessions are admitted. Drivers that
    /// cannot resize natively rely on the bounded artifact encoder fallback.
    ///
    /// # Errors
    /// Returns an error if the driver cannot apply the configured image limit.
    fn configure_max_image_edge(&self, _max_image_edge: u32) -> Result<(), DriverError> {
        Ok(())
    }

    /// Registers a bounded runtime session for native capture resource ownership.
    ///
    /// # Errors
    /// Returns an error if the native capture ownership budget is exhausted.
    fn session_started(
        &self,
        _session_id: &nexus_cua_protocol::SessionId,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// Registers native session resources and optional user-takeover protection.
    ///
    /// # Errors
    /// Returns an error if native resources or required monitoring are unavailable.
    fn session_started_guarded(
        &self,
        session_id: &nexus_cua_protocol::SessionId,
        _guard: DriverActionGuard,
        _allow_foreground: bool,
    ) -> Result<(), DriverError> {
        self.session_started(session_id)
    }

    /// Revokes capture ownership immediately; native resources are released on their actor.
    fn session_closed(&self, _session_id: &nexus_cua_protocol::SessionId) {}

    /// Performs bounded read-only settling before a requested post-action observation.
    ///
    /// # Errors
    /// Returns a native read failure without changing the action's recorded outcome.
    async fn settle_window(
        &self,
        _window: &DriverWindow,
        _application_scene: bool,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// Observes a target while binding retained capture resources to its authority lifetime.
    async fn observe_window_scoped(
        &self,
        _session_id: &nexus_cua_protocol::SessionId,
        window: &DriverWindow,
        include_screenshot: bool,
        accessibility: AccessibilityMode,
    ) -> Result<DriverObservation, DriverError> {
        self.observe_window(window, include_screenshot, accessibility)
            .await
    }

    /// Captures only the authorized application on the anchor display.
    /// Drivers without scene isolation must reject rather than expose the desktop.
    async fn observe_application_scene_scoped(
        &self,
        _session_id: &nexus_cua_protocol::SessionId,
        _window: &DriverWindow,
        _include_screenshot: bool,
        _accessibility: AccessibilityMode,
    ) -> Result<DriverObservation, DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "application scene capture is unavailable",
        ))
    }

    /// Returns immutable capability metadata.
    async fn capabilities(&self) -> Result<DriverCapabilities, DriverError>;

    /// Reads current host permission state without prompting.
    async fn permission_status(&self) -> Result<PermissionStatus, DriverError>;

    /// Lists running user applications.
    async fn list_applications(&self) -> Result<Vec<DriverApplication>, DriverError>;

    /// Lists installed application identities accepted by the native launcher.
    async fn list_launchable_applications(
        &self,
    ) -> Result<Vec<DriverLaunchableApplication>, DriverError> {
        Ok(Vec::new())
    }

    /// Starts one freshly revalidated installed application identity.
    async fn launch_application(
        &self,
        _application: &DriverLaunchableApplication,
        _allow_foreground_activation: bool,
    ) -> Result<DriverLaunchOutput, DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "installed application launch is unsupported",
        )
        .mutation_not_dispatched())
    }

    /// Enumerates once, returning only windows owned by the authorized application keys.
    async fn list_windows(
        &self,
        application_keys: &[String],
    ) -> Result<Vec<DriverWindow>, DriverError>;

    /// Lists physical displays available to the current interactive desktop.
    async fn list_displays(&self) -> Result<Vec<DriverDisplay>, DriverError> {
        Ok(Vec::new())
    }

    /// Captures one freshly revalidated physical display without creating
    /// action authority.
    async fn observe_display(
        &self,
        _display: &DriverDisplay,
    ) -> Result<(RgbaImage, ScreenRect), DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "physical display capture is unsupported",
        ))
    }

    /// Observes one exact driver window.
    async fn observe_window(
        &self,
        window: &DriverWindow,
        include_screenshot: bool,
        accessibility: AccessibilityMode,
    ) -> Result<DriverObservation, DriverError>;

    /// Reads bounded plain text from the current interactive desktop clipboard.
    async fn read_clipboard(
        &self,
        _max_text_bytes: usize,
    ) -> Result<DriverClipboardSnapshot, DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "plain-text clipboard reads are unsupported",
        ))
    }

    /// Replaces clipboard text only when the native generation still matches.
    async fn write_clipboard(
        &self,
        _expected_change_marker: &str,
        _text: &SensitiveText,
        _max_text_bytes: usize,
    ) -> Result<DriverClipboardWriteOutput, DriverError> {
        Err(DriverError::new(
            crate::DriverErrorKind::Unsupported,
            "plain-text clipboard writes are unsupported",
        )
        .mutation_not_dispatched())
    }

    /// Confirms that a prior observation fingerprint still describes the target.
    async fn observation_is_current(
        &self,
        window: &DriverWindow,
        fingerprint: &str,
    ) -> Result<bool, DriverError>;

    /// Executes with finite revocable authority. Native drivers must check this
    /// guard on their dispatch actor and between bounded input chunks.
    async fn perform_action_scoped(
        &self,
        window: &DriverWindow,
        action: DriverAction,
        allow_foreground: bool,
        restore_foreground: bool,
        guard: DriverActionGuard,
    ) -> Result<DriverActionOutput, DriverError> {
        guard.check()?;
        self.perform_action(window, action, allow_foreground, restore_foreground)
            .await
    }

    /// Executes one already-authorized action.
    ///
    /// `allow_foreground` is a hard upper bound. Implementations must return
    /// [`crate::DriverErrorKind::ForegroundRequired`] instead of silently
    /// activating the target when it is false.
    async fn perform_action(
        &self,
        window: &DriverWindow,
        action: DriverAction,
        allow_foreground: bool,
        restore_foreground: bool,
    ) -> Result<DriverActionOutput, DriverError>;

    /// Evaluates a deterministic predicate against fresh state.
    async fn verify_state(
        &self,
        window: &DriverWindow,
        predicate: &StatePredicate,
    ) -> Result<DriverVerification, DriverError>;
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::RgbaImage;

    #[test]
    fn recycled_pixels_are_zeroized_before_returning_to_the_driver() {
        let returned = Arc::new(Mutex::new(None));
        let output = Arc::clone(&returned);
        drop(RgbaImage::with_recycler(
            2,
            2,
            vec![0x5a; 16],
            move |pixels| *output.lock().unwrap() = Some(pixels),
        ));
        let pixels = returned.lock().unwrap().take().unwrap();
        assert!(pixels.is_empty());
        assert!(pixels.capacity() >= 16);
    }
}

#[cfg(test)]
mod action_guard_tests {
    use super::DriverActionGuard;
    use nexus_cua_protocol::MutationStatus;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};

    fn fresh_guard() -> DriverActionGuard {
        DriverActionGuard::new(
            Arc::new(std::sync::atomic::AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Instant::now() + Duration::from_secs(60),
            Arc::default(),
        )
    }

    #[test]
    fn takeover_latches_across_actions_without_claiming_rollback() {
        let guard = fresh_guard();
        let next = DriverActionGuard::new(
            Arc::clone(&guard.interruption),
            Arc::clone(&guard.closed),
            guard.expires_at,
            Arc::clone(&guard.dispatch.activity),
        );
        guard.begin_dispatch().unwrap();
        guard.user_took_over();
        let partial = guard.check().unwrap_err();
        assert_eq!(partial.kind, crate::DriverErrorKind::UserActive);
        assert_eq!(partial.mutation_status, MutationStatus::Indeterminate);
        let queued = next.begin_dispatch().unwrap_err();
        assert_eq!(queued.kind, crate::DriverErrorKind::UserActive);
        assert_eq!(queued.mutation_status, MutationStatus::NotDispatched);
        assert!(!queued.retryable);
        assert!(fresh_guard().begin_dispatch().is_ok());
    }

    #[test]
    fn monitor_failure_stops_input_without_inventing_user_activity() {
        let guard = fresh_guard();
        guard.activity_monitor_failed();
        guard.user_took_over();
        let failure = guard.begin_dispatch().unwrap_err();
        assert_eq!(failure.kind, crate::DriverErrorKind::Platform);
        assert_eq!(failure.mutation_status, MutationStatus::NotDispatched);
        assert!(!failure.retryable);
    }

    #[test]
    fn revocation_distinguishes_queued_actions_from_partial_dispatch() {
        let closed = Arc::new(AtomicBool::new(false));
        let guard = DriverActionGuard::new(
            Arc::new(std::sync::atomic::AtomicU8::new(0)),
            Arc::clone(&closed),
            Instant::now() + Duration::from_secs(60),
            Arc::default(),
        );
        let queued = guard.clone();
        closed.store(true, Ordering::Release);
        assert_eq!(
            queued.begin_dispatch().unwrap_err().mutation_status,
            MutationStatus::NotDispatched
        );
        closed.store(false, Ordering::Release);
        guard.begin_dispatch().unwrap();
        closed.store(true, Ordering::Release);
        assert_eq!(
            queued.check().unwrap_err().mutation_status,
            MutationStatus::Indeterminate
        );
    }

    #[test]
    fn expired_authority_cannot_start_native_dispatch() {
        let guard = DriverActionGuard::new(
            Arc::new(std::sync::atomic::AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
            Instant::now(),
            Arc::default(),
        );
        assert_eq!(
            guard.begin_dispatch().unwrap_err().mutation_status,
            MutationStatus::NotDispatched
        );
        assert!(!guard.was_dispatched());
    }

    #[tokio::test]
    async fn between_action_input_waits_without_revoking_authority() {
        let mut guard = fresh_guard();
        guard.observed_at = Some(Instant::now());
        guard.record_user_input();
        assert!(guard.check().is_ok());
        guard
            .wait_for_idle(Duration::from_millis(5), Duration::from_secs(1))
            .await
            .unwrap();
        let error = guard.begin_dispatch().unwrap_err();
        assert_eq!(error.kind, crate::DriverErrorKind::StaleObservation);
        assert_eq!(error.mutation_status, MutationStatus::NotDispatched);
        // Model must observe again after the actual quiet period, not reuse
        // pre-input coordinates or silently replay a partially executed action.
        guard.dispatch.activity.lock().unwrap().last_input =
            Some(Instant::now().checked_sub(Duration::from_secs(3)).unwrap());
        guard.observed_at = Some(Instant::now());
        assert!(guard.begin_dispatch().is_ok());
        guard.record_user_input();
        assert_eq!(
            guard.check().unwrap_err().kind,
            crate::DriverErrorKind::UserActive
        );
    }

    #[tokio::test]
    async fn idle_wait_is_bounded_and_revocable() {
        let guard = fresh_guard();
        guard.record_user_input();
        let error = guard
            .wait_for_idle(Duration::from_secs(2), Duration::from_millis(5))
            .await
            .unwrap_err();
        assert_eq!(error.kind, crate::DriverErrorKind::Busy);
        assert_eq!(error.mutation_status, MutationStatus::NotDispatched);
        assert!(guard.check().is_ok());
        guard.closed.store(true, Ordering::Release);
        assert_eq!(
            guard.wait_until_idle().await.unwrap_err().kind,
            crate::DriverErrorKind::SessionUnavailable
        );
    }

    #[test]
    fn held_user_input_never_becomes_idle_just_from_elapsed_time() {
        let guard = fresh_guard();
        guard.record_user_input_state(true);
        guard.dispatch.activity.lock().unwrap().last_input =
            Some(Instant::now().checked_sub(Duration::from_secs(10)).unwrap());
        assert_eq!(
            guard.begin_dispatch().unwrap_err().kind,
            crate::DriverErrorKind::Busy
        );
        assert!(guard.check().is_ok());
    }

    #[test]
    fn native_actor_clones_keep_takeover_coverage_until_dispatch_finishes() {
        let monitor = fresh_guard();
        let action = DriverActionGuard::new(
            Arc::clone(&monitor.interruption),
            Arc::clone(&monitor.closed),
            monitor.expires_at,
            Arc::clone(&monitor.dispatch.activity),
        );
        let actor_job = action.clone();
        action.begin_dispatch().unwrap();
        drop(action);
        assert_eq!(
            monitor.dispatch.activity.lock().unwrap().active_dispatches,
            1
        );
        drop(actor_job);
        assert_eq!(
            monitor.dispatch.activity.lock().unwrap().active_dispatches,
            0
        );
        monitor.record_user_input();
        assert!(monitor.check().is_ok());
    }
}
