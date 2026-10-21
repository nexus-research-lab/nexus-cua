//! Bounded MCP tool surface over one embedded runtime session.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use clap::ValueEnum;
use nexus_cua_protocol::{
    AccessibilityMode, Action, AppRef, ApplicationLifecycleOperation, AuthorizationToken,
    CapabilityManifest, ClipboardRef, Command, CommandResult, CuaError, DeliveryPreference,
    DiscoveryRef, DisplayRef, ElementRef, LaunchApplicationInput, ListWindowsInput, ObservationId,
    ObserveDisplayInput, ObserveWindowInput, OpenSessionInput, PROTOCOL_VERSION,
    PerformActionInput, PixelSize, PointerButton, ReadClipboardInput, RequestEnvelope, RequestId,
    ResponseOutcome, ScreenshotMapping, ScreenshotPoint, SensitiveText, SessionId, SessionInput,
    StatePredicate, VerifyStateInput, WindowRef, WriteClipboardInput,
};
use nexus_cua_runtime::Runtime;
use nexus_cua_transport::Dispatcher;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{
    ErrorData as McpError, ServerHandler, handler::server::wrapper::Parameters, tool, tool_handler,
    tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::authority::{AuthorityContext, AuthorityPolicy};

const SERVER_INSTRUCTIONS: &str = "Nexus CUA controls only applications fixed by host authority. Start with get_status; when it returns an authority-provider request_namespace, prefix every mutation request_id with that exact namespace followed by a colon. When separately authorized, list_displays and observe_display provide a read-only desktop overview; a display observation can never guard an action, so select and observe an exact window before mutating. Clipboard access is separately authorized: call read_clipboard immediately before write_clipboard and pass its fresh opaque clipboard_ref; never log or retain clipboard text. If no authorized application is running, call list_launchable_apps and launch_application; launch accepts only an exact listed stable identity, never a path or arguments. Call list_windows, then observe_window before every guarded mutation. Every mutation requires a stable host-generated request_id: preserve it and use reconcile_mutation with that same ID after a timeout or indeterminate result; never repeat the intent under a new ID. Visual coordinates use a top-left 0..1000 space independent of image resizing. Use zoom for a cropped view and its opaque view_ref with click_view_normalized; otherwise use click_normalized. Use the flat type_text, press_keys, scroll, drag_normalized, invoke_element, set_element_value, and set_range_value tools when advertised; the generic perform_action tool is reserved for action variants without a flat tool and is hidden by the vision profile. Pixel clicks default to foreground; request background only when the exact target should remain inactive, then observe to verify effect and explicitly retry foreground only if authorized. A successful action reports either confirmed/native_readback or unverified/dispatch_acceptance; always establish fresh application state after an unverified result. Use terminate_application only with explicit host authority; it cooperatively exits the exact application generation that owned the observed window. Use only opaque refs from this server. A successful mutation invalidates the observation, so observe again before the next action. Never retry an indeterminate mutation as a new action. Screenshot, accessibility, and clipboard content are transient and must not be logged or persisted.";
const MAX_CACHED_IMAGE_MAPPINGS: usize = 32;
const MAX_CACHED_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CACHED_REGION_VIEWS: usize = 32;
const MAX_DECODED_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
const NORMALIZED_COORDINATE_MAX: u16 = 1_000;
const MUTATION_MAX_INFLIGHT: usize = 64;
const MUTATION_MAX_COMPLETED: usize = 4_096;
const MUTATION_RECONCILIATION_TTL: Duration = Duration::from_secs(10 * 60);
const MUTATION_MAX_WAIT_MS: u32 = 120_000;
/// Stable MCP handshake identity used with the host registration alias.
pub const MCP_SERVER_NAME: &str = "nexuscua";
const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V_2026_07_28];

#[derive(Clone, Copy)]
enum ProtocolProfile {
    CompatibleStdio,
    CurrentHttp,
}

/// Model-facing MCP schema profile selected by the trusted process host.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ToolProfile {
    /// Advertise the complete typed action union and all convenience tools.
    #[default]
    Full,
    /// Hide the complex action union and advertise only flat, image-oriented tools.
    Vision,
}

/// One host-authorized MCP process and runtime session.
#[derive(Clone)]
pub struct NexusCuaMcp {
    runtime: Arc<Runtime>,
    mutation_dispatcher: Arc<Dispatcher>,
    mutation_authorization: AuthorizationToken,
    session: Arc<Mutex<Option<ActiveSession>>>,
    operation_lock: Arc<Mutex<()>>,
    authority: AuthorityContext,
    max_image_bytes: u64,
    image_mappings: Arc<Mutex<ImageMappingCache>>,
    tool_profile: ToolProfile,
    protocol_profile: ProtocolProfile,
    tool_router: ToolRouter<Self>,
}

#[derive(Clone)]
struct ActiveSession {
    session_id: SessionId,
    expires_at: String,
}

#[derive(Default)]
struct ImageMappingCache {
    entries: HashMap<ObservationId, CachedImage>,
    order: VecDeque<ObservationId>,
    cached_bytes: u64,
    views: HashMap<String, RegionView>,
    view_order: VecDeque<String>,
}

#[derive(Clone)]
struct CachedImage {
    window_ref: WindowRef,
    mapping: ScreenshotMapping,
    bytes: Arc<[u8]>,
}

#[derive(Clone)]
struct RegionView {
    observation_id: ObservationId,
    window_ref: WindowRef,
    source_rect: PixelRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl ImageMappingCache {
    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.cached_bytes = 0;
        self.views.clear();
        self.view_order.clear();
    }

    fn insert(
        &mut self,
        observation_id: ObservationId,
        window_ref: WindowRef,
        mapping: ScreenshotMapping,
        bytes: Vec<u8>,
    ) {
        self.invalidate_window(&window_ref);
        let byte_length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        while self.entries.len() >= MAX_CACHED_IMAGE_MAPPINGS
            || self.cached_bytes.saturating_add(byte_length) > MAX_CACHED_IMAGE_BYTES
        {
            if let Some(stale) = self.order.pop_front() {
                if let Some(removed) = self.entries.remove(&stale) {
                    self.cached_bytes = self
                        .cached_bytes
                        .saturating_sub(u64::try_from(removed.bytes.len()).unwrap_or(u64::MAX));
                }
            } else {
                break;
            }
        }
        let bytes: Arc<[u8]> = bytes.into();
        self.order.push_back(observation_id.clone());
        self.cached_bytes = self.cached_bytes.saturating_add(byte_length);
        self.entries.insert(
            observation_id,
            CachedImage {
                window_ref,
                mapping,
                bytes,
            },
        );
    }

    fn get(&self, observation_id: &ObservationId, window_ref: &WindowRef) -> Option<CachedImage> {
        self.entries
            .get(observation_id)
            .filter(|image| &image.window_ref == window_ref)
            .cloned()
    }

    fn insert_view(&mut self, view_ref: String, view: RegionView) {
        while self.views.len() >= MAX_CACHED_REGION_VIEWS {
            if let Some(stale) = self.view_order.pop_front() {
                self.views.remove(&stale);
            } else {
                break;
            }
        }
        self.view_order.push_back(view_ref.clone());
        self.views.insert(view_ref, view);
    }

    fn get_view(&self, view_ref: &str) -> Option<RegionView> {
        self.views.get(view_ref).cloned()
    }

    fn invalidate_window(&mut self, window_ref: &WindowRef) {
        let removed_bytes = self
            .entries
            .values()
            .filter(|image| &image.window_ref == window_ref)
            .map(|image| u64::try_from(image.bytes.len()).unwrap_or(u64::MAX))
            .fold(0_u64, u64::saturating_add);
        self.entries
            .retain(|_, image| &image.window_ref != window_ref);
        self.cached_bytes = self.cached_bytes.saturating_sub(removed_bytes);
        self.order
            .retain(|observation_id| self.entries.contains_key(observation_id));
        self.views.retain(|_, view| &view.window_ref != window_ref);
        self.view_order
            .retain(|view_ref| self.views.contains_key(view_ref));
    }
}

impl NexusCuaMcp {
    /// Resolves authenticated provider authority into a fresh runtime session.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError`] when native discovery is incomplete, a
    /// configured application is unavailable, or session admission fails.
    pub async fn bootstrap_with_authority_context(
        runtime: Arc<Runtime>,
        authority: AuthorityContext,
        max_image_bytes: u64,
        tool_profile: ToolProfile,
    ) -> Result<Self, BootstrapError> {
        let (application_refs, missing) =
            authorized_running_applications(&runtime, &authority).await?;
        if !missing.is_empty() {
            if !authority.allow_application_launch {
                return Err(BootstrapError::ApplicationsUnavailable(missing));
            }
            verify_launchable_applications(&runtime, &missing).await?;
        }
        let session = if application_refs.is_empty()
            && !authority.allow_display_capture
            && !authority.allow_clipboard_read
            && !authority.allow_clipboard_write
        {
            None
        } else {
            Some(open_runtime_session(&runtime, &authority, application_refs).await?)
        };
        let mutation_authorization = AuthorizationToken::new(format!(
            "mcp_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        ));
        let mutation_dispatcher = Arc::new(
            Dispatcher::new(
                Arc::clone(&runtime),
                &mutation_authorization,
                MUTATION_MAX_INFLIGHT,
                MUTATION_MAX_COMPLETED,
                MUTATION_RECONCILIATION_TTL,
                MUTATION_MAX_WAIT_MS,
            )
            .map_err(|error| BootstrapError::Transport(error.to_string()))?,
        );
        Ok(Self {
            runtime,
            mutation_dispatcher,
            mutation_authorization,
            session: Arc::new(Mutex::new(session)),
            operation_lock: Arc::new(Mutex::new(())),
            authority,
            max_image_bytes,
            image_mappings: Arc::new(Mutex::new(ImageMappingCache::default())),
            tool_profile,
            protocol_profile: ProtocolProfile::CompatibleStdio,
            tool_router: Self::router_for_profile(tool_profile),
        })
    }

    pub(crate) fn require_current_http_protocol(mut self) -> Self {
        self.protocol_profile = ProtocolProfile::CurrentHttp;
        self
    }

    fn router_for_profile(profile: ToolProfile) -> ToolRouter<Self> {
        let mut router = Self::tool_router();
        if profile == ToolProfile::Vision {
            router.disable_route("perform_action");
        }
        router
    }

    /// Closes the runtime session and deletes transient screenshot artifacts.
    pub async fn close(&self) {
        let _operation = self.operation_lock.lock().await;
        if let Some(session) = self.session.lock().await.take() {
            let _ = self
                .runtime
                .execute(Command::CloseSession(SessionInput {
                    session_id: session.session_id,
                }))
                .await;
        }
    }

    async fn execute(&self, command: Command) -> Result<CommandResult, CallToolResult> {
        if let Err(error) = self.authority.validate_provider().await {
            return Err(adapter_tool_error(
                "authority_revoked",
                &error.to_string(),
                false,
                Some("stop_adapter_and_reauthorize"),
            ));
        }
        match self.runtime.execute(command).await {
            Ok(result) => Ok(result),
            Err(error) => {
                // Runtime sessions are finite and may be reaped by TTL or
                // invalidated by a sidecar/application-generation restart.
                // Do not keep advertising the dead local session as active;
                // all its opaque refs are invalid as well.
                if error.code == nexus_cua_protocol::ErrorCode::SessionUnavailable {
                    self.invalidate_local_session().await;
                }
                Err(cua_tool_error(&error))
            }
        }
    }

    async fn invalidate_local_session(&self) {
        self.session.lock().await.take();
        self.image_mappings.lock().await.clear();
    }

    async fn execute_mutation(
        &self,
        request_id: RequestId,
        command: Command,
    ) -> Result<CommandResult, CallToolResult> {
        if let Err(error) = self.authority.validate_provider().await {
            return Err(adapter_tool_error_for_request(
                &request_id,
                "authority_revoked",
                &error.to_string(),
                false,
                Some("stop_adapter_and_reauthorize"),
            ));
        }
        if !valid_adapter_request_id(request_id.as_str())
            || !self.authority.accepts_request_id(request_id.as_str())
        {
            return Err(adapter_tool_error_for_request(
                &request_id,
                "invalid_request",
                "request_id is malformed or outside the provider-issued namespace",
                false,
                Some("read_status_and_fix_request_id"),
            ));
        }
        let response = self
            .mutation_dispatcher
            .dispatch(RequestEnvelope {
                protocol_version: PROTOCOL_VERSION.to_owned(),
                request_id: request_id.clone(),
                timeout_ms: MUTATION_MAX_WAIT_MS,
                authorization: self.mutation_authorization.clone(),
                command,
            })
            .await;
        match response.outcome {
            ResponseOutcome::Success { result } => Ok(result),
            ResponseOutcome::Error { error } => {
                Err(cua_tool_error_for_request(&request_id, &error))
            }
        }
    }

    async fn session_id(&self) -> Result<SessionId, CallToolResult> {
        self.session
            .lock()
            .await
            .as_ref()
            .map(|session| session.session_id.clone())
            .ok_or_else(|| {
                adapter_tool_error(
                    "session_unavailable",
                    "no configured application is running; launch an authorized application first",
                    true,
                    Some("launch_application"),
                )
            })
    }

    async fn dispatch_action(
        &self,
        request_id: RequestId,
        window_ref: WindowRef,
        observation_id: ObservationId,
        action: Action,
    ) -> Result<CallToolResult, McpError> {
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        match self
            .execute_mutation(
                request_id.clone(),
                Command::PerformAction(PerformActionInput {
                    session_id,
                    window_ref: window_ref.clone(),
                    observation_id,
                    action,
                }),
            )
            .await
        {
            Ok(CommandResult::ActionPerformed(output)) => {
                self.image_mappings
                    .lock()
                    .await
                    .invalidate_window(&window_ref);
                mutation_structured(&request_id, &output)
            }
            Ok(_) => Err(unexpected_result()),
            Err(error) => Ok(error),
        }
    }

    async fn rebuild_session(&self) -> Result<(), CallToolResult> {
        let (application_refs, _) = authorized_running_applications(&self.runtime, &self.authority)
            .await
            .map_err(bootstrap_tool_error)?;
        if application_refs.is_empty() {
            return Err(adapter_tool_error(
                "target_unavailable",
                "launched application did not produce a discoverable running generation",
                true,
                Some("discover_applications"),
            ));
        }
        let replacement = open_runtime_session(&self.runtime, &self.authority, application_refs)
            .await
            .map_err(bootstrap_tool_error)?;
        let prior = self.session.lock().await.replace(replacement);
        self.image_mappings.lock().await.clear();
        if let Some(prior) = prior {
            let _ = self
                .runtime
                .execute(Command::CloseSession(SessionInput {
                    session_id: prior.session_id,
                }))
                .await;
        }
        Ok(())
    }
}

async fn authorized_running_applications(
    runtime: &Runtime,
    authority: &AuthorityPolicy,
) -> Result<(Vec<DiscoveryRef>, Vec<String>), BootstrapError> {
    let CommandResult::ApplicationsDiscovered(discovered) =
        runtime.execute(Command::DiscoverApplications).await?
    else {
        return Err(BootstrapError::UnexpectedResult);
    };
    if !discovered.complete {
        return Err(BootstrapError::IncompleteDiscovery);
    }
    let configured = authority.application_ids.iter().collect::<HashSet<_>>();
    let mut matched = HashSet::new();
    let mut application_refs = Vec::new();
    for application in discovered.applications {
        if configured.contains(&application.application_id) {
            matched.insert(application.application_id);
            application_refs.push(application.discovery_ref);
        }
    }
    let missing = authority
        .application_ids
        .iter()
        .filter(|application_id| !matched.contains(*application_id))
        .cloned()
        .collect();
    Ok((application_refs, missing))
}

async fn verify_launchable_applications(
    runtime: &Runtime,
    required: &[String],
) -> Result<(), BootstrapError> {
    let CommandResult::Capabilities(capabilities) =
        runtime.execute(Command::GetCapabilities).await?
    else {
        return Err(BootstrapError::UnexpectedResult);
    };
    if !capabilities
        .application_lifecycle
        .contains(&ApplicationLifecycleOperation::Launch)
    {
        return Err(BootstrapError::ApplicationsUnavailable(required.to_vec()));
    }
    let CommandResult::LaunchableApplicationsDiscovered(discovered) = runtime
        .execute(Command::DiscoverLaunchableApplications)
        .await?
    else {
        return Err(BootstrapError::UnexpectedResult);
    };
    if !discovered.complete {
        return Err(BootstrapError::IncompleteLaunchDiscovery);
    }
    let available = discovered
        .applications
        .into_iter()
        .map(|application| application.application_id)
        .collect::<HashSet<_>>();
    let missing = required
        .iter()
        .filter(|application_id| !available.contains(*application_id))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(BootstrapError::ApplicationsUnavailable(missing))
    }
}

async fn open_runtime_session(
    runtime: &Runtime,
    authority: &AuthorityPolicy,
    application_refs: Vec<DiscoveryRef>,
) -> Result<ActiveSession, BootstrapError> {
    let manifest = CapabilityManifest {
        mode: authority.mode,
        application_refs,
        allowed_actions: authority.allowed_actions.clone(),
        allow_foreground_input: authority.allow_foreground_input,
        restore_foreground_after_input: authority.restore_foreground_after_input,
        allow_display_capture: authority.allow_display_capture,
        allow_clipboard_read: authority.allow_clipboard_read,
        allow_clipboard_write: authority.allow_clipboard_write,
        ttl_seconds: authority.ttl_seconds,
    };
    let CommandResult::SessionOpened(opened) = runtime
        .execute(Command::OpenSession(OpenSessionInput { manifest }))
        .await?
    else {
        return Err(BootstrapError::UnexpectedResult);
    };
    Ok(ActiveSession {
        session_id: opened.session_id,
        expires_at: opened.expires_at,
    })
}

fn bootstrap_tool_error(error: BootstrapError) -> CallToolResult {
    match error {
        BootstrapError::Runtime(error) => cua_tool_error(&error),
        other => adapter_tool_error(
            "session_unavailable",
            &other.to_string(),
            true,
            Some("refresh_application_state"),
        ),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListWindowsArgs {
    /// Optional opaque application reference returned by `list_apps`.
    app_ref: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ObserveDisplayArgs {
    /// Opaque target returned by `list_displays`.
    display_ref: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ObserveWindowArgs {
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Include a transient PNG image block. Defaults to true.
    include_screenshot: Option<bool>,
    /// Semantic detail: disabled, interactive, or full. Defaults to interactive.
    accessibility: Option<AccessibilityMode>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteClipboardArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Fresh opaque guard returned by `read_clipboard`.
    clipboard_ref: String,
    /// Bounded replacement text. The server does not log or retain it.
    #[schemars(length(max = 65536))]
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PerformActionArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the last `observe_window` call.
    observation_id: String,
    /// One closed nexus-cua action authorized by the host policy.
    action: Action,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TerminateApplicationArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the last `observe_window` call.
    observation_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LaunchApplicationArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Exact stable application identity returned by `list_launchable_apps`.
    application_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NormalizedClickArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the screenshot observation being clicked.
    observation_id: String,
    /// Top-left-origin horizontal coordinate from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    x: u16,
    /// Top-left-origin vertical coordinate from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    y: u16,
    /// Pointer button: left, middle, or right. Defaults to left.
    #[schemars(with = "Option<String>")]
    button: Option<PointerButton>,
    /// Click count from 1 through 3. Defaults to 1.
    #[schemars(range(min = 1, max = 3))]
    count: Option<u8>,
    /// Delivery route: foreground (default) or best-effort background.
    #[schemars(with = "Option<String>")]
    delivery: Option<DeliveryPreference>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ZoomArgs {
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Current screenshot observation being cropped.
    observation_id: String,
    /// Inclusive left edge in the full image's top-left 0..1000 space.
    #[schemars(range(min = 0, max = 1000))]
    left: u16,
    /// Inclusive top edge in the full image's top-left 0..1000 space.
    #[schemars(range(min = 0, max = 1000))]
    top: u16,
    /// Exclusive right edge in the full image's top-left 0..1000 space.
    #[schemars(range(min = 0, max = 1000))]
    right: u16,
    /// Exclusive bottom edge in the full image's top-left 0..1000 space.
    #[schemars(range(min = 0, max = 1000))]
    bottom: u16,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewClickArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque crop view returned by `zoom`.
    view_ref: String,
    /// Horizontal coordinate within the crop, from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    x: u16,
    /// Vertical coordinate within the crop, from 0 through 1000.
    #[schemars(range(min = 0, max = 1000))]
    y: u16,
    /// Pointer button: left, middle, or right. Defaults to left.
    #[schemars(with = "Option<String>")]
    button: Option<PointerButton>,
    /// Click count from 1 through 3. Defaults to 1.
    #[schemars(range(min = 1, max = 3))]
    count: Option<u8>,
    /// Delivery route: foreground (default) or best-effort background.
    #[schemars(with = "Option<String>")]
    delivery: Option<DeliveryPreference>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct VerifyStateArgs {
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Deterministic predicate evaluated against fresh native state.
    predicate: StatePredicate,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GuardedTextArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the last `observe_window` call.
    observation_id: String,
    /// Sensitive text delivered to the native driver and never written to logs.
    #[schemars(length(max = 65536))]
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GuardedKeysArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the last `observe_window` call.
    observation_id: String,
    /// One normalized chord of at most eight lower-case key names.
    #[schemars(length(min = 1, max = 8))]
    keys: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GuardedScrollArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the last `observe_window` call.
    observation_id: String,
    /// Horizontal logical scroll delta.
    #[schemars(range(min = -100_000.0, max = 100_000.0))]
    delta_x: f64,
    /// Vertical logical scroll delta.
    #[schemars(range(min = -100_000.0, max = 100_000.0))]
    delta_y: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NormalizedDragArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the screenshot observation being dragged.
    observation_id: String,
    /// Drag-start horizontal coordinate in the full image's 0..1000 space.
    #[schemars(range(min = 0, max = 1000))]
    from_x: u16,
    /// Drag-start vertical coordinate in the full image's 0..1000 space.
    #[schemars(range(min = 0, max = 1000))]
    from_y: u16,
    /// Drag-end horizontal coordinate in the full image's 0..1000 space.
    #[schemars(range(min = 0, max = 1000))]
    to_x: u16,
    /// Drag-end vertical coordinate in the full image's 0..1000 space.
    #[schemars(range(min = 0, max = 1000))]
    to_y: u16,
    /// Bounded gesture duration in milliseconds. Defaults to 500.
    #[schemars(range(min = 1, max = 10000))]
    duration_ms: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GuardedElementArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the last `observe_window` call.
    observation_id: String,
    /// Opaque element returned by that same observation.
    element_ref: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GuardedElementValueArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the last `observe_window` call.
    observation_id: String,
    /// Opaque element returned by that same observation.
    element_ref: String,
    /// Sensitive replacement value delivered without logging.
    #[schemars(length(max = 65536))]
    value: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GuardedElementRangeValueArgs {
    /// Stable host-generated mutation identity; preserve it for reconciliation.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Opaque target returned by `list_windows`.
    window_ref: String,
    /// Fresh guard returned by the last `observe_window` call.
    observation_id: String,
    /// Opaque range element returned by that same observation.
    element_ref: String,
    /// Absolute finite value within the range advertised on that element.
    #[schemars(range(min = -1_000_000_000_000.0, max = 1_000_000_000_000.0))]
    value: f64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReconcileMutationArgs {
    /// Original stable mutation identity. This query never redispatches it.
    #[schemars(length(min = 1, max = 128))]
    request_id: String,
    /// Maximum time to observe an already-admitted mutation. Defaults to zero.
    #[schemars(range(min = 0, max = 120_000))]
    wait_ms: Option<u32>,
}

#[tool_router]
impl NexusCuaMcp {
    #[tool(
        name = "get_status",
        description = "Read native capabilities, permissions, session expiry, and the non-secret host authority summary.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn get_status(&self) -> Result<CallToolResult, McpError> {
        let capabilities = match self.execute(Command::GetCapabilities).await {
            Ok(CommandResult::Capabilities(value)) => value,
            Ok(_) => return Err(unexpected_result()),
            Err(error) => return Ok(error),
        };
        let permissions = match self.execute(Command::GetPermissionStatus).await {
            Ok(CommandResult::PermissionStatus(value)) => value,
            Ok(_) => return Err(unexpected_result()),
            Err(error) => return Ok(error),
        };
        let active_session = self.session.lock().await.clone();
        let policy = self.runtime.policy_summary();
        Ok(CallToolResult::structured(json!({
            "capabilities": capabilities,
            "permissions": permissions,
            "runtime_policy": {
                "managed_sha256": policy.managed_sha256,
                "user_sha256": policy.user_sha256,
            },
            "session": {
                "mode": self.authority.mode,
                "application_ids": self.authority.application_ids,
                "allowed_actions": self.authority.allowed_actions,
                "allow_foreground_input": self.authority.allow_foreground_input,
                "restore_foreground_after_input": self.authority.restore_foreground_after_input,
                "allow_display_capture": self.authority.allow_display_capture,
                "allow_clipboard_read": self.authority.allow_clipboard_read,
                "allow_clipboard_write": self.authority.allow_clipboard_write,
                "allow_application_launch": self.authority.allow_application_launch,
                "allow_application_launch_activation": self.authority.allow_application_launch_activation,
                "tool_profile": self.tool_profile,
                "active": active_session.is_some(),
                "expires_at": active_session.map(|session| session.expires_at),
            },
            "authority_provider": self.authority.provider_metadata(),
            "authority_source": if self.authority.provider_metadata().is_some() {
                "authenticated_provider"
            } else {
                "unsigned_development_fixture"
            },
        })))
    }

    #[tool(
        name = "read_clipboard",
        description = "Read bounded UTF-8 plain text from the system clipboard under separate host authority. Returns a fresh opaque clipboard_ref required by any replacement; clipboard text is transient and must not be logged or retained.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn read_clipboard(&self) -> Result<CallToolResult, McpError> {
        if !self.authority.allow_clipboard_read {
            return Ok(adapter_tool_error(
                "capability_denied",
                "host authority does not allow clipboard reads",
                false,
                Some("request_clipboard_read_authority"),
            ));
        }
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        match self
            .execute(Command::ReadClipboard(ReadClipboardInput { session_id }))
            .await
        {
            Ok(CommandResult::ClipboardRead(snapshot)) => structured(&snapshot),
            Ok(_) => Err(unexpected_result()),
            Err(error) => Ok(error),
        }
    }

    #[tool(
        name = "write_clipboard",
        description = "Replace system clipboard text only if the fresh opaque clipboard_ref from read_clipboard still matches. This consumes the guard; after any failure, read again instead of retrying blindly.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn write_clipboard(
        &self,
        Parameters(args): Parameters<WriteClipboardArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.authority.allow_clipboard_write {
            return Ok(adapter_tool_error(
                "capability_denied",
                "host authority does not allow clipboard replacement",
                false,
                Some("request_clipboard_write_authority"),
            ));
        }
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        let request_id = RequestId::new(args.request_id);
        match self
            .execute_mutation(
                request_id.clone(),
                Command::WriteClipboard(WriteClipboardInput {
                    session_id,
                    clipboard_ref: ClipboardRef::new(args.clipboard_ref),
                    text: SensitiveText::new(args.text),
                }),
            )
            .await
        {
            Ok(CommandResult::ClipboardWritten(output)) => {
                mutation_structured(&request_id, &output)
            }
            Ok(_) => Err(unexpected_result()),
            Err(error) => Ok(error),
        }
    }

    #[tool(
        name = "list_apps",
        description = "List host-authorized running applications and return session-scoped opaque app refs.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_apps(&self) -> Result<CallToolResult, McpError> {
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        match self
            .execute(Command::ListApps(SessionInput { session_id }))
            .await
        {
            Ok(CommandResult::Apps(apps)) => structured(&apps),
            Ok(_) => Err(unexpected_result()),
            Err(error) => Ok(error),
        }
    }

    #[tool(
        name = "list_launchable_apps",
        description = "List installed applications that are both launchable by the native driver and allowed by the host authority file. Returned paths are descriptive provenance, never launch inputs.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_launchable_apps(&self) -> Result<CallToolResult, McpError> {
        if !self.authority.allow_application_launch {
            return Ok(adapter_tool_error(
                "capability_denied",
                "host authority does not allow application launch",
                false,
                Some("request_launch_authority"),
            ));
        }
        let discovered = match self.execute(Command::DiscoverLaunchableApplications).await {
            Ok(CommandResult::LaunchableApplicationsDiscovered(discovered)) => discovered,
            Ok(_) => return Err(unexpected_result()),
            Err(error) => return Ok(error),
        };
        if !discovered.complete {
            return Ok(adapter_tool_error(
                "busy",
                "installed application discovery was incomplete",
                true,
                Some("retry_installed_application_discovery"),
            ));
        }
        let configured = self
            .authority
            .application_ids
            .iter()
            .collect::<HashSet<_>>();
        let applications = discovered
            .applications
            .into_iter()
            .filter(|application| configured.contains(&application.application_id))
            .collect::<Vec<_>>();
        structured(&applications)
    }

    #[tool(
        name = "launch_application",
        description = "Cold-launch one exact host-authorized installed application identity. The server performs fresh native discovery, never accepts a path or arguments, and rebuilds its bounded session around the new process generation.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn launch_application(
        &self,
        Parameters(args): Parameters<LaunchApplicationArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.authority.allow_application_launch
            || !self
                .authority
                .application_ids
                .contains(&args.application_id)
        {
            return Ok(adapter_tool_error(
                "capability_denied",
                "application identity is outside host launch authority",
                false,
                Some("request_launch_authority"),
            ));
        }
        let _operation = self.operation_lock.lock().await;
        let discovered = match self.execute(Command::DiscoverLaunchableApplications).await {
            Ok(CommandResult::LaunchableApplicationsDiscovered(discovered)) => discovered,
            Ok(_) => return Err(unexpected_result()),
            Err(error) => return Ok(error),
        };
        if !discovered.complete {
            return Ok(adapter_tool_error(
                "busy",
                "installed application discovery was incomplete",
                true,
                Some("retry_installed_application_discovery"),
            ));
        }
        let mut matches = discovered
            .applications
            .into_iter()
            .filter(|application| application.application_id == args.application_id);
        let Some(application) = matches.next() else {
            return Ok(adapter_tool_error(
                "target_unavailable",
                "authorized installed application identity is unavailable",
                true,
                Some("list_launchable_apps"),
            ));
        };
        if matches.next().is_some() {
            return Ok(adapter_tool_error(
                "invalid_request",
                "installed application identity resolves to multiple native registrations",
                false,
                Some("narrow_host_application_identity"),
            ));
        }
        let request_id = RequestId::new(args.request_id);
        let output = match self
            .execute_mutation(
                request_id.clone(),
                Command::LaunchApplication(LaunchApplicationInput {
                    launch_ref: application.launch_ref,
                    allow_foreground_activation: self.authority.allow_application_launch_activation,
                }),
            )
            .await
        {
            Ok(CommandResult::ApplicationLaunched(output)) => output,
            Ok(_) => return Err(unexpected_result()),
            Err(error) => return Ok(error),
        };
        if self.rebuild_session().await.is_err() {
            return Ok(adapter_indeterminate_tool_error(
                "session_refresh_failed",
                "application launched but MCP could not bind a fresh session to its process generation",
                Some("list_apps"),
            ));
        }
        mutation_structured(&request_id, &output)
    }

    #[tool(
        name = "list_windows",
        description = "List exact top-level windows for the authorized session, optionally filtered by an opaque app ref.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_windows(
        &self,
        Parameters(args): Parameters<ListWindowsArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        let app_ref = args.app_ref.map(AppRef::new);
        match self
            .execute(Command::ListWindows(ListWindowsInput {
                session_id,
                app_ref,
            }))
            .await
        {
            Ok(CommandResult::Windows(windows)) => structured(&windows),
            Ok(_) => Err(unexpected_result()),
            Err(error) => Ok(error),
        }
    }

    #[tool(
        name = "list_displays",
        description = "List exact physical displays under separate host authority. Display refs are read-only overview targets and can never authorize actions.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn list_displays(&self) -> Result<CallToolResult, McpError> {
        if !self.authority.allow_display_capture {
            return Ok(adapter_tool_error(
                "capability_denied",
                "host authority does not allow physical-display capture",
                false,
                Some("request_display_capture_authority"),
            ));
        }
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        match self
            .execute(Command::ListDisplays(SessionInput { session_id }))
            .await
        {
            Ok(CommandResult::Displays(displays)) => structured(&displays),
            Ok(_) => Err(unexpected_result()),
            Err(error) => Ok(error),
        }
    }

    #[tool(
        name = "observe_display",
        description = "Capture one exact physical display as a verified transient PNG. This is read-only desktop overview; its observation_id cannot be used for any action.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn observe_display(
        &self,
        Parameters(args): Parameters<ObserveDisplayArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.authority.allow_display_capture {
            return Ok(adapter_tool_error(
                "capability_denied",
                "host authority does not allow physical-display capture",
                false,
                Some("request_display_capture_authority"),
            ));
        }
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        let observation = match self
            .execute(Command::ObserveDisplay(ObserveDisplayInput {
                session_id,
                display_ref: DisplayRef::new(args.display_ref),
            }))
            .await
        {
            Ok(CommandResult::DisplayObserved(observation)) => observation,
            Ok(_) => return Err(unexpected_result()),
            Err(error) => return Ok(error),
        };
        let structured_content = public_observation_value(observation.as_ref())?;
        let size = observation.screenshot.mapping.pixel_size;
        let bytes = match self
            .runtime
            .take_artifact(&observation.screenshot, self.max_image_bytes)
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => return Ok(cua_tool_error(&error)),
        };
        let content = vec![
            ContentBlock::text(format!(
                "Observed read-only display {} at {}; the PNG is exactly {}x{} top-left-origin pixels. Select and observe an exact window before any action.",
                observation.display_ref, observation.captured_at, size.width, size.height
            )),
            ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(bytes),
                observation.screenshot.mime_type.clone(),
            ),
        ];
        let mut result = CallToolResult::success(content);
        result.structured_content = Some(structured_content);
        Ok(result)
    }

    #[tool(
        name = "observe_window",
        description = "Observe one exact window. Returns structured accessibility state and, by default, a verified PNG image block. Every call supersedes older observations for that window.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn observe_window(
        &self,
        Parameters(args): Parameters<ObserveWindowArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        let command = Command::ObserveWindow(ObserveWindowInput {
            session_id,
            window_ref: WindowRef::new(args.window_ref),
            include_screenshot: args.include_screenshot.unwrap_or(true),
            accessibility: args.accessibility.unwrap_or(AccessibilityMode::Interactive),
        });
        let observation = match self.execute(command).await {
            Ok(CommandResult::WindowObserved(observation)) => observation,
            Ok(_) => return Err(unexpected_result()),
            Err(error) => return Ok(error),
        };
        let structured_content = public_observation_value(observation.as_ref())?;
        let mut summary = format!(
            "Observed window {} at {}; {} accessibility element(s).",
            observation.window_ref,
            observation.captured_at,
            observation.elements.len()
        );
        let image = if let Some(screenshot) = &observation.screenshot {
            let size = screenshot.mapping.pixel_size;
            let bytes = match self
                .runtime
                .take_artifact(screenshot, self.max_image_bytes)
                .await
            {
                Ok(bytes) => bytes,
                Err(error) => return Ok(cua_tool_error(&error)),
            };
            let image = ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(&bytes),
                screenshot.mime_type.clone(),
            );
            self.image_mappings.lock().await.insert(
                observation.observation_id.clone(),
                observation.window_ref.clone(),
                screenshot.mapping,
                bytes,
            );
            write!(
                summary,
                " The PNG is exactly {}x{} top-left-origin pixels. Prefer click_normalized with x/y in 0..1000 so client-side image resizing cannot change the click target.",
                size.width, size.height
            )
            .expect("formatting into a String cannot fail");
            Some(image)
        } else {
            None
        };
        let mut content = vec![ContentBlock::text(summary)];
        if let Some(image) = image {
            content.push(image);
        }
        let mut result = CallToolResult::success(content);
        result.structured_content = Some(structured_content);
        Ok(result)
    }

    #[tool(
        name = "zoom",
        description = "Crop and enlarge a region from the current verified screenshot. The region uses the full image's top-left 0..1000 space and returns an opaque view_ref for coordinate-safe clicks.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn zoom(
        &self,
        Parameters(args): Parameters<ZoomArgs>,
    ) -> Result<CallToolResult, McpError> {
        let window_ref = WindowRef::new(args.window_ref);
        let observation_id = ObservationId::new(args.observation_id);
        let image = self
            .image_mappings
            .lock()
            .await
            .get(&observation_id, &window_ref);
        let Some(image) = image else {
            return Ok(adapter_tool_error(
                "stale_observation",
                "zoom requires this MCP process's current screenshot observation",
                true,
                Some("observe_window"),
            ));
        };
        let Some(source_rect) = normalized_region(
            args.left,
            args.top,
            args.right,
            args.bottom,
            image.mapping.pixel_size,
        ) else {
            return Ok(adapter_tool_error(
                "invalid_request",
                "zoom edges must be within 0..1000 with left < right and top < bottom",
                false,
                Some("fix_request"),
            ));
        };
        let source_size = image.mapping.pixel_size;
        let bytes = Arc::clone(&image.bytes);
        let max_image_bytes = self.max_image_bytes;
        let cropped = tokio::task::spawn_blocking(move || {
            crop_png(bytes.as_ref(), source_size, source_rect, max_image_bytes)
        })
        .await
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let cropped = match cropped {
            Ok(cropped) => cropped,
            Err(error) => return Ok(image_tool_error(&error)),
        };
        let view_ref = format!("view_{}", Uuid::new_v4().simple());
        self.image_mappings.lock().await.insert_view(
            view_ref.clone(),
            RegionView {
                observation_id: observation_id.clone(),
                window_ref: window_ref.clone(),
                source_rect,
            },
        );
        let crop_size = PixelSize {
            width: source_rect.width,
            height: source_rect.height,
        };
        let content = vec![
            ContentBlock::text(format!(
                "Zoom view {view_ref} is exactly {}x{} pixels. Use click_view_normalized with top-left x/y in 0..1000 to act inside this crop.",
                crop_size.width, crop_size.height
            )),
            ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(cropped),
                "image/png",
            ),
        ];
        let mut result = CallToolResult::success(content);
        result.structured_content = Some(json!({
            "view_ref": view_ref,
            "observation_id": observation_id,
            "window_ref": window_ref,
            "source_pixel_bounds": {
                "x": source_rect.x,
                "y": source_rect.y,
                "width": source_rect.width,
                "height": source_rect.height,
            },
            "pixel_size": crop_size,
        }));
        Ok(result)
    }

    #[tool(
        name = "perform_action",
        description = "Perform exactly one host-authorized desktop mutation guarded by a fresh observation. Observe again after success. Do not repeat an indeterminate result.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn perform_action(
        &self,
        Parameters(args): Parameters<PerformActionArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_action(
            RequestId::new(args.request_id),
            WindowRef::new(args.window_ref),
            ObservationId::new(args.observation_id),
            args.action,
        )
        .await
    }

    #[tool(
        name = "terminate_application",
        description = "Cooperatively exit the exact application generation that owns an observed window. Requires explicit terminate_application authority and invalidates that application's observations in every session.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn terminate_application(
        &self,
        Parameters(args): Parameters<TerminateApplicationArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_action(
            RequestId::new(args.request_id),
            WindowRef::new(args.window_ref),
            ObservationId::new(args.observation_id),
            Action::TerminateApplication,
        )
        .await
    }

    #[tool(
        name = "click_normalized",
        description = "Click a screenshot observation using a top-left 0..1000 coordinate space. Prefer this for visual models because MCP clients may resize images before inference.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn click_normalized(
        &self,
        Parameters(args): Parameters<NormalizedClickArgs>,
    ) -> Result<CallToolResult, McpError> {
        let window_ref = WindowRef::new(args.window_ref);
        let observation_id = ObservationId::new(args.observation_id);
        let count = args.count.unwrap_or(1);
        if args.x > NORMALIZED_COORDINATE_MAX
            || args.y > NORMALIZED_COORDINATE_MAX
            || !(1..=3).contains(&count)
        {
            return Ok(adapter_tool_error(
                "invalid_request",
                "normalized x/y must be between 0 and 1000 and count must be between 1 and 3",
                false,
                Some("fix_request"),
            ));
        }
        let image = self
            .image_mappings
            .lock()
            .await
            .get(&observation_id, &window_ref);
        let Some(image) = image else {
            return Ok(adapter_tool_error(
                "stale_observation",
                "normalized click requires this MCP process's current screenshot observation",
                true,
                Some("observe_window"),
            ));
        };
        let point = normalized_point(args.x, args.y, image.mapping.pixel_size);
        self.dispatch_action(
            RequestId::new(args.request_id),
            window_ref,
            observation_id,
            Action::ClickPoint {
                point,
                button: args.button.unwrap_or(PointerButton::Left),
                count,
                delivery: args.delivery.unwrap_or_default(),
            },
        )
        .await
    }

    #[tool(
        name = "click_view_normalized",
        description = "Click inside an opaque zoom view using that crop's top-left 0..1000 coordinate space. The server maps it back through the guarded full screenshot.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn click_view_normalized(
        &self,
        Parameters(args): Parameters<ViewClickArgs>,
    ) -> Result<CallToolResult, McpError> {
        let count = args.count.unwrap_or(1);
        if args.x > NORMALIZED_COORDINATE_MAX
            || args.y > NORMALIZED_COORDINATE_MAX
            || !(1..=3).contains(&count)
        {
            return Ok(adapter_tool_error(
                "invalid_request",
                "normalized x/y must be between 0 and 1000 and count must be between 1 and 3",
                false,
                Some("fix_request"),
            ));
        }
        let view = self.image_mappings.lock().await.get_view(&args.view_ref);
        let Some(view) = view else {
            return Ok(adapter_tool_error(
                "stale_observation",
                "zoom view is expired or was invalidated by a newer observation",
                true,
                Some("observe_window"),
            ));
        };
        let point = normalized_view_point(args.x, args.y, view.source_rect);
        self.dispatch_action(
            RequestId::new(args.request_id),
            view.window_ref,
            view.observation_id,
            Action::ClickPoint {
                point,
                button: args.button.unwrap_or(PointerButton::Left),
                count,
                delivery: args.delivery.unwrap_or_default(),
            },
        )
        .await
    }

    #[tool(
        name = "type_text",
        description = "Type sensitive text through the foreground input route, guarded by a fresh observation. This flat schema is preferred by visual-model harnesses.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn type_text(
        &self,
        Parameters(args): Parameters<GuardedTextArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_action(
            RequestId::new(args.request_id),
            WindowRef::new(args.window_ref),
            ObservationId::new(args.observation_id),
            Action::TypeText {
                text: SensitiveText::new(args.text),
            },
        )
        .await
    }

    #[tool(
        name = "press_keys",
        description = "Press one normalized keyboard chord guarded by a fresh observation. Examples: [\"meta\",\"a\"] or [\"enter\"].",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn press_keys(
        &self,
        Parameters(args): Parameters<GuardedKeysArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_action(
            RequestId::new(args.request_id),
            WindowRef::new(args.window_ref),
            ObservationId::new(args.observation_id),
            Action::PressKeys { keys: args.keys },
        )
        .await
    }

    #[tool(
        name = "scroll",
        description = "Scroll the guarded window surface using bounded logical deltas. Positive vertical values scroll down.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn scroll(
        &self,
        Parameters(args): Parameters<GuardedScrollArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_action(
            RequestId::new(args.request_id),
            WindowRef::new(args.window_ref),
            ObservationId::new(args.observation_id),
            Action::Scroll {
                element_ref: None,
                delta_x: args.delta_x,
                delta_y: args.delta_y,
            },
        )
        .await
    }

    #[tool(
        name = "drag_normalized",
        description = "Drag between two points in a screenshot's top-left 0..1000 coordinate space. The server maps both points through the guarded full image.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn drag_normalized(
        &self,
        Parameters(args): Parameters<NormalizedDragArgs>,
    ) -> Result<CallToolResult, McpError> {
        let window_ref = WindowRef::new(args.window_ref);
        let observation_id = ObservationId::new(args.observation_id);
        if args.from_x > NORMALIZED_COORDINATE_MAX
            || args.from_y > NORMALIZED_COORDINATE_MAX
            || args.to_x > NORMALIZED_COORDINATE_MAX
            || args.to_y > NORMALIZED_COORDINATE_MAX
            || args
                .duration_ms
                .is_some_and(|value| !(1..=10_000).contains(&value))
        {
            return Ok(adapter_tool_error(
                "invalid_request",
                "normalized coordinates must be between 0 and 1000 and duration_ms between 1 and 10000",
                false,
                Some("fix_request"),
            ));
        }
        let image = self
            .image_mappings
            .lock()
            .await
            .get(&observation_id, &window_ref);
        let Some(image) = image else {
            return Ok(adapter_tool_error(
                "stale_observation",
                "normalized drag requires this MCP process's current screenshot observation",
                true,
                Some("observe_window"),
            ));
        };
        let size = image.mapping.pixel_size;
        self.dispatch_action(
            RequestId::new(args.request_id),
            window_ref,
            observation_id,
            Action::Drag {
                from: normalized_point(args.from_x, args.from_y, size),
                to: normalized_point(args.to_x, args.to_y, size),
                duration_ms: args.duration_ms.unwrap_or(500),
            },
        )
        .await
    }

    #[tool(
        name = "invoke_element",
        description = "Invoke one accessibility element returned by the current observation using a flat visual-model-friendly schema.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn invoke_element(
        &self,
        Parameters(args): Parameters<GuardedElementArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_action(
            RequestId::new(args.request_id),
            WindowRef::new(args.window_ref),
            ObservationId::new(args.observation_id),
            Action::InvokeElement {
                element_ref: ElementRef::new(args.element_ref),
            },
        )
        .await
    }

    #[tool(
        name = "set_element_value",
        description = "Replace one accessibility element value returned by the current observation without logging the sensitive value.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn set_element_value(
        &self,
        Parameters(args): Parameters<GuardedElementValueArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_action(
            RequestId::new(args.request_id),
            WindowRef::new(args.window_ref),
            ObservationId::new(args.observation_id),
            Action::SetValue {
                element_ref: ElementRef::new(args.element_ref),
                value: SensitiveText::new(args.value),
            },
        )
        .await
    }

    #[tool(
        name = "set_range_value",
        description = "Set one native numeric range control to an absolute value within the bounds advertised by the current observation.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_range_value(
        &self,
        Parameters(args): Parameters<GuardedElementRangeValueArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_action(
            RequestId::new(args.request_id),
            WindowRef::new(args.window_ref),
            ObservationId::new(args.observation_id),
            Action::SetRangeValue {
                element_ref: ElementRef::new(args.element_ref),
                value: args.value,
            },
        )
        .await
    }

    #[tool(
        name = "reconcile_mutation",
        description = "Query one stable mutation request_id in this runtime epoch without dispatching it again. A bounded wait observes only work that was already admitted.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn reconcile_mutation(
        &self,
        Parameters(args): Parameters<ReconcileMutationArgs>,
    ) -> Result<CallToolResult, McpError> {
        let wait_ms = args.wait_ms.unwrap_or(0);
        if !valid_adapter_request_id(&args.request_id)
            || !self.authority.accepts_request_id(&args.request_id)
            || wait_ms > MUTATION_MAX_WAIT_MS
        {
            return Ok(adapter_tool_error(
                "invalid_request",
                "request_id must be normalized ASCII within 128 bytes, use the provider-issued namespace, and wait_ms must not exceed 120000",
                false,
                Some("fix_request"),
            ));
        }
        let request_id = RequestId::new(args.request_id);
        let result = match self
            .mutation_dispatcher
            .reconcile_mutation(&request_id, Duration::from_millis(u64::from(wait_ms)))
            .await
        {
            Ok(result) => result,
            Err(error) => return Ok(cua_tool_error_for_request(&request_id, &error)),
        };
        structured(&result)
    }

    #[tool(
        name = "verify_state",
        description = "Evaluate a deterministic predicate against fresh native state without mutating the desktop.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn verify_state(
        &self,
        Parameters(args): Parameters<VerifyStateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let _operation = self.operation_lock.lock().await;
        let session_id = match self.session_id().await {
            Ok(session_id) => session_id,
            Err(error) => return Ok(error),
        };
        match self
            .execute(Command::VerifyState(VerifyStateInput {
                session_id,
                window_ref: WindowRef::new(args.window_ref),
                predicate: args.predicate,
            }))
            .await
        {
            Ok(CommandResult::StateVerified(output)) => structured(&output),
            Ok(_) => Err(unexpected_result()),
            Err(error) => Ok(error),
        }
    }
}

#[allow(unknown_lints, clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for NexusCuaMcp {
    fn get_info(&self) -> ServerInfo {
        let protocol_version = match self.protocol_profile {
            ProtocolProfile::CompatibleStdio => ProtocolVersion::LATEST,
            ProtocolProfile::CurrentHttp => ProtocolVersion::V_2026_07_28,
        };
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                MCP_SERVER_NAME,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_protocol_version(protocol_version)
            .with_instructions(SERVER_INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        match self.protocol_profile {
            ProtocolProfile::CompatibleStdio => Cow::Borrowed(ProtocolVersion::KNOWN_VERSIONS),
            ProtocolProfile::CurrentHttp => Cow::Borrowed(SUPPORTED_PROTOCOL_VERSIONS),
        }
    }
}

fn structured(value: &impl Serialize) -> Result<CallToolResult, McpError> {
    serde_json::to_value(value)
        .map(CallToolResult::structured)
        .map_err(|error| McpError::internal_error(error.to_string(), None))
}

fn mutation_structured(
    request_id: &RequestId,
    value: &impl Serialize,
) -> Result<CallToolResult, McpError> {
    let mut value = serde_json::to_value(value)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;
    let Some(object) = value.as_object_mut() else {
        return Err(McpError::internal_error(
            "mutation result must serialize as an object",
            None,
        ));
    };
    object.insert("request_id".to_owned(), json!(request_id));
    Ok(CallToolResult::structured(value))
}

fn unexpected_result() -> McpError {
    McpError::internal_error("nexus-cua returned an unexpected result variant", None)
}

fn valid_adapter_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn normalized_point(x: u16, y: u16, size: PixelSize) -> ScreenshotPoint {
    ScreenshotPoint {
        x: normalized_axis(x, size.width),
        y: normalized_axis(y, size.height),
    }
}

fn normalized_view_point(x: u16, y: u16, rect: PixelRect) -> ScreenshotPoint {
    ScreenshotPoint {
        x: rect.x.saturating_add(normalized_axis(x, rect.width)),
        y: rect.y.saturating_add(normalized_axis(y, rect.height)),
    }
}

fn normalized_axis(value: u16, length: u32) -> u32 {
    let maximum = u64::from(length.saturating_sub(1));
    u32::try_from((u64::from(value) * maximum + 500) / 1_000).unwrap_or(u32::MAX)
}

fn normalized_region(
    left: u16,
    top: u16,
    right: u16,
    bottom: u16,
    size: PixelSize,
) -> Option<PixelRect> {
    if right > NORMALIZED_COORDINATE_MAX
        || bottom > NORMALIZED_COORDINATE_MAX
        || left >= right
        || top >= bottom
        || size.width == 0
        || size.height == 0
    {
        return None;
    }
    let start =
        |value: u16, length: u32| u32::try_from(u64::from(value) * u64::from(length) / 1_000).ok();
    let end = |value: u16, length: u32| {
        u32::try_from((u64::from(value) * u64::from(length)).div_ceil(1_000))
            .ok()
            .map(|edge| edge.min(length))
    };
    let x = start(left, size.width)?;
    let y = start(top, size.height)?;
    let right = end(right, size.width)?;
    let bottom = end(bottom, size.height)?;
    Some(PixelRect {
        x,
        y,
        width: right.checked_sub(x)?,
        height: bottom.checked_sub(y)?,
    })
    .filter(|rect| rect.width > 0 && rect.height > 0)
}

fn adapter_tool_error(
    code: &str,
    message: &str,
    retryable: bool,
    recovery_action: Option<&str>,
) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(message.to_owned())]);
    result.structured_content = Some(json!({
        "code": code,
        "message": message,
        "retryable": retryable,
        "recovery_action": recovery_action,
        "mutation_status": "not_dispatched"
    }));
    result
}

fn adapter_tool_error_for_request(
    request_id: &RequestId,
    code: &str,
    message: &str,
    retryable: bool,
    recovery_action: Option<&str>,
) -> CallToolResult {
    let mut result = adapter_tool_error(code, message, retryable, recovery_action);
    if let Some(Value::Object(object)) = result.structured_content.as_mut() {
        object.insert("request_id".to_owned(), json!(request_id));
    }
    result
}

fn adapter_indeterminate_tool_error(
    code: &str,
    message: &str,
    recovery_action: Option<&str>,
) -> CallToolResult {
    let mut result = CallToolResult::error(vec![ContentBlock::text(message.to_owned())]);
    result.structured_content = Some(json!({
        "code": code,
        "message": message,
        "retryable": true,
        "recovery_action": recovery_action,
        "mutation_status": "indeterminate"
    }));
    result
}

fn cua_tool_error(error: &CuaError) -> CallToolResult {
    let value = serde_json::to_value(error).unwrap_or_else(|_| {
        json!({
            "code": "internal",
            "message": "failed to serialize nexus-cua error",
            "retryable": false,
            "recovery_action": null,
            "mutation_status": "not_applicable"
        })
    });
    let mut result = CallToolResult::error(vec![ContentBlock::text(format!(
        "nexus-cua refused the tool call: {}. Recovery: {}.",
        error.message,
        error.recovery_action.as_deref().unwrap_or("inspect status")
    ))]);
    result.structured_content = Some(value);
    result
}

fn cua_tool_error_for_request(request_id: &RequestId, error: &CuaError) -> CallToolResult {
    let mut result = cua_tool_error(error);
    if let Some(Value::Object(object)) = result.structured_content.as_mut() {
        object.insert("request_id".to_owned(), json!(request_id));
    }
    result
}

fn public_observation_value(observation: &impl Serialize) -> Result<Value, McpError> {
    let mut value = serde_json::to_value(observation)
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;
    remove_private_artifact_path(&mut value);
    Ok(value)
}

fn remove_private_artifact_path(value: &mut Value) {
    if let Some(screenshot) = value.get_mut("screenshot").and_then(Value::as_object_mut) {
        screenshot.remove("path");
    }
}

fn crop_png(
    input_png: &[u8],
    expected_size: PixelSize,
    rect: PixelRect,
    max_encoded_bytes: u64,
) -> Result<Vec<u8>, ImageError> {
    let expected_decoded = u64::from(expected_size.width)
        .checked_mul(u64::from(expected_size.height))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_DECODED_IMAGE_BYTES)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or(ImageError::DecodedImageTooLarge)?;
    let decoder = png::Decoder::new(Cursor::new(input_png));
    let mut reader = decoder.read_info()?;
    if reader.output_buffer_size() != Some(expected_decoded) {
        return Err(ImageError::UnexpectedFormat);
    }
    let mut pixels = vec![0; expected_decoded];
    let info = reader.next_frame(&mut pixels)?;
    if info.width != expected_size.width
        || info.height != expected_size.height
        || info.color_type != png::ColorType::Rgba
        || info.bit_depth != png::BitDepth::Eight
        || info.buffer_size() != expected_decoded
    {
        return Err(ImageError::UnexpectedFormat);
    }
    let source_stride = usize::try_from(expected_size.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or(ImageError::DecodedImageTooLarge)?;
    let crop_stride = usize::try_from(rect.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or(ImageError::DecodedImageTooLarge)?;
    let crop_bytes = usize::try_from(rect.height)
        .ok()
        .and_then(|height| height.checked_mul(crop_stride))
        .ok_or(ImageError::DecodedImageTooLarge)?;
    let mut cropped = Vec::with_capacity(crop_bytes);
    for row in rect.y..rect.y.saturating_add(rect.height) {
        let start = usize::try_from(row)
            .ok()
            .and_then(|row| row.checked_mul(source_stride))
            .and_then(|offset| {
                usize::try_from(rect.x)
                    .ok()
                    .and_then(|x| x.checked_mul(4))
                    .and_then(|x| offset.checked_add(x))
            })
            .ok_or(ImageError::UnexpectedFormat)?;
        let end = start
            .checked_add(crop_stride)
            .filter(|end| *end <= pixels.len())
            .ok_or(ImageError::UnexpectedFormat)?;
        cropped.extend_from_slice(&pixels[start..end]);
    }
    if cropped.len() != crop_bytes {
        return Err(ImageError::UnexpectedFormat);
    }
    let mut output = Vec::new();
    {
        let mut png_encoder = png::Encoder::new(&mut output, rect.width, rect.height);
        png_encoder.set_color(png::ColorType::Rgba);
        png_encoder.set_depth(png::BitDepth::Eight);
        let mut writer = png_encoder.write_header()?;
        writer.write_image_data(&cropped)?;
    }
    let output_length = u64::try_from(output.len()).unwrap_or(u64::MAX);
    if output_length > max_encoded_bytes {
        return Err(ImageError::TooLarge {
            actual: output_length,
            maximum: max_encoded_bytes,
        });
    }
    Ok(output)
}

fn image_tool_error(error: &ImageError) -> CallToolResult {
    let value = json!({
        "code": "screenshot_artifact_invalid",
        "message": error.to_string(),
        "retryable": true,
        "recovery_action": "observe_without_screenshot_or_reduce_target_size",
        "mutation_status": "not_applicable"
    });
    let mut result = CallToolResult::error(vec![ContentBlock::text(format!(
        "Screenshot could not be returned safely: {error}"
    ))]);
    result.structured_content = Some(value);
    result
}

/// Session bootstrap failure before MCP starts accepting calls.
#[derive(Debug, Error)]
pub enum BootstrapError {
    /// Runtime rejected discovery or session creation.
    #[error("nexus-cua runtime rejected MCP bootstrap: {0:?}")]
    Runtime(CuaError),
    /// The bounded in-process mutation dispatcher could not be constructed.
    #[error("nexus-cua MCP mutation dispatcher configuration is invalid: {0}")]
    Transport(String),
    /// Discovery result was truncated, so authority could not be resolved completely.
    #[error("application discovery was incomplete; increase the trusted runtime bound")]
    IncompleteDiscovery,
    /// Installed-application discovery was truncated.
    #[error("installed application discovery was incomplete; increase the trusted runtime bound")]
    IncompleteLaunchDiscovery,
    /// At least one exact configured identity was not running.
    #[error("authorized applications are not running: {0:?}")]
    ApplicationsUnavailable(Vec<String>),
    /// Runtime returned an impossible command/result pairing.
    #[error("nexus-cua returned an unexpected result during MCP bootstrap")]
    UnexpectedResult,
}

impl From<CuaError> for BootstrapError {
    fn from(error: CuaError) -> Self {
        Self::Runtime(error)
    }
}

#[derive(Debug, Error)]
enum ImageError {
    #[error("screenshot is {actual} bytes, above the configured {maximum}-byte MCP limit")]
    TooLarge { actual: u64, maximum: u64 },
    #[error("decoded screenshot exceeds the bounded image-processing limit")]
    DecodedImageTooLarge,
    #[error("screenshot is not the expected RGBA8 PNG")]
    UnexpectedFormat,
    #[error("screenshot PNG decoding failed: {0}")]
    Decode(#[from] png::DecodingError),
    #[error("zoom PNG encoding failed: {0}")]
    Encode(#[from] png::EncodingError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_cua_protocol::{PixelSize, ScreenRect, ScreenshotMapping};

    #[test]
    fn vision_profile_hides_the_complex_action_union_and_keeps_flat_controls() {
        let tools = NexusCuaMcp::router_for_profile(ToolProfile::Vision).list_all();
        let names = tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<HashSet<_>>();
        assert!(!names.contains("perform_action"));
        assert!(names.iter().all(|name| !name.starts_with("nexus_cua_")));
        for required in [
            "read_clipboard",
            "write_clipboard",
            "list_displays",
            "observe_display",
            "observe_window",
            "zoom",
            "click_normalized",
            "click_view_normalized",
            "drag_normalized",
            "type_text",
            "press_keys",
            "scroll",
            "invoke_element",
            "set_element_value",
            "set_range_value",
            "reconcile_mutation",
        ] {
            assert!(names.contains(required), "missing vision tool {required}");
        }

        for tool in tools.iter().filter(|tool| {
            matches!(
                tool.name.as_ref(),
                "zoom"
                    | "click_normalized"
                    | "click_view_normalized"
                    | "drag_normalized"
                    | "type_text"
                    | "press_keys"
                    | "scroll"
                    | "invoke_element"
                    | "set_element_value"
                    | "set_range_value"
            )
        }) {
            let schema = serde_json::to_value(tool.input_schema.as_ref()).unwrap();
            assert_eq!(schema["type"], "object", "{} root type", tool.name);
            assert_eq!(
                schema["additionalProperties"],
                Value::Bool(false),
                "{} must reject unknown fields",
                tool.name
            );
            assert!(
                serde_json::to_vec(&schema).unwrap().len() <= 16 * 1024,
                "{} schema is too large for the compact visual surface",
                tool.name
            );
            assert!(
                !contains_schema_keyword(&schema, "oneOf"),
                "{} unexpectedly contains a complex oneOf union: {schema}",
                tool.name,
            );
        }
    }

    #[test]
    fn every_mutation_schema_requires_an_explicit_request_id() {
        let tools = NexusCuaMcp::router_for_profile(ToolProfile::Full).list_all();
        for name in [
            "write_clipboard",
            "launch_application",
            "perform_action",
            "terminate_application",
            "click_normalized",
            "click_view_normalized",
            "type_text",
            "press_keys",
            "scroll",
            "drag_normalized",
            "invoke_element",
            "set_element_value",
            "set_range_value",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name.as_ref() == name)
                .unwrap_or_else(|| panic!("missing mutation tool {name}"));
            let schema = serde_json::to_value(tool.input_schema.as_ref()).unwrap();
            assert!(
                schema["properties"].get("request_id").is_some(),
                "{name} has no request_id property"
            );
            assert!(
                schema["required"]
                    .as_array()
                    .is_some_and(|required| required.iter().any(|field| field == "request_id")),
                "{name} does not require request_id"
            );
        }
    }

    fn contains_schema_keyword(value: &Value, keyword: &str) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key(keyword)
                    || object
                        .values()
                        .any(|value| contains_schema_keyword(value, keyword))
            }
            Value::Array(values) => values
                .iter()
                .any(|value| contains_schema_keyword(value, keyword)),
            _ => false,
        }
    }

    #[test]
    fn structured_observation_hides_the_private_artifact_path() {
        let mut value = json!({
            "observation_id": "observation_test",
            "screenshot": {
                "artifact_ref": "artifact_test",
                "path": "/private/runtime/session/artifact.png",
                "byte_length": 42
            }
        });

        remove_private_artifact_path(&mut value);

        assert!(value["screenshot"].get("path").is_none());
        assert_eq!(value["screenshot"]["artifact_ref"], "artifact_test");
    }

    #[test]
    fn image_cache_enforces_an_aggregate_byte_ceiling() {
        let mapping = ScreenshotMapping {
            screen_bounds: ScreenRect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            pixel_size: PixelSize {
                width: 1,
                height: 1,
            },
        };
        let image_bytes = 17 * 1024 * 1024;
        let mut cache = ImageMappingCache::default();
        cache.insert(
            ObservationId::new("observation_one"),
            WindowRef::new("window_one"),
            mapping,
            vec![0; image_bytes],
        );
        cache.insert(
            ObservationId::new("observation_two"),
            WindowRef::new("window_two"),
            mapping,
            vec![0; image_bytes],
        );

        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.cached_bytes, image_bytes as u64);
        assert!(cache.cached_bytes <= MAX_CACHED_IMAGE_BYTES);
    }

    #[test]
    fn normalized_points_cover_the_exact_image_without_resize_assumptions() {
        let size = PixelSize {
            width: 1_920,
            height: 1_504,
        };
        assert_eq!(normalized_point(0, 0, size), ScreenshotPoint { x: 0, y: 0 });
        assert_eq!(
            normalized_point(500, 500, size),
            ScreenshotPoint { x: 960, y: 752 }
        );
        assert_eq!(
            normalized_point(1_000, 1_000, size),
            ScreenshotPoint { x: 1_919, y: 1_503 }
        );
    }

    #[test]
    fn normalized_zoom_maps_back_to_source_pixels() {
        let size = PixelSize {
            width: 1_920,
            height: 1_504,
        };
        let rect = normalized_region(250, 200, 750, 800, size).unwrap();
        assert_eq!(
            rect,
            PixelRect {
                x: 480,
                y: 300,
                width: 960,
                height: 904,
            }
        );
        assert_eq!(
            normalized_view_point(500, 500, rect),
            ScreenshotPoint { x: 960, y: 752 }
        );
    }

    #[test]
    fn zoom_crop_keeps_rgba_row_and_column_identity() {
        let source = [
            255, 0, 0, 255, // top-left red
            0, 255, 0, 255, // top-right green
            0, 0, 255, 255, // bottom-left blue
            255, 255, 255, 255, // bottom-right white
        ];
        let mut source_png = Vec::new();
        {
            let mut png_encoder = png::Encoder::new(&mut source_png, 2, 2);
            png_encoder.set_color(png::ColorType::Rgba);
            png_encoder.set_depth(png::BitDepth::Eight);
            let mut writer = png_encoder.write_header().unwrap();
            writer.write_image_data(&source).unwrap();
        }
        let crop = crop_png(
            &source_png,
            PixelSize {
                width: 2,
                height: 2,
            },
            PixelRect {
                x: 1,
                y: 1,
                width: 1,
                height: 1,
            },
            1024,
        )
        .unwrap();
        let decoder = png::Decoder::new(Cursor::new(crop));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(&pixels[..info.buffer_size()], &[255, 255, 255, 255]);
    }
}
