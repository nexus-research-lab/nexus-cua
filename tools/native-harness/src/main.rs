//! Maintainer harness for deterministic native fixture validation.

use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use nexus_cua_protocol::{
    AccessibilityMode, Action, ActionKind, AuthorizationToken, CapabilityManifest, Command,
    CommandResult, CuaError, DeliveryMode, DiscoverApplicationsOutput, ErrorCode, ListWindowsInput,
    MutationStatus, OpenSessionInput, PROTOCOL_VERSION, PerformActionInput, PermissionMode,
    Platform, PointerButton, RequestEnvelope, RequestId, ResponseOutcome, ScreenRect,
    ScreenshotPoint, SensitiveText, SessionInput, WindowObservation, WindowSummary,
};
use nexus_cua_transport::LocalEndpoint;
use serde_json::{Value, json};
use uuid::Uuid;

mod evidence;
mod fault;
mod performance;
mod permission;
mod restart;

const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Parser)]
#[command(name = "nexus-cua-native-harness", version, about)]
struct Cli {
    /// Private local sidecar endpoint.
    #[arg(long)]
    endpoint: Option<String>,
    /// Host-private transport-token file.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Case-sensitive substring of the fixture application name, id, or path.
    #[arg(long, default_value = "nexus-cua-native-fixture")]
    application_match: String,
    /// Prefix of the exact fixture window title.
    #[arg(long, default_value = "Nexus CUA Native Fixture · Generation ")]
    window_title_prefix: String,
    /// Machine-readable cross-platform fixture contract.
    #[arg(long, default_value = "fixtures/native/contract.json")]
    contract_file: PathBuf,
    /// Maximum encoded request or response size.
    #[arg(long, default_value_t = DEFAULT_MAX_FRAME_BYTES)]
    max_frame_bytes: usize,
    #[command(subcommand)]
    command: HarnessCommand,
}

#[derive(Debug, Subcommand)]
enum HarnessCommand {
    /// Inspect discovery, permissions, exact-window capture, and semantic output.
    Inspect {
        /// Skip pixel capture while inspecting discovery and semantics.
        #[arg(long)]
        no_screenshot: bool,
    },
    /// Exercise the required read and semantic-mutation fixture path.
    Validate {
        /// Fail unless every required scenario is automated and passed.
        #[arg(long)]
        enforce: bool,
    },
    /// Measure warm end-to-end latency distributions against absolute budgets.
    Benchmark(performance::BenchmarkArgs),
    /// Exercise idle or active lifecycle/resource behavior for a bounded duration.
    Soak(performance::SoakArgs),
    /// Prove bounded accessibility-provider failure and mutation disposition.
    Fault(fault::FaultArgs),
    /// Prove operating-system denial, revocation, and protected-target behavior.
    Permission(permission::PermissionArgs),
    /// Merge raw native reports into one release-gate evidence summary.
    Evidence(evidence::EvidenceArgs),
    /// Leave a live session and artifact for an orchestrated sidecar restart.
    RestartPrepare(restart::RestartArgs),
    /// Prove a prepared session is unavailable after an orchestrated restart.
    RestartVerify(restart::RestartArgs),
}

struct Client {
    endpoint: LocalEndpoint,
    authorization: AuthorizationToken,
    max_frame_bytes: usize,
    sequence: u64,
}

impl Client {
    fn new(cli: &Cli) -> Result<Self, Box<dyn Error>> {
        let endpoint = cli
            .endpoint
            .as_ref()
            .ok_or("--endpoint is required for live native commands")?;
        let token_file = cli
            .token_file
            .as_ref()
            .ok_or("--token-file is required for live native commands")?;
        let token = std::fs::read_to_string(token_file)?;
        let token = token.trim();
        if token.is_empty() {
            return Err("transport token file is empty".into());
        }
        Ok(Self {
            endpoint: LocalEndpoint::new(endpoint.clone())?,
            authorization: AuthorizationToken::new(token),
            max_frame_bytes: cli.max_frame_bytes,
            sequence: 0,
        })
    }

    async fn send(&mut self, command: Command) -> Result<CommandResult, Box<dyn Error>> {
        match self.send_outcome(command).await? {
            Ok(result) => Ok(result),
            Err(error) => Err(format!(
                "runtime error {:?}/{:?}: {} ({:?})",
                error.code, error.mutation_status, error.message, error.recovery_action
            )
            .into()),
        }
    }

    async fn send_outcome(
        &mut self,
        command: Command,
    ) -> Result<Result<CommandResult, CuaError>, Box<dyn Error>> {
        self.sequence = self.sequence.wrapping_add(1);
        self.send_outcome_with(
            RequestId::new(format!(
                "native_harness_{}_{}_{}",
                std::process::id(),
                self.sequence,
                Uuid::new_v4().simple()
            )),
            30_000,
            command,
        )
        .await
    }

    async fn send_outcome_with(
        &self,
        request_id: RequestId,
        timeout_ms: u32,
        command: Command,
    ) -> Result<Result<CommandResult, CuaError>, Box<dyn Error>> {
        let request = RequestEnvelope {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            request_id,
            timeout_ms,
            authorization: self.authorization.clone(),
            command,
        };
        let response =
            nexus_cua_transport::request(&self.endpoint, &request, self.max_frame_bytes).await?;
        match response.outcome {
            ResponseOutcome::Success { result } => Ok(Ok(result)),
            ResponseOutcome::Error { error } => Ok(Err(error)),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    if let HarnessCommand::Evidence(args) = &cli.command {
        return evidence::summarize(args);
    }
    let mut client = Client::new(&cli)?;
    match &cli.command {
        HarnessCommand::Inspect { no_screenshot } => {
            inspect(&cli, &mut client, !*no_screenshot).await
        }
        HarnessCommand::Validate { enforce } => validate(&cli, &mut client, *enforce).await,
        HarnessCommand::Benchmark(args) => performance::benchmark(&cli, &mut client, args).await,
        HarnessCommand::Soak(args) => performance::soak(&cli, &mut client, args).await,
        HarnessCommand::Fault(args) => fault::validate(&cli, &mut client, args).await,
        HarnessCommand::Permission(args) => permission::validate(&cli, &mut client, args).await,
        HarnessCommand::RestartPrepare(args) => restart::prepare(&cli, &mut client, args).await,
        HarnessCommand::RestartVerify(args) => restart::verify(&mut client, args).await,
        HarnessCommand::Evidence(_) => unreachable!("evidence returned before IPC setup"),
    }
}

async fn validate(cli: &Cli, client: &mut Client, enforce: bool) -> Result<(), Box<dyn Error>> {
    let capabilities = match client.send(Command::GetCapabilities).await? {
        CommandResult::Capabilities(value) => value,
        other => return Err(unexpected("capabilities", &other)),
    };
    let permissions = match client.send(Command::GetPermissionStatus).await? {
        CommandResult::PermissionStatus(value) => value,
        other => return Err(unexpected("permission status", &other)),
    };
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("application discovery", &other)),
    };
    let application = select_application(&discovered, &cli.application_match)?;
    let session = match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::Bounded,
                application_refs: vec![application.discovery_ref.clone()],
                allowed_actions: capabilities.actions.clone(),
                allow_foreground_input: true,
                ttl_seconds: 180,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => value,
        other => return Err(unexpected("open session", &other)),
    };
    let session_id = session.session_id.clone();
    let validation = validate_session(cli, client, &session_id, capabilities.platform)
        .await
        .map_err(|error| format!("core fixture path failed: {error}"));
    let close = client
        .send(Command::CloseSession(SessionInput {
            session_id: session.session_id,
        }))
        .await;
    let (mut report, artifact_path) = validation?;
    close?;
    if let Some(path) = artifact_path {
        if std::path::Path::new(&path).exists() {
            return Err("session close did not delete its screenshot artifact".into());
        }
        report["scenario_groups"]["artifact_cleanup"] = json!("passed");
    }
    validate_session_expiry(cli, client)
        .await
        .map_err(|error| format!("session-expiry path failed: {error}"))?;
    report["scenario_groups"]["session_expiry"] = json!("passed");
    validate_desktop_conditions(cli, client, &capabilities)
        .await
        .map_err(|error| format!("desktop-condition path failed: {error}"))?;
    report["scenario_groups"]["minimized_window"] = json!("passed");
    report["scenario_groups"]["occluded_window"] = json!("passed");
    report["permissions"] = serde_json::to_value(permissions)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if enforce && report["status"] != "passed" {
        return Err("native validation has required scenarios that did not pass".into());
    }
    Ok(())
}

async fn validate_session(
    cli: &Cli,
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    platform: Platform,
) -> Result<(Value, Option<String>), Box<dyn Error>> {
    let windows = list_windows(client, session_id).await?;
    let window = select_window(windows, &cli.window_title_prefix)?;
    let reconciled = reconcile_observation(client, session_id, &window).await?;
    validate_initial_observation(&window, &reconciled)?;
    let baseline = observe(client, session_id, &window, true, AccessibilityMode::Full).await?;
    validate_initial_observation(&window, &baseline)?;
    let interactive = observe(
        client,
        session_id,
        &window,
        false,
        AccessibilityMode::Interactive,
    )
    .await?;
    if interactive.elements.len() > baseline.elements.len()
        || find_actionable(&interactive, "Increment Counter", "invoke").is_err()
        || find_actionable(&interactive, "Fixture Text", "set_value").is_err()
    {
        return Err("interactive accessibility view is not a bounded actionable subset".into());
    }
    // Observations are deliberately single-current. Comparing the interactive
    // view invalidates `baseline`, so take a fresh full snapshot for mutation.
    let initial = observe(client, session_id, &window, true, AccessibilityMode::Full).await?;
    validate_initial_observation(&window, &initial)?;
    validate_fixture_contract(cli, platform, &initial)?;
    let artifact_path = initial
        .screenshot
        .as_ref()
        .map(|artifact| artifact.path.clone());

    let increment = find_actionable(&initial, "Increment Counter", "invoke")?;
    perform(
        client,
        session_id,
        &window,
        &initial,
        Action::InvokeElement {
            element_ref: increment.element_ref.clone(),
        },
        ActionKind::InvokeElement,
    )
    .await?;
    let stale_retry = client
        .send_outcome(Command::PerformAction(PerformActionInput {
            session_id: session_id.clone(),
            window_ref: window.window_ref.clone(),
            observation_id: initial.observation_id.clone(),
            action: Action::InvokeElement {
                element_ref: increment.element_ref.clone(),
            },
        }))
        .await?;
    match stale_retry {
        Err(error) if error.code == ErrorCode::StaleObservation => {}
        Err(error) => {
            return Err(format!(
                "invalidated observation returned {:?}, expected stale_observation",
                error.code
            )
            .into());
        }
        Ok(_) => return Err("invalidated observation was accepted for a second mutation".into()),
    }

    let after_increment =
        observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    require_state(&after_increment, &["counter=1"])?;
    let text = find_actionable(&after_increment, "Fixture Text", "set_value")?;
    perform(
        client,
        session_id,
        &window,
        &after_increment,
        Action::SetValue {
            element_ref: text.element_ref.clone(),
            value: SensitiveText::new("semantic-ready"),
        },
        ActionKind::SetValue,
    )
    .await?;
    let after_text = observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    if find_named(&after_text, "Fixture Text")?.value.as_deref() != Some("semantic-ready") {
        return Err("semantic set-value did not update Fixture Text".into());
    }

    let toggle = find_actionable(&after_text, "Enable Feature", "toggle")?;
    perform(
        client,
        session_id,
        &window,
        &after_text,
        Action::ToggleElement {
            element_ref: toggle.element_ref.clone(),
        },
        ActionKind::ToggleElement,
    )
    .await?;
    let after_toggle = observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    require_state(&after_toggle, &["checked=true"])?;

    let beta = find_actionable(&after_toggle, "Beta", "select")?;
    perform(
        client,
        session_id,
        &window,
        &after_toggle,
        Action::SelectElement {
            element_ref: beta.element_ref.clone(),
        },
        ActionKind::SelectElement,
    )
    .await?;
    let after_select = observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    require_state(&after_select, &["selection=Beta"])?;

    let expandable = find_actionable(&after_select, "Advanced Options", "set_expanded")?;
    perform(
        client,
        session_id,
        &window,
        &after_select,
        Action::SetExpanded {
            element_ref: expandable.element_ref.clone(),
            expanded: true,
        },
        ActionKind::SetExpanded,
    )
    .await?;
    let after_expand = observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    require_state(&after_expand, &["expanded=true"])?;

    perform(
        client,
        session_id,
        &window,
        &after_expand,
        Action::FocusWindow,
        ActionKind::FocusWindow,
    )
    .await?;
    let after_focus = observe(client, session_id, &window, true, AccessibilityMode::Full).await?;
    require_state(&after_focus, &["counter=1"])?;
    let increment_point = element_screenshot_point(&after_focus, "Increment Counter")?;
    perform(
        client,
        session_id,
        &window,
        &after_focus,
        Action::MovePointer {
            point: increment_point,
            duration_ms: 80,
        },
        ActionKind::MovePointer,
    )
    .await?;
    let after_move = observe(client, session_id, &window, true, AccessibilityMode::Full).await?;
    require_state(&after_move, &["counter=1"])?;
    let increment_point = element_screenshot_point(&after_move, "Increment Counter")?;
    perform(
        client,
        session_id,
        &window,
        &after_move,
        Action::ClickPoint {
            point: increment_point,
            button: PointerButton::Left,
            count: 1,
        },
        ActionKind::ClickPoint,
    )
    .await?;
    let after_click = observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    require_state(&after_click, &["counter=2"])?;

    let text = find_actionable(&after_click, "Fixture Text", "focus")?;
    perform(
        client,
        session_id,
        &window,
        &after_click,
        Action::FocusElement {
            element_ref: text.element_ref.clone(),
        },
        ActionKind::FocusElement,
    )
    .await?;
    let after_text_focus =
        observe(client, session_id, &window, true, AccessibilityMode::Full).await?;
    if !find_named(&after_text_focus, "Fixture Text")?.focused {
        return Err("semantic focus did not mark Fixture Text as focused".into());
    }
    let text_point = element_screenshot_point(&after_text_focus, "Fixture Text")?;
    perform(
        client,
        session_id,
        &window,
        &after_text_focus,
        Action::ClickPoint {
            point: text_point,
            button: PointerButton::Left,
            count: 1,
        },
        ActionKind::ClickPoint,
    )
    .await?;
    let after_text_click =
        observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    let select_all = match platform {
        Platform::Macos => vec!["meta".to_owned(), "a".to_owned()],
        Platform::Windows => vec!["control".to_owned(), "a".to_owned()],
        Platform::Unsupported => return Err("native fixture platform is unsupported".into()),
    };
    perform(
        client,
        session_id,
        &window,
        &after_text_click,
        Action::PressKeys { keys: select_all },
        ActionKind::PressKeys,
    )
    .await?;
    let after_select_all =
        observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    perform(
        client,
        session_id,
        &window,
        &after_select_all,
        Action::TypeText {
            text: SensitiveText::new("foreground-ready"),
        },
        ActionKind::TypeText,
    )
    .await?;
    wait_for_element_value(
        client,
        session_id,
        &window,
        "Fixture Text",
        "foreground-ready",
    )
    .await?;
    let after_type = observe(client, session_id, &window, true, AccessibilityMode::Full).await?;

    let scroll_point = element_screenshot_point(&after_type, "Fixture Scroll Region")?;
    perform(
        client,
        session_id,
        &window,
        &after_type,
        Action::MovePointer {
            point: scroll_point,
            duration_ms: 60,
        },
        ActionKind::MovePointer,
    )
    .await?;
    let before_scroll =
        observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    perform(
        client,
        session_id,
        &window,
        &before_scroll,
        Action::Scroll {
            element_ref: None,
            delta_x: 0.0,
            delta_y: -180.0,
        },
        ActionKind::Scroll,
    )
    .await?;
    let after_scroll = observe(client, session_id, &window, true, AccessibilityMode::Full).await?;
    let drag_from = element_screenshot_point(&after_scroll, "Drag Token")?;
    let drag_to = element_screenshot_point(&after_scroll, "Drop Zone")?;
    perform(
        client,
        session_id,
        &window,
        &after_scroll,
        Action::Drag {
            from: drag_from,
            to: drag_to,
            duration_ms: 320,
        },
        ActionKind::Drag,
    )
    .await?;
    let after_drag = observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    require_state(&after_drag, &["dropped=true"])?;

    let geometry = find_actionable(&after_drag, "Change Fixture Geometry", "invoke")?;
    perform(
        client,
        session_id,
        &window,
        &after_drag,
        Action::InvokeElement {
            element_ref: geometry.element_ref.clone(),
        },
        ActionKind::InvokeElement,
    )
    .await?;
    let moved_window = wait_for_geometry_change(
        client,
        session_id,
        &cli.window_title_prefix,
        window.screen_bounds,
    )
    .await?;
    let (display_window, display_conditions) =
        validate_display_transition(client, session_id, &moved_window).await?;

    let before_replace = observe(
        client,
        session_id,
        &display_window,
        false,
        AccessibilityMode::Full,
    )
    .await?;
    let replace = find_actionable(&before_replace, "Replace Fixture Window", "invoke")?;
    perform(
        client,
        session_id,
        &display_window,
        &before_replace,
        Action::InvokeElement {
            element_ref: replace.element_ref.clone(),
        },
        ActionKind::InvokeElement,
    )
    .await?;
    let replacement = wait_for_window_replacement(
        client,
        session_id,
        &cli.window_title_prefix,
        &display_window,
    )
    .await?;
    wait_for_window_unavailable(client, session_id, &display_window).await?;
    if !replacement.title.ends_with('2') {
        return Err("fixture window replacement did not advance to generation 2".into());
    }
    let topology_complete = ["multiple_display", "mixed_dpi", "negative_coordinates"]
        .into_iter()
        .all(|scenario| display_conditions[scenario] == "passed");

    Ok((
        json!({
            "fixture_contract": "nexus.cua.fixture.v1",
            "status": if topology_complete { "passed" } else { "partial" },
            "platform": platform,
            "architecture": std::env::consts::ARCH,
            "scenario_groups": {
                "discovery_and_identity": "passed",
                "observation": "passed",
                "secure_redaction": "passed",
                "semantic_mutation": "passed",
                "foreground_mutation": "passed",
                "same_request_reconciliation": "passed",
                "stale_observation": "passed",
                "window_generation": "passed",
                "multiple_display": display_conditions["multiple_display"],
                "mixed_dpi": display_conditions["mixed_dpi"],
                "negative_coordinates": display_conditions["negative_coordinates"],
            },
            "automated_topology_complete": topology_complete,
            "window_generation": window.title,
            "element_count": initial.elements.len(),
            "screenshot_bytes": initial.screenshot.as_ref().map(|value| value.byte_length),
        }),
        artifact_path,
    ))
}

async fn validate_display_transition(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    window: &WindowSummary,
) -> Result<(WindowSummary, Value), Box<dyn Error>> {
    let before_image = observe(client, session_id, window, true, AccessibilityMode::Full).await?;
    let before = observe(client, session_id, window, false, AccessibilityMode::Full).await?;
    let next_display = find_actionable(&before, "Move Fixture To Next Display", "invoke")?;
    let before_scale = screenshot_scale(&before_image)?;
    perform(
        client,
        session_id,
        window,
        &before,
        Action::InvokeElement {
            element_ref: next_display.element_ref.clone(),
        },
        ActionKind::InvokeElement,
    )
    .await?;
    let Some(moved) = wait_for_optional_geometry_change(
        client,
        session_id,
        &window.window_ref,
        window.screen_bounds,
    )
    .await?
    else {
        return Ok((
            window.clone(),
            json!({
                "multiple_display": "manual_required",
                "mixed_dpi": "manual_required",
                "negative_coordinates": "manual_required",
            }),
        ));
    };
    let after_move = observe(client, session_id, &moved, true, AccessibilityMode::Full).await?;
    let after_scale = screenshot_scale(&after_move)?;
    let point = element_screenshot_point(&after_move, "Increment Counter")?;
    perform(
        client,
        session_id,
        &moved,
        &after_move,
        Action::ClickPoint {
            point,
            button: PointerButton::Left,
            count: 1,
        },
        ActionKind::ClickPoint,
    )
    .await?;
    let after_click = observe(client, session_id, &moved, false, AccessibilityMode::Full).await?;
    require_state(&after_click, &["counter=3"])?;
    let mixed_dpi = (before_scale.0 - after_scale.0).abs() > 0.01
        || (before_scale.1 - after_scale.1).abs() > 0.01;
    let negative = moved.screen_bounds.x < 0.0 || moved.screen_bounds.y < 0.0;
    Ok((
        moved,
        json!({
            "multiple_display": "passed",
            "mixed_dpi": if mixed_dpi { "passed" } else { "manual_required" },
            "negative_coordinates": if negative { "passed" } else { "manual_required" },
        }),
    ))
}

fn screenshot_scale(observation: &WindowObservation) -> Result<(f64, f64), Box<dyn Error>> {
    let screenshot = observation
        .screenshot
        .as_ref()
        .ok_or("display transition observation omitted its screenshot")?;
    let bounds = screenshot.mapping.screen_bounds;
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Err("display transition screenshot has invalid bounds".into());
    }
    Ok((
        f64::from(screenshot.mapping.pixel_size.width) / bounds.width,
        f64::from(screenshot.mapping.pixel_size.height) / bounds.height,
    ))
}

async fn wait_for_optional_geometry_change(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    window_ref: &nexus_cua_protocol::WindowRef,
    previous_bounds: ScreenRect,
) -> Result<Option<WindowSummary>, Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(window) = list_windows(client, session_id)
            .await?
            .into_iter()
            .find(|candidate| &candidate.window_ref == window_ref)
            && window.screen_bounds != previous_bounds
        {
            return Ok(Some(window));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn reconcile_observation(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    window: &WindowSummary,
) -> Result<WindowObservation, Box<dyn Error>> {
    let request_id = RequestId::new(format!(
        "native_harness_reconcile_{}_{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let command = Command::ObserveWindow(nexus_cua_protocol::ObserveWindowInput {
        session_id: session_id.clone(),
        window_ref: window.window_ref.clone(),
        include_screenshot: true,
        accessibility: AccessibilityMode::Full,
    });
    match client
        .send_outcome_with(request_id.clone(), 1, command.clone())
        .await?
    {
        Err(error) if error.code == ErrorCode::DeadlineExceeded => {}
        Err(error) => {
            return Err(format!(
                "short reconciliation request returned {:?}, expected deadline_exceeded",
                error.code
            )
            .into());
        }
        Ok(_) => return Err("1 ms observation unexpectedly completed before its deadline".into()),
    }
    match client
        .send_outcome_with(request_id, 30_000, command)
        .await?
    {
        Ok(CommandResult::WindowObserved(value)) => Ok(*value),
        Ok(other) => Err(unexpected("reconciled observation", &other)),
        // Reconciliation promises the terminal result of the admitted request,
        // not that the operation itself must succeed. A provider can be
        // transiently unavailable while the 1 ms probe is in flight; receiving
        // that stable terminal error still proves the ledger contract. Use a
        // new request for the fixture correctness path after reconciliation.
        Err(error)
            if matches!(
                error.code,
                ErrorCode::TargetUnavailable | ErrorCode::TargetUnresponsive
            ) && error.mutation_status == MutationStatus::NotApplicable =>
        {
            observe(client, session_id, window, true, AccessibilityMode::Full).await
        }
        Err(error) => Err(format!(
            "same-request reconciliation returned {:?}: {}",
            error.code, error.message
        )
        .into()),
    }
}

async fn validate_session_expiry(cli: &Cli, client: &mut Client) -> Result<(), Box<dyn Error>> {
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("application discovery", &other)),
    };
    let application = select_application(&discovered, &cli.application_match)?;
    let session = match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::ReadOnly,
                application_refs: vec![application.discovery_ref.clone()],
                allowed_actions: Vec::new(),
                allow_foreground_input: false,
                ttl_seconds: 3,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => value,
        other => return Err(unexpected("expiry session", &other)),
    };
    let window = select_window(
        list_windows(client, &session.session_id).await?,
        &cli.window_title_prefix,
    )?;
    let observation = observe(
        client,
        &session.session_id,
        &window,
        true,
        AccessibilityMode::Disabled,
    )
    .await?;
    let artifact_path = observation
        .screenshot
        .as_ref()
        .ok_or("expiry observation did not create a screenshot artifact")?
        .path
        .clone();
    tokio::time::sleep(Duration::from_millis(3_250)).await;
    match client
        .send_outcome(Command::ListWindows(ListWindowsInput {
            session_id: session.session_id,
            app_ref: None,
        }))
        .await?
    {
        Err(error) if error.code == ErrorCode::SessionUnavailable => {}
        Err(error) => {
            return Err(format!(
                "expired session returned {:?}, expected session_unavailable",
                error.code
            )
            .into());
        }
        Ok(_) => return Err("expired session remained usable after its deadline".into()),
    }
    if std::path::Path::new(&artifact_path).exists() {
        return Err("expired session retained its screenshot artifact".into());
    }
    Ok(())
}

async fn validate_desktop_conditions(
    cli: &Cli,
    client: &mut Client,
    capabilities: &nexus_cua_protocol::DriverCapabilities,
) -> Result<(), Box<dyn Error>> {
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("application discovery", &other)),
    };
    let application = select_application(&discovered, &cli.application_match)?;
    let session = match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::Bounded,
                application_refs: vec![application.discovery_ref.clone()],
                allowed_actions: capabilities.actions.clone(),
                allow_foreground_input: true,
                ttl_seconds: 60,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => value,
        other => return Err(unexpected("desktop-condition session", &other)),
    };
    let result = validate_desktop_session(cli, client, &session.session_id).await;
    let close = client
        .send(Command::CloseSession(SessionInput {
            session_id: session.session_id,
        }))
        .await;
    result?;
    close?;
    Ok(())
}

async fn validate_desktop_session(
    cli: &Cli,
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
) -> Result<(), Box<dyn Error>> {
    let window = select_window(
        list_windows(client, session_id).await?,
        &cli.window_title_prefix,
    )?;
    let before_occlusion =
        observe(client, session_id, &window, true, AccessibilityMode::Full).await?;
    let toggle_occluder = find_actionable(&before_occlusion, "Toggle Fixture Occluder", "invoke")?;
    perform(
        client,
        session_id,
        &window,
        &before_occlusion,
        Action::InvokeElement {
            element_ref: toggle_occluder.element_ref.clone(),
        },
        ActionKind::InvokeElement,
    )
    .await?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let occluded = observe(client, session_id, &window, true, AccessibilityMode::Full).await?;
    let screenshot = occluded
        .screenshot
        .as_ref()
        .ok_or("occluded exact-window observation omitted its screenshot")?;
    if png_contains_magenta_occluder(&screenshot.path)? {
        return Err("exact-window screenshot contains pixels from the magenta occluder".into());
    }
    let toggle_occluder = find_actionable(&occluded, "Toggle Fixture Occluder", "invoke")?;
    perform(
        client,
        session_id,
        &window,
        &occluded,
        Action::InvokeElement {
            element_ref: toggle_occluder.element_ref.clone(),
        },
        ActionKind::InvokeElement,
    )
    .await?;

    let before_minimize =
        observe(client, session_id, &window, false, AccessibilityMode::Full).await?;
    let minimize = find_actionable(&before_minimize, "Minimize Fixture Window", "invoke")?;
    perform(
        client,
        session_id,
        &window,
        &before_minimize,
        Action::InvokeElement {
            element_ref: minimize.element_ref.clone(),
        },
        ActionKind::InvokeElement,
    )
    .await?;
    wait_for_minimized(client, session_id, &window).await?;
    match client
        .send_outcome(Command::ObserveWindow(
            nexus_cua_protocol::ObserveWindowInput {
                session_id: session_id.clone(),
                window_ref: window.window_ref.clone(),
                include_screenshot: true,
                accessibility: AccessibilityMode::Disabled,
            },
        ))
        .await?
    {
        Err(error)
            if matches!(
                error.code,
                ErrorCode::TargetUnavailable | ErrorCode::Unsupported
            ) => {}
        Err(error) => {
            return Err(format!(
                "minimized observation returned {:?}, expected target_unavailable/unsupported",
                error.code
            )
            .into());
        }
        Ok(_) => {
            return Err(
                "minimized observation claimed a current screenshot instead of failing closed"
                    .into(),
            );
        }
    }
    let still_minimized = list_windows(client, session_id)
        .await?
        .into_iter()
        .find(|candidate| candidate.window_ref == window.window_ref)
        .is_some_and(|candidate| candidate.minimized);
    if !still_minimized {
        return Err("minimized observation restored or lost the target implicitly".into());
    }
    Ok(())
}

async fn wait_for_minimized(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    window: &WindowSummary,
) -> Result<(), Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if list_windows(client, session_id)
            .await?
            .into_iter()
            .find(|candidate| candidate.window_ref == window.window_ref)
            .is_some_and(|candidate| candidate.minimized)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("fixture window did not report minimized=true".into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn png_contains_magenta_occluder(path: &str) -> Result<bool, Box<dyn Error>> {
    let decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    let mut reader = decoder.read_info()?;
    let buffer_size = reader
        .output_buffer_size()
        .ok_or("PNG output buffer exceeds platform limits")?;
    let mut bytes = vec![0; buffer_size];
    let info = reader.next_frame(&mut bytes)?;
    let bytes = &bytes[..info.buffer_size()];
    let stride = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => return Err(format!("unexpected screenshot PNG color type: {other:?}").into()),
    };
    Ok(bytes
        .chunks_exact(stride)
        .any(|pixel| pixel[0] >= 240 && pixel[1] <= 20 && pixel[2] >= 240))
}

fn validate_initial_observation(
    window: &WindowSummary,
    observation: &WindowObservation,
) -> Result<(), Box<dyn Error>> {
    if observation.window_screen_bounds != window.screen_bounds {
        return Err("observation and listed-window bounds differ".into());
    }
    let screenshot = observation
        .screenshot
        .as_ref()
        .ok_or("fixture observation did not include a screenshot")?;
    if screenshot.mapping.screen_bounds != observation.window_screen_bounds
        || screenshot.mapping.pixel_size.width == 0
        || screenshot.mapping.pixel_size.height == 0
    {
        return Err("screenshot mapping does not describe the exact window".into());
    }
    for (name, action) in [
        ("Increment Counter", "invoke"),
        ("Fixture Text", "set_value"),
        ("Fixture Secure Text", "set_value"),
        ("Enable Feature", "toggle"),
        ("Beta", "select"),
        ("Advanced Options", "set_expanded"),
    ] {
        find_actionable(observation, name, action)?;
    }
    find_named(observation, "Drag Token")?;
    find_named(observation, "Drop Zone")?;
    let secure = find_named(observation, "Fixture Secure Text")?;
    if secure.value.is_some() {
        return Err("secure text value crossed the native driver boundary".into());
    }
    if serde_json::to_string(observation)?.contains("fixture-secret") {
        return Err("secure sentinel appeared in serialized observation".into());
    }
    require_state(
        observation,
        &[
            "generation=1",
            "counter=0",
            "text=ready",
            "checked=false",
            "selection=none",
            "expanded=false",
            "dropped=false",
        ],
    )
}

fn validate_fixture_contract(
    cli: &Cli,
    platform: Platform,
    observation: &WindowObservation,
) -> Result<(), Box<dyn Error>> {
    let contract: Value = serde_json::from_slice(&std::fs::read(&cli.contract_file)?)?;
    if contract["contract_version"] != "nexus.cua.fixture.v1" {
        return Err("fixture contract has an unsupported version".into());
    }
    let platform_key = match platform {
        Platform::Macos => "macos",
        Platform::Windows => "windows",
        Platform::Unsupported => return Err("fixture contract platform is unsupported".into()),
    };
    let controls = contract["controls"]
        .as_object()
        .ok_or("fixture contract controls must be an object")?;
    for (control_id, control) in controls {
        let name = control["name"]
            .as_str()
            .ok_or_else(|| format!("fixture control {control_id:?} has no name"))?;
        let roles = control["roles_by_platform"][platform_key]
            .as_array()
            .ok_or_else(|| {
                format!("fixture control {control_id:?} has no roles for {platform_key}")
            })?;
        let required_actions = control["required_actions"]
            .as_array()
            .ok_or_else(|| format!("fixture control {control_id:?} has no required_actions"))?;
        let matches = observation.elements.iter().any(|element| {
            let name_matches = element.name == name
                || (name == "Fixture State" && element.name.starts_with("Fixture State:"));
            name_matches
                && roles
                    .iter()
                    .any(|role| role.as_str() == Some(&element.role))
                && required_actions.iter().all(|action| {
                    action.as_str().is_some_and(|action| {
                        element.actions.iter().any(|candidate| candidate == action)
                    })
                })
        });
        if !matches {
            return Err(format!(
                "observation does not satisfy fixture control {control_id:?} ({name:?}) on {platform_key}"
            )
            .into());
        }
    }
    Ok(())
}

async fn perform(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    window: &WindowSummary,
    observation: &WindowObservation,
    action: Action,
    expected_kind: ActionKind,
) -> Result<(), Box<dyn Error>> {
    if action.kind() != expected_kind {
        return Err("harness action-kind assertion failed".into());
    }
    let expected_delivery = if action.requires_foreground() {
        DeliveryMode::Foreground
    } else {
        DeliveryMode::Semantic
    };
    let result = client
        .send(Command::PerformAction(PerformActionInput {
            session_id: session_id.clone(),
            window_ref: window.window_ref.clone(),
            observation_id: observation.observation_id.clone(),
            action,
        }))
        .await
        .map_err(|error| format!("{expected_kind:?} action failed: {error}"))?;
    match result {
        CommandResult::ActionPerformed(output)
            if output.delivery_mode == expected_delivery
                && output.dispatched
                && output.observation_invalidated =>
        {
            Ok(())
        }
        CommandResult::ActionPerformed(output) if output.delivery_mode != expected_delivery => {
            Err(format!(
                "action used {:?} delivery, expected {:?}",
                output.delivery_mode, expected_delivery
            )
            .into())
        }
        CommandResult::ActionPerformed(_) => {
            Err("action did not report dispatch plus observation invalidation".into())
        }
        other => Err(unexpected("action result", &other)),
    }
}

async fn list_windows(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
) -> Result<Vec<WindowSummary>, Box<dyn Error>> {
    match client
        .send(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: None,
        }))
        .await?
    {
        CommandResult::Windows(value) => Ok(value),
        other => Err(unexpected("window list", &other)),
    }
}

async fn wait_for_geometry_change(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    title_prefix: &str,
    previous_bounds: ScreenRect,
) -> Result<WindowSummary, Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let window = select_window(list_windows(client, session_id).await?, title_prefix)?;
        if window.screen_bounds != previous_bounds {
            return Ok(window);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("fixture geometry action did not change top-level window bounds".into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_element_value(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    window: &WindowSummary,
    element_name: &str,
    expected_value: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let observation =
            observe(client, session_id, window, false, AccessibilityMode::Full).await?;
        let actual_value = find_named(&observation, element_name)?.value.clone();
        if actual_value.as_deref() == Some(expected_value) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "foreground key and text input left {element_name:?} at {actual_value:?}; expected {expected_value:?}"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_window_replacement(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    title_prefix: &str,
    previous: &WindowSummary,
) -> Result<WindowSummary, Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(window) = list_windows(client, session_id)
            .await?
            .into_iter()
            .find(|window| {
                window.title.starts_with(title_prefix)
                    && window.window_ref != previous.window_ref
                    && window.title != previous.title
            })
        {
            return Ok(window);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("fixture window replacement did not create a new generation".into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_window_unavailable(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    previous: &WindowSummary,
) -> Result<(), Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let outcome = client
            .send_outcome(Command::ObserveWindow(
                nexus_cua_protocol::ObserveWindowInput {
                    session_id: session_id.clone(),
                    window_ref: previous.window_ref.clone(),
                    include_screenshot: false,
                    accessibility: AccessibilityMode::Disabled,
                },
            ))
            .await?;
        match outcome {
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::TargetUnavailable
                        | ErrorCode::ReferenceNotFound
                        | ErrorCode::StaleObservation
                ) =>
            {
                return Ok(());
            }
            Err(error) => {
                return Err(
                    format!("old window returned unexpected error: {:?}", error.code).into(),
                );
            }
            Ok(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(_) => return Err("destroyed top-level window remained observable".into()),
        }
    }
}

fn element_screenshot_point(
    observation: &WindowObservation,
    name: &str,
) -> Result<ScreenshotPoint, Box<dyn Error>> {
    let bounds = find_named(observation, name)?
        .screen_bounds
        .ok_or_else(|| format!("element {name:?} has no screen bounds"))?;
    let screenshot = observation
        .screenshot
        .as_ref()
        .ok_or("coordinate action requires a fresh screenshot")?;
    screen_rect_center_to_screenshot(bounds, screenshot.mapping)
        .ok_or_else(|| format!("element {name:?} does not intersect the screenshot").into())
}

fn screen_rect_center_to_screenshot(
    element: ScreenRect,
    mapping: nexus_cua_protocol::ScreenshotMapping,
) -> Option<ScreenshotPoint> {
    let screen = mapping.screen_bounds;
    let left = element.x.max(screen.x);
    let top = element.y.max(screen.y);
    let right = (element.x + element.width).min(screen.x + screen.width);
    let bottom = (element.y + element.height).min(screen.y + screen.height);
    if right <= left
        || bottom <= top
        || screen.width <= 0.0
        || screen.height <= 0.0
        || mapping.pixel_size.width == 0
        || mapping.pixel_size.height == 0
    {
        return None;
    }
    let x = (((left + right) * 0.5 - screen.x) * f64::from(mapping.pixel_size.width)
        / screen.width)
        .floor()
        .clamp(0.0, f64::from(mapping.pixel_size.width - 1));
    let y = (((top + bottom) * 0.5 - screen.y) * f64::from(mapping.pixel_size.height)
        / screen.height)
        .floor()
        .clamp(0.0, f64::from(mapping.pixel_size.height - 1));
    Some(ScreenshotPoint {
        x: x as u32,
        y: y as u32,
    })
}

fn select_window(
    windows: Vec<WindowSummary>,
    title_prefix: &str,
) -> Result<WindowSummary, Box<dyn Error>> {
    windows
        .into_iter()
        .find(|window| window.title.starts_with(title_prefix))
        .ok_or_else(|| format!("no window title starts with {title_prefix:?}").into())
}

async fn observe(
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    window: &WindowSummary,
    include_screenshot: bool,
    accessibility: AccessibilityMode,
) -> Result<WindowObservation, Box<dyn Error>> {
    const MAX_ATTEMPTS: usize = 40;

    for attempt in 0..MAX_ATTEMPTS {
        let outcome = client
            .send_outcome(Command::ObserveWindow(
                nexus_cua_protocol::ObserveWindowInput {
                    session_id: session_id.clone(),
                    window_ref: window.window_ref.clone(),
                    include_screenshot,
                    accessibility,
                },
            ))
            .await?;
        match outcome {
            Ok(CommandResult::WindowObserved(value)) => return Ok(*value),
            Ok(other) => return Err(unexpected("window observation", &other)),
            Err(error)
                if attempt + 1 < MAX_ATTEMPTS
                    && error.retryable
                    && matches!(
                        error.code,
                        ErrorCode::Busy
                            | ErrorCode::DriverFailure
                            | ErrorCode::TargetUnavailable
                            | ErrorCode::TargetUnresponsive
                    ) =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => {
                return Err(format!(
                    "runtime error {:?}/{:?}: {} ({:?})",
                    error.code, error.mutation_status, error.message, error.recovery_action
                )
                .into());
            }
        }
    }
    Err("window observation exhausted its bounded retry loop".into())
}

fn find_named<'a>(
    observation: &'a WindowObservation,
    name: &str,
) -> Result<&'a nexus_cua_protocol::AccessibilityElement, Box<dyn Error>> {
    observation
        .elements
        .iter()
        .find(|element| element.name == name)
        .ok_or_else(|| format!("observation is missing element {name:?}").into())
}

fn find_actionable<'a>(
    observation: &'a WindowObservation,
    name: &str,
    action: &str,
) -> Result<&'a nexus_cua_protocol::AccessibilityElement, Box<dyn Error>> {
    observation
        .elements
        .iter()
        .find(|element| {
            element.name == name && element.actions.iter().any(|candidate| candidate == action)
        })
        .ok_or_else(|| format!("element {name:?} does not expose action {action:?}").into())
}

fn require_state(
    observation: &WindowObservation,
    fragments: &[&str],
) -> Result<(), Box<dyn Error>> {
    let state = observation
        .elements
        .iter()
        .find(|element| element.name.starts_with("Fixture State:"))
        .ok_or("observation is missing Fixture State")?;
    for fragment in fragments {
        if !state.name.contains(fragment) {
            return Err(format!("fixture state is missing {fragment:?}: {}", state.name).into());
        }
    }
    Ok(())
}

async fn inspect(
    cli: &Cli,
    client: &mut Client,
    include_screenshot: bool,
) -> Result<(), Box<dyn Error>> {
    let capabilities = match client.send(Command::GetCapabilities).await? {
        CommandResult::Capabilities(value) => value,
        other => return Err(unexpected("capabilities", &other)),
    };
    let permissions = match client.send(Command::GetPermissionStatus).await? {
        CommandResult::PermissionStatus(value) => value,
        other => return Err(unexpected("permission status", &other)),
    };
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("application discovery", &other)),
    };
    let application = select_application(&discovered, &cli.application_match)?;
    let session = match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::ReadOnly,
                application_refs: vec![application.discovery_ref.clone()],
                allowed_actions: Vec::new(),
                allow_foreground_input: false,
                ttl_seconds: 120,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => value,
        other => return Err(unexpected("open session", &other)),
    };

    let inspection = inspect_session(cli, client, &session.session_id, include_screenshot).await;
    let close_result = client
        .send(Command::CloseSession(SessionInput {
            session_id: session.session_id,
        }))
        .await;
    let (window, observation) = inspection?;
    close_result?;

    let screenshot = observation.screenshot.as_ref().map(|artifact| {
        json!({
            "mime_type": artifact.mime_type,
            "byte_length": artifact.byte_length,
            "sha256": artifact.sha256,
            "mapping": artifact.mapping,
        })
    });
    let elements: Vec<Value> = observation
        .elements
        .iter()
        .map(|element| {
            json!({
                "role": element.role,
                "name": element.name,
                "value": element.value,
                "enabled": element.enabled,
                "focused": element.focused,
                "actions": element.actions,
                "screen_bounds": element.screen_bounds,
            })
        })
        .collect();
    let report = json!({
        "fixture_contract": "nexus.cua.fixture.v1",
        "capabilities": capabilities,
        "permissions": permissions,
        "discovery": {
            "complete": discovered.complete,
            "application_count": discovered.applications.len(),
            "selected_name": application.name,
            "selected_application_id": application.application_id,
            "selected_provenance": application.provenance,
        },
        "window": window,
        "observation": {
            "captured_at": observation.captured_at,
            "window_screen_bounds": observation.window_screen_bounds,
            "elements_complete": observation.elements_complete,
            "elements_truncation": observation.elements_truncation,
            "screenshot": screenshot,
            "elements": elements,
        },
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

async fn inspect_session(
    cli: &Cli,
    client: &mut Client,
    session_id: &nexus_cua_protocol::SessionId,
    include_screenshot: bool,
) -> Result<(WindowSummary, WindowObservation), Box<dyn Error>> {
    let windows = match client
        .send(Command::ListWindows(ListWindowsInput {
            session_id: session_id.clone(),
            app_ref: None,
        }))
        .await?
    {
        CommandResult::Windows(value) => value,
        other => return Err(unexpected("window list", &other)),
    };
    let window = windows
        .into_iter()
        .find(|window| window.title.starts_with(&cli.window_title_prefix))
        .ok_or_else(|| format!("no window title starts with {:?}", cli.window_title_prefix))?;
    let observation = match client
        .send(Command::ObserveWindow(
            nexus_cua_protocol::ObserveWindowInput {
                session_id: session_id.clone(),
                window_ref: window.window_ref.clone(),
                include_screenshot,
                accessibility: AccessibilityMode::Full,
            },
        ))
        .await?
    {
        CommandResult::WindowObserved(value) => *value,
        other => return Err(unexpected("window observation", &other)),
    };
    Ok((window, observation))
}

fn select_application<'a>(
    discovered: &'a DiscoverApplicationsOutput,
    application_match: &str,
) -> Result<&'a nexus_cua_protocol::DiscoveredApplication, Box<dyn Error>> {
    discovered
        .applications
        .iter()
        .find(|application| {
            application.name.contains(application_match)
                || application.application_id.contains(application_match)
                || serde_json::to_string(&application.provenance)
                    .is_ok_and(|value| value.contains(application_match))
        })
        .ok_or_else(|| {
            let candidates = discovered
                .applications
                .iter()
                .map(|application| {
                    format!("{} ({})", application.name, application.application_id)
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "fixture application containing {application_match:?} was not discovered; candidates: {candidates}"
            )
            .into()
        })
}

fn unexpected(expected: &str, actual: &CommandResult) -> Box<dyn Error> {
    format!("expected {expected}, received {actual:?}").into()
}
