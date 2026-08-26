//! Release-runner benchmark and resource-soak commands.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{Duration, Instant};

use clap::{Args, ValueEnum};
use nexus_cua_protocol::{
    AccessibilityMode, Action, CapabilityManifest, Command, CommandResult, DeliveryMode,
    OpenSessionInput, OpenSessionOutput, PerformActionInput, PermissionMode, SensitiveText,
    SessionId, SessionInput, WindowObservation, WindowSummary,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::{
    Cli, Client, element_screenshot_point, find_actionable, list_windows, select_application,
    select_window, unexpected,
};

const ARTIFACT_BATCH: usize = 24;
const IPC_P95_MS: f64 = 3.0;
const DISCOVERY_P95_MS: f64 = 50.0;
const CAPTURE_P95_MS: f64 = 100.0;
const SEMANTIC_P95_MS: f64 = 120.0;
const COMBINED_P95_MS: f64 = 180.0;
const SEMANTIC_ACTION_P95_MS: f64 = 50.0;
const FOREGROUND_ACTION_P95_MS: f64 = 60.0;
const RSS_GROWTH_NOISE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Args)]
pub(super) struct BenchmarkArgs {
    /// Warm samples omitted from every reported distribution.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=100))]
    warmups: u16,
    /// Reported samples for every operation.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(2..=10_000))]
    samples: u16,
    /// Concrete maintained-runner manifest. Omission makes the run diagnostic.
    #[arg(long)]
    runner_manifest: Option<PathBuf>,
    /// Return failure unless every absolute budget and evidence precondition passes.
    #[arg(long)]
    enforce: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SoakProfile {
    Diagnostic,
    Idle,
    Engineering,
    Release,
}

impl SoakProfile {
    fn minimum_duration(self) -> Duration {
        Duration::from_secs(match self {
            Self::Diagnostic => 1,
            Self::Idle => 5 * 60,
            Self::Engineering => 60 * 60,
            Self::Release => 8 * 60 * 60,
        })
    }

    fn default_duration(self) -> Duration {
        match self {
            Self::Diagnostic => Duration::from_secs(60),
            other => other.minimum_duration(),
        }
    }

    fn is_idle(self) -> bool {
        self == Self::Idle
    }
}

#[derive(Debug, Args)]
pub(super) struct SoakArgs {
    /// Evidence profile; non-diagnostic profiles enforce their normative duration.
    #[arg(long, value_enum, default_value = "diagnostic")]
    profile: SoakProfile,
    /// Override profile duration. Short runs remain diagnostic and fail with --enforce.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    duration_seconds: Option<u64>,
    /// PID of the sidecar process whose resources are sampled.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    service_pid: u32,
    /// Active-workload interval; ignored by the idle profile.
    #[arg(long, default_value_t = 1_000, value_parser = clap::value_parser!(u64).range(10..))]
    operation_interval_ms: u64,
    /// Resource sampling interval.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    resource_interval_seconds: u64,
    /// Concrete maintained-runner manifest. Required for accepted release evidence.
    #[arg(long)]
    runner_manifest: Option<PathBuf>,
    /// Return failure unless duration, runner, resource, and idle-CPU gates pass.
    #[arg(long)]
    enforce: bool,
}

#[derive(Debug, Serialize)]
struct Distribution {
    samples: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

impl Distribution {
    fn from_durations(values: &[Duration]) -> Result<Self, Box<dyn Error>> {
        if values.is_empty() {
            return Err("performance distribution has no samples".into());
        }
        let mut milliseconds = values
            .iter()
            .map(|value| value.as_secs_f64() * 1_000.0)
            .collect::<Vec<_>>();
        milliseconds.sort_by(f64::total_cmp);
        Ok(Self {
            samples: milliseconds.len(),
            p50_ms: percentile(&milliseconds, 0.50),
            p95_ms: percentile(&milliseconds, 0.95),
            max_ms: *milliseconds.last().expect("non-empty distribution"),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
struct ResourceSample {
    elapsed_seconds: f64,
    resident_bytes: u64,
    peak_resident_bytes: Option<u64>,
    cpu_seconds: f64,
    handles: Option<u64>,
    files: Option<u64>,
}

pub(super) async fn benchmark(
    cli: &Cli,
    client: &mut Client,
    args: &BenchmarkArgs,
) -> Result<(), Box<dyn Error>> {
    let capabilities = capabilities(client).await?;
    let total = usize::from(args.warmups) + usize::from(args.samples);
    let warmups = usize::from(args.warmups);

    let mut ipc = Vec::with_capacity(usize::from(args.samples));
    for index in 0..total {
        let started = Instant::now();
        let result = client.send(Command::GetCapabilities).await?;
        if !matches!(result, CommandResult::Capabilities(_)) {
            return Err(unexpected("capabilities", &result));
        }
        record_sample(&mut ipc, index, warmups, started.elapsed());
    }

    let mut discovery = Vec::with_capacity(usize::from(args.samples));
    for index in 0..total {
        let started = Instant::now();
        let result = client.send(Command::DiscoverApplications).await?;
        let elapsed = started.elapsed();
        let discovered = match result {
            CommandResult::ApplicationsDiscovered(value) => value,
            other => return Err(unexpected("application discovery", &other)),
        };
        select_application(&discovered, &cli.application_match)?;
        record_sample(&mut discovery, index, warmups, elapsed);
    }

    let session = open_fixture_session(cli, client, &capabilities, 180).await?;
    let mut window_discovery = Vec::with_capacity(usize::from(args.samples));
    for index in 0..total {
        let started = Instant::now();
        let windows = list_windows(client, &session.session_id).await?;
        let elapsed = started.elapsed();
        select_window(windows, &cli.window_title_prefix)?;
        record_sample(&mut window_discovery, index, warmups, elapsed);
    }
    close_session(client, session.session_id).await?;

    let (capture, capture_shape) = benchmark_observation(
        cli,
        client,
        &capabilities,
        total,
        warmups,
        true,
        AccessibilityMode::Disabled,
    )
    .await?;
    let (semantics, semantic_shape) = benchmark_observation(
        cli,
        client,
        &capabilities,
        total,
        warmups,
        false,
        AccessibilityMode::Interactive,
    )
    .await?;
    let (combined, combined_shape) = benchmark_observation(
        cli,
        client,
        &capabilities,
        total,
        warmups,
        true,
        AccessibilityMode::Interactive,
    )
    .await?;
    let semantic_action =
        benchmark_semantic_action(cli, client, &capabilities, total, warmups).await?;
    let foreground_action =
        benchmark_foreground_action(cli, client, &capabilities, total, warmups).await?;

    let distributions = json!({
        "ipc_dispatch": Distribution::from_durations(&ipc)?,
        "application_discovery": Distribution::from_durations(&discovery)?,
        "window_discovery": Distribution::from_durations(&window_discovery)?,
        "window_capture": Distribution::from_durations(&capture)?,
        "interactive_semantics": Distribution::from_durations(&semantics)?,
        "combined_observation": Distribution::from_durations(&combined)?,
        "semantic_action": Distribution::from_durations(&semantic_action)?,
        "foreground_action": Distribution::from_durations(&foreground_action)?,
    });
    let capture_budget_applies = capture_shape
        .as_ref()
        .is_some_and(|shape| shape[0] >= 1_920 && shape[1] >= 1_080);
    let semantic_shape = semantic_shape.ok_or("semantic benchmark did not return a shape")?;
    let semantic_budget_applies = semantic_shape[2] <= 1_000;
    let runner = load_runner_manifest(
        args.runner_manifest.as_deref(),
        &serde_json::to_value(capabilities.platform)?,
    )?;
    let gates = json!({
        "ipc_dispatch": gate(&distributions["ipc_dispatch"], IPC_P95_MS, true),
        "application_discovery": gate(&distributions["application_discovery"], DISCOVERY_P95_MS, true),
        "window_discovery": gate(&distributions["window_discovery"], DISCOVERY_P95_MS, true),
        "window_capture": gate(&distributions["window_capture"], CAPTURE_P95_MS, capture_budget_applies),
        "interactive_semantics": gate(&distributions["interactive_semantics"], SEMANTIC_P95_MS, semantic_budget_applies),
        "combined_observation": gate(&distributions["combined_observation"], COMBINED_P95_MS, capture_budget_applies),
        "semantic_action": gate(&distributions["semantic_action"], SEMANTIC_ACTION_P95_MS, true),
        "foreground_action": gate(&distributions["foreground_action"], FOREGROUND_ACTION_P95_MS, true),
    });
    let absolute_pass = gates
        .as_object()
        .expect("gate map")
        .values()
        .all(|value| !value["applies"].as_bool().unwrap_or(false) || value["passed"] == true);
    let evidence_accepted = absolute_pass && capture_budget_applies && runner.is_some();
    let report = json!({
        "fixture_contract": "nexus.cua.fixture.v1",
        "evidence_kind": "native_performance",
        "classification": if evidence_accepted { "accepted" } else { "diagnostic" },
        "platform": capabilities.platform,
        "architecture": std::env::consts::ARCH,
        "runtime_version": capabilities.runtime_version,
        "runner": runner,
        "warmups_per_operation": args.warmups,
        "semantic_action_kind": "set_value",
        "foreground_action_kind": "move_pointer",
        "distributions": distributions,
        "observed_shapes": {
            "capture": capture_shape,
            "interactive_semantics": {
                "screen_width": semantic_shape[0],
                "screen_height": semantic_shape[1],
                "element_count": semantic_shape[2],
            },
            "combined": combined_shape,
        },
        "gates": gates,
        "absolute_budgets_passed": absolute_pass,
        "accepted_release_evidence": evidence_accepted,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    if args.enforce && !evidence_accepted {
        return Err("benchmark did not satisfy every release-evidence precondition".into());
    }
    Ok(())
}

pub(super) async fn soak(
    cli: &Cli,
    client: &mut Client,
    args: &SoakArgs,
) -> Result<(), Box<dyn Error>> {
    let duration = args
        .duration_seconds
        .map(Duration::from_secs)
        .unwrap_or_else(|| args.profile.default_duration());
    let duration_qualified = duration >= args.profile.minimum_duration();
    let capabilities = capabilities(client).await?;
    let runner = load_runner_manifest(
        args.runner_manifest.as_deref(),
        &serde_json::to_value(capabilities.platform)?,
    )?;
    let runner_qualified = runner.is_some();
    if !args.profile.is_idle() {
        warm_capture(cli, client, &capabilities).await?;
    }
    // Capture initialization and the first encoder allocation are warm-up
    // costs. The 4K budget measures incremental steady-state capture memory.
    let memory_baseline = sample_process(args.service_pid)?;
    let started = Instant::now();
    let deadline = started + duration;
    let resource_interval = Duration::from_secs(args.resource_interval_seconds);
    let operation_interval = Duration::from_millis(args.operation_interval_ms);
    let mut next_resource = started;
    let mut next_operation = started;
    let mut resource_samples = Vec::new();
    let mut operations = 0_u64;
    let mut max_pixel_width = 0_u32;
    let mut max_pixel_height = 0_u32;
    let mut session: Option<(OpenSessionOutput, WindowSummary, usize)> = None;
    while Instant::now() < deadline {
        let now = Instant::now();
        if now >= next_resource {
            let mut sample = sample_process(args.service_pid)?;
            sample.elapsed_seconds = started.elapsed().as_secs_f64();
            resource_samples.push(sample);
            next_resource += resource_interval;
        }
        if !args.profile.is_idle() && now >= next_operation {
            if session
                .as_ref()
                .is_none_or(|(_, _, artifacts)| *artifacts >= ARTIFACT_BATCH)
            {
                if let Some((previous, _, _)) = session.take() {
                    close_session(client, previous.session_id).await?;
                }
                let opened = open_fixture_session(cli, client, &capabilities, 180).await?;
                let window = select_window(
                    list_windows(client, &opened.session_id).await?,
                    &cli.window_title_prefix,
                )?;
                session = Some((opened, window, 0));
            }
            let (opened, window, artifacts) = session.as_mut().expect("active soak session");
            let observation = observe_command(
                client,
                &opened.session_id,
                window,
                true,
                AccessibilityMode::Interactive,
            )
            .await?;
            let screenshot = observation
                .screenshot
                .as_ref()
                .ok_or("active soak observation omitted its screenshot")?;
            max_pixel_width = max_pixel_width.max(screenshot.mapping.pixel_size.width);
            max_pixel_height = max_pixel_height.max(screenshot.mapping.pixel_size.height);
            *artifacts += 1;
            operations += 1;
            next_operation += operation_interval;
        }
        let next = if args.profile.is_idle() {
            next_resource.min(deadline)
        } else {
            next_resource.min(next_operation).min(deadline)
        };
        tokio::time::sleep(next.saturating_duration_since(Instant::now())).await;
    }
    if let Some((opened, _, _)) = session {
        close_session(client, opened.session_id).await?;
    }
    let mut final_sample = sample_process(args.service_pid)?;
    final_sample.elapsed_seconds = started.elapsed().as_secs_f64();
    resource_samples.push(final_sample);

    let resource_gate = resource_growth_report(&resource_samples);
    let cpu_percent = cpu_percent(&resource_samples);
    let idle_cpu_applies = args.profile.is_idle();
    let idle_cpu_pass = !idle_cpu_applies || cpu_percent < 0.5;
    let resources_pass = resource_gate["monotonic_growth_detected"] == false;
    let peak_resident_bytes = resource_samples
        .iter()
        .map(|sample| sample.peak_resident_bytes.unwrap_or(sample.resident_bytes))
        .max()
        .unwrap_or(memory_baseline.resident_bytes);
    let peak_resident_delta = peak_resident_bytes.saturating_sub(memory_baseline.resident_bytes);
    let four_k_applies =
        !args.profile.is_idle() && max_pixel_width >= 3_840 && max_pixel_height >= 2_160;
    let memory_pass = four_k_applies && peak_resident_delta < 128 * 1024 * 1024;
    let memory_qualified = args.profile.is_idle() || memory_pass;
    let accepted = duration_qualified
        && runner_qualified
        && resources_pass
        && idle_cpu_pass
        && memory_qualified
        && args.profile != SoakProfile::Diagnostic;
    let report = json!({
        "fixture_contract": "nexus.cua.fixture.v1",
        "evidence_kind": "native_resource_soak",
        "classification": if accepted { "accepted" } else { "diagnostic" },
        "profile": format!("{:?}", args.profile).to_lowercase(),
        "platform": capabilities.platform,
        "architecture": std::env::consts::ARCH,
        "runtime_version": capabilities.runtime_version,
        "resident_memory_metric": if cfg!(target_os = "macos") {
            "physical_footprint"
        } else if cfg!(windows) {
            "working_set"
        } else {
            "resident_set_size"
        },
        "runner": runner,
        "service_pid": args.service_pid,
        "required_duration_seconds": args.profile.minimum_duration().as_secs(),
        "actual_duration_seconds": started.elapsed().as_secs_f64(),
        "duration_qualified": duration_qualified,
        "operation_count": operations,
        "max_screenshot_pixel_size": if args.profile.is_idle() {
            Value::Null
        } else {
            json!({"width": max_pixel_width, "height": max_pixel_height})
        },
        "resource_sample_count": resource_samples.len(),
        "resource_samples": resource_samples,
        "resource_gate": resource_gate,
        "capture_memory_gate": {
            "applies": four_k_applies,
            "budget_peak_delta_bytes": 128 * 1024 * 1024_u64,
            "baseline_resident_bytes": memory_baseline.resident_bytes,
            "peak_resident_bytes": peak_resident_bytes,
            "peak_delta_bytes": peak_resident_delta,
            "passed": four_k_applies && memory_pass,
        },
        "idle_cpu_gate": {
            "applies": idle_cpu_applies,
            "budget_percent": 0.5,
            "observed_percent": cpu_percent,
            "passed": idle_cpu_applies && idle_cpu_pass,
        },
        "accepted_release_evidence": accepted,
        "unavailable_resource_counts": ["com", "iosurface", "gpu"],
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    if args.enforce && !accepted {
        return Err("soak did not satisfy every selected evidence gate".into());
    }
    Ok(())
}

async fn capabilities(
    client: &mut Client,
) -> Result<nexus_cua_protocol::DriverCapabilities, Box<dyn Error>> {
    match client.send(Command::GetCapabilities).await? {
        CommandResult::Capabilities(value) => Ok(value),
        other => Err(unexpected("capabilities", &other)),
    }
}

async fn open_fixture_session(
    cli: &Cli,
    client: &mut Client,
    capabilities: &nexus_cua_protocol::DriverCapabilities,
    ttl_seconds: u32,
) -> Result<OpenSessionOutput, Box<dyn Error>> {
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("application discovery", &other)),
    };
    let application = select_application(&discovered, &cli.application_match)?;
    match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::Bounded,
                application_refs: vec![application.discovery_ref.clone()],
                allowed_actions: capabilities.actions.clone(),
                allow_foreground_input: true,
                ttl_seconds,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => Ok(value),
        other => Err(unexpected("open performance session", &other)),
    }
}

async fn close_session(client: &mut Client, session_id: SessionId) -> Result<(), Box<dyn Error>> {
    match client
        .send(Command::CloseSession(SessionInput { session_id }))
        .await?
    {
        CommandResult::Acknowledged => Ok(()),
        other => Err(unexpected("close performance session", &other)),
    }
}

async fn benchmark_observation(
    cli: &Cli,
    client: &mut Client,
    capabilities: &nexus_cua_protocol::DriverCapabilities,
    total: usize,
    warmups: usize,
    include_screenshot: bool,
    accessibility: AccessibilityMode,
) -> Result<(Vec<Duration>, Option<[u32; 3]>), Box<dyn Error>> {
    let mut durations = Vec::with_capacity(total.saturating_sub(warmups));
    let mut shape = None;
    let mut completed = 0;
    while completed < total {
        let opened = open_fixture_session(cli, client, capabilities, 180).await?;
        let window = select_window(
            list_windows(client, &opened.session_id).await?,
            &cli.window_title_prefix,
        )?;
        let batch = if include_screenshot {
            ARTIFACT_BATCH.min(total - completed)
        } else {
            total - completed
        };
        for _ in 0..batch {
            let started = Instant::now();
            let observation = observe_command(
                client,
                &opened.session_id,
                &window,
                include_screenshot,
                accessibility,
            )
            .await?;
            let elapsed = started.elapsed();
            if completed >= warmups {
                durations.push(elapsed);
            }
            shape = Some(if let Some(screenshot) = &observation.screenshot {
                [
                    screenshot.mapping.pixel_size.width,
                    screenshot.mapping.pixel_size.height,
                    u32::try_from(observation.elements.len()).unwrap_or(u32::MAX),
                ]
            } else {
                [
                    observation.window_screen_bounds.width.max(0.0) as u32,
                    observation.window_screen_bounds.height.max(0.0) as u32,
                    u32::try_from(observation.elements.len()).unwrap_or(u32::MAX),
                ]
            });
            completed += 1;
        }
        close_session(client, opened.session_id).await?;
    }
    Ok((durations, shape))
}

async fn benchmark_semantic_action(
    cli: &Cli,
    client: &mut Client,
    capabilities: &nexus_cua_protocol::DriverCapabilities,
    total: usize,
    warmups: usize,
) -> Result<Vec<Duration>, Box<dyn Error>> {
    let opened = open_fixture_session(cli, client, capabilities, 180).await?;
    let window = select_window(
        list_windows(client, &opened.session_id).await?,
        &cli.window_title_prefix,
    )?;
    let mut durations = Vec::with_capacity(total.saturating_sub(warmups));
    for index in 0..total {
        let observation = observe_command(
            client,
            &opened.session_id,
            &window,
            false,
            AccessibilityMode::Interactive,
        )
        .await?;
        let text = find_actionable(&observation, "Fixture Text", "set_value")?;
        let started = Instant::now();
        perform_command(
            client,
            &opened.session_id,
            &window,
            &observation,
            Action::SetValue {
                element_ref: text.element_ref.clone(),
                value: SensitiveText::new(if index % 2 == 0 {
                    "benchmark-even"
                } else {
                    "benchmark-odd"
                }),
            },
            DeliveryMode::Semantic,
        )
        .await?;
        record_sample(&mut durations, index, warmups, started.elapsed());
    }
    close_session(client, opened.session_id).await?;
    Ok(durations)
}

async fn benchmark_foreground_action(
    cli: &Cli,
    client: &mut Client,
    capabilities: &nexus_cua_protocol::DriverCapabilities,
    total: usize,
    warmups: usize,
) -> Result<Vec<Duration>, Box<dyn Error>> {
    let mut durations = Vec::with_capacity(total.saturating_sub(warmups));
    let mut completed = 0;
    while completed < total {
        let opened = open_fixture_session(cli, client, capabilities, 180).await?;
        let window = select_window(
            list_windows(client, &opened.session_id).await?,
            &cli.window_title_prefix,
        )?;
        let batch = ARTIFACT_BATCH.min(total - completed);
        for _ in 0..batch {
            let observation = observe_command(
                client,
                &opened.session_id,
                &window,
                true,
                AccessibilityMode::Interactive,
            )
            .await?;
            let point = element_screenshot_point(&observation, "Increment Counter")?;
            let started = Instant::now();
            perform_command(
                client,
                &opened.session_id,
                &window,
                &observation,
                Action::MovePointer {
                    point,
                    duration_ms: 0,
                },
                DeliveryMode::Foreground,
            )
            .await?;
            record_sample(&mut durations, completed, warmups, started.elapsed());
            completed += 1;
        }
        close_session(client, opened.session_id).await?;
    }
    Ok(durations)
}

async fn warm_capture(
    cli: &Cli,
    client: &mut Client,
    capabilities: &nexus_cua_protocol::DriverCapabilities,
) -> Result<(), Box<dyn Error>> {
    let opened = open_fixture_session(cli, client, capabilities, 180).await?;
    let window = select_window(
        list_windows(client, &opened.session_id).await?,
        &cli.window_title_prefix,
    )?;
    for _ in 0..3 {
        observe_command(
            client,
            &opened.session_id,
            &window,
            true,
            AccessibilityMode::Interactive,
        )
        .await?;
    }
    close_session(client, opened.session_id).await
}

async fn observe_command(
    client: &mut Client,
    session_id: &SessionId,
    window: &WindowSummary,
    include_screenshot: bool,
    accessibility: AccessibilityMode,
) -> Result<WindowObservation, Box<dyn Error>> {
    match client
        .send(Command::ObserveWindow(
            nexus_cua_protocol::ObserveWindowInput {
                session_id: session_id.clone(),
                window_ref: window.window_ref.clone(),
                include_screenshot,
                accessibility,
            },
        ))
        .await?
    {
        CommandResult::WindowObserved(value) => Ok(*value),
        other => Err(unexpected("performance observation", &other)),
    }
}

async fn perform_command(
    client: &mut Client,
    session_id: &SessionId,
    window: &WindowSummary,
    observation: &WindowObservation,
    action: Action,
    delivery_mode: DeliveryMode,
) -> Result<(), Box<dyn Error>> {
    let expected_kind = action.kind();
    match client
        .send(Command::PerformAction(PerformActionInput {
            session_id: session_id.clone(),
            window_ref: window.window_ref.clone(),
            observation_id: observation.observation_id.clone(),
            action,
        }))
        .await?
    {
        CommandResult::ActionPerformed(output)
            if output.delivery_mode == delivery_mode
                && output.dispatched
                && output.observation_invalidated =>
        {
            Ok(())
        }
        CommandResult::ActionPerformed(output) => Err(format!(
            "{expected_kind:?} returned delivery={:?}, dispatched={}, invalidated={}",
            output.delivery_mode, output.dispatched, output.observation_invalidated
        )
        .into()),
        other => Err(unexpected("performance action", &other)),
    }
}

fn record_sample(output: &mut Vec<Duration>, index: usize, warmups: usize, elapsed: Duration) {
    if index >= warmups {
        output.push(elapsed);
    }
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

fn gate(distribution: &Value, budget_ms: f64, applies: bool) -> Value {
    let p95 = distribution["p95_ms"].as_f64().unwrap_or(f64::INFINITY);
    json!({
        "applies": applies,
        "budget_p95_ms": budget_ms,
        "observed_p95_ms": p95,
        "passed": applies && p95 <= budget_ms,
    })
}

fn load_runner_manifest(
    path: Option<&Path>,
    expected_platform: &Value,
) -> Result<Option<Value>, Box<dyn Error>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    for field in [
        "runner_id",
        "status",
        "platform",
        "architecture",
        "cpu",
        "memory_bytes",
        "gpu",
        "display_topology",
        "power_mode",
        "os_build",
        "toolchain",
    ] {
        if value.get(field).is_none_or(Value::is_null) {
            return Err(format!("runner manifest is missing non-null field {field:?}").into());
        }
    }
    if value["status"] != "active" {
        return Err("runner manifest status must be active".into());
    }
    if &value["platform"] != expected_platform {
        return Err("runner manifest platform does not match the active driver".into());
    }
    if value["architecture"] != std::env::consts::ARCH {
        return Err("runner manifest architecture does not match this executable".into());
    }
    Ok(Some(value))
}

fn sample_process(pid: u32) -> Result<ResourceSample, Box<dyn Error>> {
    #[cfg(windows)]
    {
        let script = format!(
            "$p=Get-Process -Id {pid} -ErrorAction Stop; Write-Output ($p.WorkingSet64.ToString()+' '+$p.PeakWorkingSet64.ToString()+' '+$p.TotalProcessorTime.TotalSeconds.ToString([Globalization.CultureInfo]::InvariantCulture)+' '+$p.HandleCount.ToString())"
        );
        let output = ProcessCommand::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()?;
        if !output.status.success() {
            return Err("failed to sample Windows sidecar process".into());
        }
        let fields = String::from_utf8(output.stdout)?;
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4 {
            return Err("Windows process sample has an unexpected shape".into());
        }
        return Ok(ResourceSample {
            elapsed_seconds: 0.0,
            resident_bytes: fields[0].parse()?,
            peak_resident_bytes: Some(fields[1].parse()?),
            cpu_seconds: fields[2].parse()?,
            handles: Some(fields[3].parse()?),
            files: None,
        });
    }
    #[cfg(not(windows))]
    {
        let output = ProcessCommand::new("ps")
            .args(["-p", &pid.to_string(), "-o", "rss=", "-o", "time="])
            .output()?;
        if !output.status.success() {
            return Err("failed to sample sidecar process with ps".into());
        }
        let fields = String::from_utf8(output.stdout)?;
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 {
            return Err("process sample has an unexpected shape".into());
        }
        let files = if cfg!(target_os = "macos") {
            macos_file_count(pid).ok()
        } else {
            None
        };
        #[cfg(target_os = "macos")]
        let (resident_bytes, peak_resident_bytes) = macos_physical_footprint(pid)?;
        #[cfg(not(target_os = "macos"))]
        let resident_bytes = fields[0].parse::<u64>()?.saturating_mul(1_024);
        #[cfg(not(target_os = "macos"))]
        let peak_resident_bytes = None;
        Ok(ResourceSample {
            elapsed_seconds: 0.0,
            resident_bytes,
            peak_resident_bytes,
            cpu_seconds: parse_cpu_time(fields[1])?,
            handles: None,
            files,
        })
    }
}

#[cfg(target_os = "macos")]
fn macos_physical_footprint(pid: u32) -> Result<(u64, Option<u64>), Box<dyn Error>> {
    let pid = i32::try_from(pid).map_err(|_| "macOS process id exceeds libproc limits")?;
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: libproc writes one rusage_info_v4 into the correctly sized,
    // initialized output buffer for the supplied live process identifier.
    let result = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            usage.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if result != 0 {
        return Err("failed to sample macOS physical footprint with libproc".into());
    }
    // SAFETY: A successful proc_pid_rusage call initialized the full structure.
    let usage = unsafe { usage.assume_init() };
    Ok((
        usage.ri_phys_footprint,
        Some(usage.ri_lifetime_max_phys_footprint),
    ))
}

#[cfg(not(windows))]
fn parse_cpu_time(value: &str) -> Result<f64, Box<dyn Error>> {
    let fields = value.split(':').collect::<Vec<_>>();
    let seconds = match fields.as_slice() {
        [minutes, seconds] => minutes.parse::<f64>()? * 60.0 + seconds.parse::<f64>()?,
        [hours, minutes, seconds] => {
            hours.parse::<f64>()? * 3_600.0
                + minutes.parse::<f64>()? * 60.0
                + seconds.parse::<f64>()?
        }
        _ => return Err("process CPU time has an unexpected shape".into()),
    };
    Ok(seconds)
}

#[cfg(not(windows))]
fn macos_file_count(pid: u32) -> Result<u64, Box<dyn Error>> {
    let output = ProcessCommand::new("/usr/sbin/lsof")
        .args(["-n", "-P", "-p", &pid.to_string()])
        .output()?;
    if !output.status.success() {
        return Err("lsof failed while sampling sidecar files".into());
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .count()
        .saturating_sub(1) as u64)
}

fn cpu_percent(samples: &[ResourceSample]) -> f64 {
    let Some((first, rest)) = samples.split_first() else {
        return f64::INFINITY;
    };
    let Some(last) = rest.last() else {
        return f64::INFINITY;
    };
    let wall = last.elapsed_seconds - first.elapsed_seconds;
    if wall <= 0.0 {
        return f64::INFINITY;
    }
    ((last.cpu_seconds - first.cpu_seconds).max(0.0) / wall) * 100.0
}

fn resource_growth_report(samples: &[ResourceSample]) -> Value {
    let resident = samples
        .iter()
        .map(|value| value.resident_bytes)
        .collect::<Vec<_>>();
    let handles = samples
        .iter()
        .filter_map(|value| value.handles)
        .collect::<Vec<_>>();
    let files = samples
        .iter()
        .filter_map(|value| value.files)
        .collect::<Vec<_>>();
    let resident_growth = monotonic_growth(&resident, RSS_GROWTH_NOISE_BYTES);
    let handle_growth = monotonic_growth(&handles, 0);
    let file_growth = monotonic_growth(&files, 0);
    json!({
        "monotonic_growth_detected": resident_growth || handle_growth || file_growth,
        "resident_bytes": series_summary(&resident, resident_growth, RSS_GROWTH_NOISE_BYTES),
        "handles": series_summary(&handles, handle_growth, 0),
        "files": series_summary(&files, file_growth, 0),
    })
}

fn monotonic_growth(values: &[u64], minimum_net_growth: u64) -> bool {
    values.len() >= 4
        && values
            .last()
            .zip(values.first())
            .is_some_and(|(last, first)| last.saturating_sub(*first) > minimum_net_growth)
        && values.windows(2).all(|pair| pair[1] >= pair[0])
}

fn series_summary(values: &[u64], growth: bool, minimum_net_growth: u64) -> Value {
    if values.is_empty() {
        return json!({"available": false});
    }
    json!({
        "available": true,
        "first": values.first(),
        "last": values.last(),
        "min": values.iter().min(),
        "max": values.iter().max(),
        "minimum_net_growth": minimum_net_growth,
        "monotonic_growth": growth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_growth_ignores_vm_page_noise_but_detects_sustained_growth() {
        assert!(!monotonic_growth(&[100, 120, 140, 160], 64));
        assert!(monotonic_growth(&[100, 130, 160, 190], 64));
        assert!(!monotonic_growth(&[100, 200, 150, 300], 64));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_physical_footprint_is_available_for_this_process() {
        let (current, peak) = macos_physical_footprint(std::process::id()).unwrap();
        assert!(current > 0);
        assert!(peak.is_some_and(|peak| peak >= current));
    }
}
