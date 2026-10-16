//! Fail-closed Codex MCP integration planning and application.

use std::ffi::OsString;
use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use clap::{Args, Subcommand, ValueEnum};
use nexus_cua_mcp::{AuthorityPolicy, ToolProfile, current_unix_ms};
use nexus_cua_platform::{HostExecutableIdentity, inspect_host_executable};
use nexus_cua_provider::{
    AuthenticatedIntegrationStore, McpRegisteredTransport, RegisteredAdapterProfile,
    RegistrationRequest, RegistrationStore, default_state_root, default_user_home,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tokio::time::timeout;
use uuid::Uuid;
use zeroize::Zeroizing;

const PLAN_SCHEMA_VERSION: u32 = 1;
const RECEIPT_SCHEMA_VERSION: u32 = 1;
const INSPECTION_SCHEMA_VERSION: u32 = 1;
const PLAN_TTL_MS: u64 = 10 * 60 * 1_000;
const MAX_HOST_OUTPUT_BYTES: u64 = 256 * 1024;
const MAX_SKILL_BYTES: u64 = 24 * 1024;
const HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const MCP_ALIAS: &str = "nexuscua";
const SKILL_NAME: &str = "nexuscua-computer-use";
const PROVIDER_ID: &str = "nexuscua.local.v1";
const EMBEDDED_MCP_SKILL: &[u8] = include_bytes!("../assets/mcp-skill.md");

/// Host integration command group.
#[derive(Debug, Args)]
pub(crate) struct IntegrationArgs {
    /// Integration operation.
    #[command(subcommand)]
    pub(crate) command: IntegrationCommand,
}

/// Closed host integration operations.
#[derive(Debug, Subcommand)]
pub(crate) enum IntegrationCommand {
    /// Inspect only the known Codex MCP alias and generated Skill location.
    Inspect(IntegrationInspectArgs),
    /// Create a short-lived authenticated change plan without mutating Codex.
    Plan(IntegrationPlanArgs),
    /// Apply one fresh plan after a real terminal confirmation.
    Apply(IntegrationApplyArgs),
    /// Revoke and remove only an unchanged integration owned by this tool.
    Remove(IntegrationRemoveArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum IntegrationHostKind {
    /// Local Codex client using its supported `codex mcp` command.
    Codex,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum IntegrationProfile {
    /// MCP binding for Codex and compatible model hosts.
    Mcp,
}

/// Read-only integration inspection input.
#[derive(Debug, Args)]
pub(crate) struct IntegrationInspectArgs {
    /// Host to inspect. Only the exact `nexuscua` alias is reported.
    #[arg(long, value_enum, default_value_t = IntegrationHostKind::Codex)]
    host: IntegrationHostKind,
    /// Exact Codex executable; PATH resolution is used when omitted.
    #[arg(long)]
    codex_executable: Option<PathBuf>,
    /// Optional non-secret registration identity to authenticate and inspect.
    #[arg(long)]
    registration: Option<String>,
    /// Optional exact Codex global Skill root; the documented default is used when omitted.
    #[arg(long)]
    skill_root: Option<PathBuf>,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

/// Integration plan input.
#[derive(Debug, Args)]
pub(crate) struct IntegrationPlanArgs {
    /// Host to configure.
    #[arg(long, value_enum)]
    host: IntegrationHostKind,
    /// Installation profile. Codex supports only the MCP binding.
    #[arg(long, value_enum)]
    profile: IntegrationProfile,
    /// Exact Codex executable; PATH resolution is used when omitted.
    #[arg(long)]
    codex_executable: Option<PathBuf>,
    /// Owner-private, human-authored authority ceiling consumed only by planning.
    #[arg(long)]
    authority_policy_file: PathBuf,
    /// Authenticated local principal label shown in the plan.
    #[arg(long)]
    principal: String,
    /// Bounded work scope shown in the plan and bound into every grant.
    #[arg(long)]
    work_scope: String,
    /// MCP schema profile exposed to the host.
    #[arg(long, value_enum, default_value_t = ToolProfile::Full)]
    tool_profile: ToolProfile,
    /// Optional exact Codex global Skill root; the documented default is used when omitted.
    #[arg(long)]
    skill_root: Option<PathBuf>,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

/// Apply input. Confirmation is accepted only from a real terminal.
#[derive(Debug, Args)]
pub(crate) struct IntegrationApplyArgs {
    /// Opaque plan identity returned by `integration plan`.
    #[arg(long)]
    plan: String,
    /// Confirm the exact visible plan. Non-terminal invocations are rejected.
    #[arg(long)]
    confirm: bool,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

/// Removal input. Current host state must still match the installation receipt.
#[derive(Debug, Args)]
pub(crate) struct IntegrationRemoveArgs {
    /// Non-secret registration identity returned by apply.
    #[arg(long)]
    registration: String,
    /// Confirm revocation and removal from a real terminal.
    #[arg(long)]
    confirm: bool,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Error)]
pub(crate) enum IntegrationError {
    #[error("integration input is invalid: {0}")]
    Invalid(String),
    #[error("Codex executable was not found")]
    HostNotFound,
    #[error("Codex MCP command failed")]
    HostCommand,
    #[error("Codex MCP output is invalid or exceeds its bound")]
    HostOutput,
    #[error("integration plan is stale, consumed, or expired")]
    StalePlan,
    #[error("the `nexuscua` MCP alias is already configured")]
    AliasConflict,
    #[error("the generated `nexuscua-computer-use` Skill target is already occupied")]
    SkillConflict,
    #[error("human confirmation from a real terminal is required")]
    ConfirmationRequired,
    #[error("integration state no longer matches the authenticated plan or receipt")]
    StateDrift,
    #[error("integration failed and rollback could not remove every owned artifact")]
    RollbackIncomplete,
    #[error("integration I/O failed")]
    Io(#[from] std::io::Error),
    #[error("integration JSON failed")]
    Json(#[from] serde_json::Error),
    #[error("registered authority state failed")]
    Provider(#[from] nexus_cua_provider::RegistrationError),
    #[error("authority policy is invalid")]
    Authority(#[from] nexus_cua_mcp::AuthorityError),
    #[error("host executable identity could not be established")]
    HostIdentity(#[from] nexus_cua_platform::HostIdentityError),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IntegrationPlan {
    schema_version: u32,
    plan_ref: String,
    registration_id: String,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    host: String,
    profile: String,
    alias: String,
    product_version: String,
    transport: McpRegisteredTransport,
    tool_profile: String,
    host_executable: PathBuf,
    host_identity: HostExecutableIdentity,
    runtime_executable: PathBuf,
    runtime_executable_sha256: String,
    skill_file: PathBuf,
    skill_sha256: String,
    host_precondition_sha256: String,
    registration: RegistrationRequest,
    changes: Vec<PlannedChange>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PlannedChange {
    operation: String,
    target: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IntegrationReceipt {
    schema_version: u32,
    registration_id: String,
    installed_at_unix_ms: u64,
    host: String,
    profile: String,
    alias: String,
    product_version: String,
    transport: McpRegisteredTransport,
    tool_profile: String,
    host_executable: PathBuf,
    host_identity: HostExecutableIdentity,
    runtime_executable: PathBuf,
    runtime_executable_sha256: String,
    skill_file: PathBuf,
    skill_sha256: String,
    alias_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct IntegrationInspection {
    schema_version: u32,
    product_version: &'static str,
    host: &'static str,
    host_available: bool,
    host_identity: Option<HostExecutableIdentity>,
    alias: &'static str,
    alias_state: &'static str,
    skill_state: &'static str,
    registration_state: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct IntegrationApplied {
    schema_version: u32,
    status: &'static str,
    registration_id: String,
    host: String,
    profile: String,
    alias: String,
    transport: McpRegisteredTransport,
    tool_profile: String,
    skill_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct IntegrationRemoved {
    schema_version: u32,
    status: &'static str,
    registration_id: String,
    alias: String,
}

#[derive(Clone, Debug)]
struct HostSnapshot {
    configuration_sha256: String,
    alias: AliasSnapshot,
}

#[derive(Clone, Debug)]
struct AliasSnapshot {
    present: bool,
    sha256: String,
    plain_stdio_command: Option<String>,
    plain_stdio_args: Option<Vec<String>>,
}

impl AliasSnapshot {
    fn is_owned(&self, runtime: &Path, registration_id: &str) -> bool {
        let Some(runtime) = runtime.to_str() else {
            return false;
        };
        let expected = ["mcp", "launch", "--registration", registration_id].map(str::to_owned);
        self.present
            && self.plain_stdio_command.as_deref() == Some(runtime)
            && self.plain_stdio_args.as_deref() == Some(expected.as_slice())
    }
}

#[async_trait]
trait McpHost: Send + Sync {
    fn executable(&self) -> &Path;
    async fn inspect(&self) -> Result<HostSnapshot, IntegrationError>;
    async fn add(&self, runtime: &Path, registration_id: &str) -> Result<(), IntegrationError>;
    async fn remove(&self) -> Result<(), IntegrationError>;
}

trait ExecutableInspector: Send + Sync {
    fn inspect(&self, path: &Path) -> Result<HostExecutableIdentity, IntegrationError>;
}

struct SystemExecutableInspector;

impl ExecutableInspector for SystemExecutableInspector {
    fn inspect(&self, path: &Path) -> Result<HostExecutableIdentity, IntegrationError> {
        inspect_host_executable(path).map_err(Into::into)
    }
}

struct CodexMcpHost {
    executable: PathBuf,
}

impl CodexMcpHost {
    fn discover(explicit: Option<&Path>) -> Result<Self, IntegrationError> {
        let executable = match explicit {
            Some(path) => path.to_path_buf(),
            None => resolve_program("codex").ok_or(IntegrationError::HostNotFound)?,
        };
        let executable = std::fs::canonicalize(executable)?;
        if !std::fs::symlink_metadata(&executable)?.is_file() {
            return Err(IntegrationError::HostNotFound);
        }
        Ok(Self { executable })
    }

    async fn invoke(&self, arguments: &[&str]) -> Result<Zeroizing<Vec<u8>>, IntegrationError> {
        let mut child = Command::new(&self.executable)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().ok_or(IntegrationError::HostOutput)?;
        let mut bytes = Zeroizing::new(Vec::new());
        timeout(
            HOST_COMMAND_TIMEOUT,
            stdout
                .take(MAX_HOST_OUTPUT_BYTES.saturating_add(1))
                .read_to_end(&mut bytes),
        )
        .await
        .map_err(|_| IntegrationError::HostCommand)??;
        if bytes.len() as u64 > MAX_HOST_OUTPUT_BYTES {
            let _ = child.kill().await;
            return Err(IntegrationError::HostOutput);
        }
        let status = timeout(HOST_COMMAND_TIMEOUT, child.wait())
            .await
            .map_err(|_| IntegrationError::HostCommand)??;
        if status.success() {
            Ok(bytes)
        } else {
            Err(IntegrationError::HostCommand)
        }
    }
}

#[async_trait]
impl McpHost for CodexMcpHost {
    fn executable(&self) -> &Path {
        &self.executable
    }

    async fn inspect(&self) -> Result<HostSnapshot, IntegrationError> {
        let bytes = self.invoke(&["mcp", "list", "--json"]).await?;
        parse_codex_snapshot(&bytes)
    }

    async fn add(&self, runtime: &Path, registration_id: &str) -> Result<(), IntegrationError> {
        let runtime = runtime.to_str().ok_or_else(|| {
            IntegrationError::Invalid("runtime path is not valid UTF-8".to_owned())
        })?;
        self.invoke(&[
            "mcp",
            "add",
            MCP_ALIAS,
            "--",
            runtime,
            "mcp",
            "launch",
            "--registration",
            registration_id,
        ])
        .await
        .map(|_| ())
    }

    async fn remove(&self) -> Result<(), IntegrationError> {
        self.invoke(&["mcp", "remove", MCP_ALIAS]).await.map(|_| ())
    }
}

pub(crate) async fn run(args: IntegrationArgs) -> Result<(), IntegrationError> {
    match args.command {
        IntegrationCommand::Inspect(args) => inspect(args).await,
        IntegrationCommand::Plan(args) => plan(args).await,
        IntegrationCommand::Apply(args) => apply(args).await,
        IntegrationCommand::Remove(args) => remove(args).await,
    }
}

async fn inspect(args: IntegrationInspectArgs) -> Result<(), IntegrationError> {
    let IntegrationHostKind::Codex = args.host;
    let host = CodexMcpHost::discover(args.codex_executable.as_deref());
    let (host_available, host_identity, alias_state) = match host {
        Ok(host) => {
            let identity = SystemExecutableInspector.inspect(host.executable())?;
            let snapshot = host.inspect().await?;
            (
                true,
                Some(identity),
                if snapshot.alias.present {
                    "configured"
                } else {
                    "absent"
                },
            )
        }
        Err(IntegrationError::HostNotFound | IntegrationError::Io(_)) => {
            (false, None, "unavailable")
        }
        Err(error) => return Err(error),
    };
    let skill_root = resolve_skill_root(args.skill_root.as_deref())?;
    let skill_state = inspect_skill(&skill_root.join(SKILL_NAME).join("SKILL.md"))?;
    let registration_state = match args.registration.as_deref() {
        None => "not_requested",
        Some(registration_id) => {
            RegistrationStore::system(&default_state_root()?)?.inspect(registration_id)?;
            "registered"
        }
    };
    write_output(
        &IntegrationInspection {
            schema_version: INSPECTION_SCHEMA_VERSION,
            product_version: env!("CARGO_PKG_VERSION"),
            host: "codex",
            host_available,
            host_identity,
            alias: MCP_ALIAS,
            alias_state,
            skill_state,
            registration_state,
        },
        args.json,
    )
}

async fn plan(args: IntegrationPlanArgs) -> Result<(), IntegrationError> {
    let IntegrationHostKind::Codex = args.host;
    let IntegrationProfile::Mcp = args.profile;
    let states = AuthenticatedIntegrationStore::system(&default_state_root()?)?;
    let host = CodexMcpHost::discover(args.codex_executable.as_deref())?;
    let runtime = std::env::current_exe()?.canonicalize()?;
    let skill_root = resolve_skill_root(args.skill_root.as_deref())?;
    let authority = AuthorityPolicy::load_plan_input_file(&args.authority_policy_file)?;
    let plan = create_plan(
        &states,
        &host,
        &SystemExecutableInspector,
        &runtime,
        &skill_root,
        authority,
        args.principal,
        args.work_scope,
        args.tool_profile,
        current_unix_ms()?,
    )
    .await?;
    write_output(&plan, args.json)
}

async fn apply(args: IntegrationApplyArgs) -> Result<(), IntegrationError> {
    require_confirmation(args.confirm)?;
    let state_root = default_state_root()?;
    let states = AuthenticatedIntegrationStore::system(&state_root)?;
    let registrations = RegistrationStore::system(&state_root)?;
    let plan = load_plan(&states, &args.plan)?;
    let host = CodexMcpHost::discover(Some(&plan.host_executable))?;
    let receipt = apply_plan(
        &states,
        &registrations,
        &host,
        &SystemExecutableInspector,
        &args.plan,
        current_unix_ms()?,
    )
    .await?;
    write_output(&applied(&receipt), args.json)
}

async fn remove(args: IntegrationRemoveArgs) -> Result<(), IntegrationError> {
    require_confirmation(args.confirm)?;
    let state_root = default_state_root()?;
    let states = AuthenticatedIntegrationStore::system(&state_root)?;
    let registrations = RegistrationStore::system(&state_root)?;
    let receipt = load_receipt(&states, &args.registration)?;
    let host = CodexMcpHost::discover(Some(&receipt.host_executable))?;
    remove_integration(
        &states,
        &registrations,
        &host,
        &SystemExecutableInspector,
        &args.registration,
    )
    .await?;
    write_output(
        &IntegrationRemoved {
            schema_version: RECEIPT_SCHEMA_VERSION,
            status: "removed",
            registration_id: args.registration,
            alias: MCP_ALIAS.to_owned(),
        },
        args.json,
    )
}

#[allow(clippy::too_many_arguments)]
async fn create_plan(
    states: &AuthenticatedIntegrationStore,
    host: &dyn McpHost,
    inspector: &dyn ExecutableInspector,
    runtime: &Path,
    skill_root: &Path,
    authority: AuthorityPolicy,
    principal: String,
    work_scope: String,
    tool_profile: ToolProfile,
    now_unix_ms: u64,
) -> Result<IntegrationPlan, IntegrationError> {
    validate_binding(&principal, "principal")?;
    validate_binding(&work_scope, "work_scope")?;
    authority.validate()?;
    let snapshot = host.inspect().await?;
    if snapshot.alias.present {
        return Err(IntegrationError::AliasConflict);
    }
    let skill_root = normalize_future_directory(skill_root)?;
    let skill_file = skill_root.join(SKILL_NAME).join("SKILL.md");
    if skill_file.exists() || skill_file.parent().is_some_and(Path::exists) {
        return Err(IntegrationError::SkillConflict);
    }
    let host_executable = host.executable().canonicalize()?;
    let runtime_executable = runtime.canonicalize()?;
    let host_identity = inspector.inspect(&host_executable)?;
    let runtime_executable_sha256 = inspector.inspect(&runtime_executable)?.executable_sha256;
    let tool_profile = tool_profile_name(tool_profile).to_owned();
    let registration_id = format!("registration_{}", Uuid::new_v4().simple());
    let plan_ref = format!("plan_{}", Uuid::new_v4().simple());
    let registration = RegistrationRequest {
        registration_id: registration_id.clone(),
        provider_id: PROVIDER_ID.to_owned(),
        product_version: env!("CARGO_PKG_VERSION").to_owned(),
        adapter_profile: RegisteredAdapterProfile::Mcp {
            transport: McpRegisteredTransport::Stdio,
            tool_profile: tool_profile.clone(),
        },
        host_identity: host_identity.clone(),
        principal,
        work_scope,
        authority,
    };
    let plan = IntegrationPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_ref: plan_ref.clone(),
        registration_id,
        created_at_unix_ms: now_unix_ms,
        expires_at_unix_ms: now_unix_ms.saturating_add(PLAN_TTL_MS),
        host: "codex".to_owned(),
        profile: "mcp".to_owned(),
        alias: MCP_ALIAS.to_owned(),
        product_version: env!("CARGO_PKG_VERSION").to_owned(),
        transport: McpRegisteredTransport::Stdio,
        tool_profile,
        host_executable,
        host_identity,
        runtime_executable,
        runtime_executable_sha256,
        skill_file: skill_file.clone(),
        skill_sha256: digest(EMBEDDED_MCP_SKILL),
        host_precondition_sha256: snapshot.configuration_sha256,
        registration,
        changes: vec![
            PlannedChange {
                operation: "install_authenticated_registration".to_owned(),
                target: "owner-private provider state".to_owned(),
            },
            PlannedChange {
                operation: "install_mcp_skill".to_owned(),
                target: skill_file.display().to_string(),
            },
            PlannedChange {
                operation: "add_mcp_alias".to_owned(),
                target: MCP_ALIAS.to_owned(),
            },
        ],
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&plan)?);
    states.install(&plan_ref, &bytes)?;
    Ok(plan)
}

async fn apply_plan(
    states: &AuthenticatedIntegrationStore,
    registrations: &RegistrationStore,
    host: &dyn McpHost,
    inspector: &dyn ExecutableInspector,
    plan_ref: &str,
    now_unix_ms: u64,
) -> Result<IntegrationReceipt, IntegrationError> {
    let plan = load_plan(states, plan_ref)?;
    validate_plan(&plan, plan_ref, now_unix_ms)?;
    validate_plan_environment(&plan, host, inspector).await?;
    states.remove(plan_ref)?;

    let mut registration_installed = false;
    let mut skill_installed = false;
    let result = async {
        registrations.install(plan.registration.clone(), now_unix_ms)?;
        registration_installed = true;
        install_skill(&plan.skill_file, &plan.skill_sha256)?;
        skill_installed = true;
        host.add(&plan.runtime_executable, &plan.registration_id)
            .await?;
        let installed = host.inspect().await?;
        if !installed
            .alias
            .is_owned(&plan.runtime_executable, &plan.registration_id)
        {
            return Err(IntegrationError::StateDrift);
        }
        let receipt = receipt_from_plan(&plan, now_unix_ms, installed.alias.sha256);
        let bytes = Zeroizing::new(serde_json::to_vec(&receipt)?);
        states.install(&receipt_ref(&plan.registration_id), &bytes)?;
        Ok(receipt)
    }
    .await;

    match result {
        Ok(receipt) => Ok(receipt),
        Err(error) => {
            let rolled_back = rollback_apply(
                registrations,
                host,
                &plan,
                registration_installed,
                skill_installed,
            )
            .await;
            if rolled_back {
                Err(error)
            } else {
                Err(IntegrationError::RollbackIncomplete)
            }
        }
    }
}

async fn validate_plan_environment(
    plan: &IntegrationPlan,
    host: &dyn McpHost,
    inspector: &dyn ExecutableInspector,
) -> Result<(), IntegrationError> {
    if host.executable().canonicalize()? != plan.host_executable
        || inspector.inspect(host.executable())? != plan.host_identity
    {
        return Err(IntegrationError::StateDrift);
    }
    let runtime = plan.runtime_executable.canonicalize()?;
    if runtime != plan.runtime_executable
        || inspector.inspect(&runtime)?.executable_sha256 != plan.runtime_executable_sha256
    {
        return Err(IntegrationError::StateDrift);
    }
    let skill_root = plan
        .skill_file
        .parent()
        .and_then(Path::parent)
        .ok_or(IntegrationError::StateDrift)?;
    if normalize_future_directory(skill_root)? != skill_root
        || plan.skill_file.exists()
        || plan.skill_file.parent().is_some_and(Path::exists)
    {
        return Err(IntegrationError::StateDrift);
    }
    let snapshot = host.inspect().await?;
    if snapshot.configuration_sha256 != plan.host_precondition_sha256 || snapshot.alias.present {
        return Err(IntegrationError::StalePlan);
    }
    Ok(())
}

async fn rollback_apply(
    registrations: &RegistrationStore,
    host: &dyn McpHost,
    plan: &IntegrationPlan,
    registration_installed: bool,
    skill_installed: bool,
) -> bool {
    let mut clean = true;
    if registration_installed && registrations.remove(&plan.registration_id).is_err() {
        clean = false;
    }
    match host.inspect().await {
        Ok(snapshot)
            if snapshot
                .alias
                .is_owned(&plan.runtime_executable, &plan.registration_id) =>
        {
            if host.remove().await.is_err() {
                clean = false;
            }
        }
        Ok(_) => {}
        Err(_) => clean = false,
    }
    if skill_installed && remove_skill(&plan.skill_file, &plan.skill_sha256).is_err() {
        clean = false;
    }
    clean
}

async fn remove_integration(
    states: &AuthenticatedIntegrationStore,
    registrations: &RegistrationStore,
    host: &dyn McpHost,
    inspector: &dyn ExecutableInspector,
    registration_id: &str,
) -> Result<(), IntegrationError> {
    let receipt = load_receipt(states, registration_id)?;
    validate_receipt(&receipt, registration_id)?;

    // Revocation precedes best-effort host cleanup, so a dangling alias fails closed.
    registrations.remove(registration_id)?;
    if host.executable().canonicalize()? != receipt.host_executable
        || inspector.inspect(host.executable())? != receipt.host_identity
    {
        return Err(IntegrationError::StateDrift);
    }
    let snapshot = host.inspect().await?;
    if snapshot.alias.present
        && (snapshot.alias.sha256 != receipt.alias_sha256
            || !snapshot
                .alias
                .is_owned(&receipt.runtime_executable, registration_id))
    {
        return Err(IntegrationError::StateDrift);
    }
    if inspect_skill_digest(&receipt.skill_file)?
        .is_some_and(|actual| actual != receipt.skill_sha256)
    {
        return Err(IntegrationError::StateDrift);
    }
    if snapshot.alias.present {
        host.remove().await?;
    }
    remove_skill(&receipt.skill_file, &receipt.skill_sha256)?;
    states.remove(&receipt_ref(registration_id))?;
    Ok(())
}

fn load_plan(
    states: &AuthenticatedIntegrationStore,
    plan_ref: &str,
) -> Result<IntegrationPlan, IntegrationError> {
    if !plan_ref.starts_with("plan_") {
        return Err(IntegrationError::StalePlan);
    }
    let bytes = states.load(plan_ref)?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

fn load_receipt(
    states: &AuthenticatedIntegrationStore,
    registration_id: &str,
) -> Result<IntegrationReceipt, IntegrationError> {
    if !registration_id.starts_with("registration_") {
        return Err(IntegrationError::StateDrift);
    }
    let bytes = states.load(&receipt_ref(registration_id))?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

fn validate_plan(
    plan: &IntegrationPlan,
    plan_ref: &str,
    now_unix_ms: u64,
) -> Result<(), IntegrationError> {
    let expected_profile = RegisteredAdapterProfile::Mcp {
        transport: McpRegisteredTransport::Stdio,
        tool_profile: plan.tool_profile.clone(),
    };
    if plan.schema_version != PLAN_SCHEMA_VERSION
        || plan.plan_ref != plan_ref
        || now_unix_ms < plan.created_at_unix_ms
        || now_unix_ms > plan.expires_at_unix_ms
        || plan.expires_at_unix_ms != plan.created_at_unix_ms.saturating_add(PLAN_TTL_MS)
        || plan.host != "codex"
        || plan.profile != "mcp"
        || plan.alias != MCP_ALIAS
        || plan.product_version != env!("CARGO_PKG_VERSION")
        || plan.transport != McpRegisteredTransport::Stdio
        || !matches!(plan.tool_profile.as_str(), "full" | "vision")
        || plan.skill_sha256 != digest(EMBEDDED_MCP_SKILL)
        || plan.registration.registration_id != plan.registration_id
        || plan.registration.provider_id != PROVIDER_ID
        || plan.registration.product_version != plan.product_version
        || plan.registration.host_identity != plan.host_identity
        || plan.registration.adapter_profile != expected_profile
    {
        return Err(IntegrationError::StalePlan);
    }
    plan.registration.authority.validate()?;
    Ok(())
}

fn validate_receipt(
    receipt: &IntegrationReceipt,
    registration_id: &str,
) -> Result<(), IntegrationError> {
    if receipt.schema_version != RECEIPT_SCHEMA_VERSION
        || receipt.registration_id != registration_id
        || receipt.host != "codex"
        || receipt.profile != "mcp"
        || receipt.alias != MCP_ALIAS
        || receipt.product_version != env!("CARGO_PKG_VERSION")
        || receipt.transport != McpRegisteredTransport::Stdio
        || !matches!(receipt.tool_profile.as_str(), "full" | "vision")
        || receipt.skill_sha256 != digest(EMBEDDED_MCP_SKILL)
    {
        return Err(IntegrationError::StateDrift);
    }
    Ok(())
}

fn receipt_from_plan(
    plan: &IntegrationPlan,
    installed_at_unix_ms: u64,
    alias_sha256: String,
) -> IntegrationReceipt {
    IntegrationReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        registration_id: plan.registration_id.clone(),
        installed_at_unix_ms,
        host: plan.host.clone(),
        profile: plan.profile.clone(),
        alias: plan.alias.clone(),
        product_version: plan.product_version.clone(),
        transport: plan.transport,
        tool_profile: plan.tool_profile.clone(),
        host_executable: plan.host_executable.clone(),
        host_identity: plan.host_identity.clone(),
        runtime_executable: plan.runtime_executable.clone(),
        runtime_executable_sha256: plan.runtime_executable_sha256.clone(),
        skill_file: plan.skill_file.clone(),
        skill_sha256: plan.skill_sha256.clone(),
        alias_sha256,
    }
}

fn applied(receipt: &IntegrationReceipt) -> IntegrationApplied {
    IntegrationApplied {
        schema_version: RECEIPT_SCHEMA_VERSION,
        status: "applied",
        registration_id: receipt.registration_id.clone(),
        host: receipt.host.clone(),
        profile: receipt.profile.clone(),
        alias: receipt.alias.clone(),
        transport: receipt.transport,
        tool_profile: receipt.tool_profile.clone(),
        skill_sha256: receipt.skill_sha256.clone(),
    }
}

fn parse_codex_snapshot(bytes: &[u8]) -> Result<HostSnapshot, IntegrationError> {
    let value: Value = serde_json::from_slice(bytes)?;
    let Value::Array(entries) = &value else {
        return Err(IntegrationError::HostOutput);
    };
    let matching = entries
        .iter()
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(MCP_ALIAS))
        .collect::<Vec<_>>();
    let alias = match matching.as_slice() {
        [] => AliasSnapshot {
            present: false,
            sha256: digest(b"nexus.cua.codex.alias.absent.v1\0nexuscua"),
            plain_stdio_command: None,
            plain_stdio_args: None,
        },
        [entry] => parse_alias(entry)?,
        _ => return Err(IntegrationError::HostOutput),
    };
    Ok(HostSnapshot {
        configuration_sha256: digest(&serde_json::to_vec(&value)?),
        alias,
    })
}

fn parse_alias(entry: &Value) -> Result<AliasSnapshot, IntegrationError> {
    let transport = entry
        .get("transport")
        .and_then(Value::as_object)
        .ok_or(IntegrationError::HostOutput)?;
    let transport_type = transport.get("type").and_then(Value::as_str);
    let command = transport.get("command").and_then(Value::as_str);
    let args = transport.get("args").and_then(Value::as_array);
    let parsed_args = match args {
        Some(values) => Some(
            values
                .iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .ok_or(IntegrationError::HostOutput)?,
        ),
        None => None,
    };
    let no_cwd = transport.get("cwd").is_none_or(Value::is_null);
    let no_env = transport.get("env").is_none_or(|value| {
        value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty)
    });
    let no_env_vars = transport.get("env_vars").is_none_or(|value| {
        value.is_null()
            || value.as_array().is_some_and(Vec::is_empty)
            || value.as_object().is_some_and(serde_json::Map::is_empty)
    });
    let plain = transport_type == Some("stdio") && no_cwd && no_env && no_env_vars;
    Ok(AliasSnapshot {
        present: true,
        sha256: digest(&serde_json::to_vec(entry)?),
        plain_stdio_command: plain.then(|| command.map(str::to_owned)).flatten(),
        plain_stdio_args: plain.then_some(parsed_args).flatten(),
    })
}

fn resolve_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names = executable_names(name);
    for directory in std::env::split_paths(&path) {
        for name in &names {
            let candidate = directory.join(name);
            if std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_file()) {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_names(name: &str) -> Vec<OsString> {
    #[cfg(windows)]
    let names = vec![OsString::from(name), OsString::from(format!("{name}.exe"))];
    #[cfg(not(windows))]
    let names = vec![OsString::from(name)];
    names
}

fn resolve_skill_root(explicit: Option<&Path>) -> Result<PathBuf, IntegrationError> {
    let root = match explicit {
        Some(root) => root.to_path_buf(),
        None => default_user_home()?.join(".agents").join("skills"),
    };
    normalize_future_directory(&root)
}

fn normalize_future_directory(path: &Path) -> Result<PathBuf, IntegrationError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(IntegrationError::Invalid(
            "Skill root must be an absolute normalized path".to_owned(),
        ));
    }
    let mut existing = path;
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            IntegrationError::Invalid("Skill root has no existing ancestor".to_owned())
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            IntegrationError::Invalid("Skill root has no existing ancestor".to_owned())
        })?;
    }
    if !std::fs::symlink_metadata(existing)?.is_dir() {
        return Err(IntegrationError::Invalid(
            "Skill root ancestor is not a directory".to_owned(),
        ));
    }
    let mut normalized = existing.canonicalize()?;
    for component in missing.iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

fn inspect_skill(skill_file: &Path) -> Result<&'static str, IntegrationError> {
    match inspect_skill_digest(skill_file)? {
        None if skill_file.parent().is_some_and(Path::exists) => Ok("conflict"),
        None => Ok("absent"),
        Some(value) if value == digest(EMBEDDED_MCP_SKILL) => Ok("matching"),
        Some(_) => Ok("conflict"),
    }
}

fn inspect_skill_digest(skill_file: &Path) -> Result<Option<String>, IntegrationError> {
    let metadata = match std::fs::symlink_metadata(skill_file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_SKILL_BYTES
    {
        return Err(IntegrationError::StateDrift);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    std::fs::File::open(skill_file)?
        .take(MAX_SKILL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SKILL_BYTES {
        return Err(IntegrationError::StateDrift);
    }
    Ok(Some(digest(&bytes)))
}

fn install_skill(skill_file: &Path, expected_sha256: &str) -> Result<(), IntegrationError> {
    if digest(EMBEDDED_MCP_SKILL) != expected_sha256 {
        return Err(IntegrationError::StateDrift);
    }
    let directory = skill_file.parent().ok_or(IntegrationError::StateDrift)?;
    let root = directory.parent().ok_or(IntegrationError::StateDrift)?;
    std::fs::create_dir_all(root)?;
    if root.canonicalize()? != root || std::fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(IntegrationError::StateDrift);
    }
    match std::fs::create_dir(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(IntegrationError::SkillConflict);
        }
        Err(error) => return Err(error.into()),
    }
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(skill_file)?;
        file.write_all(EMBEDDED_MCP_SKILL)?;
        file.sync_all()?;
        if inspect_skill_digest(skill_file)?.as_deref() != Some(expected_sha256) {
            return Err(IntegrationError::StateDrift);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(skill_file);
        let _ = std::fs::remove_dir(directory);
    }
    result
}

fn remove_skill(skill_file: &Path, expected_sha256: &str) -> Result<(), IntegrationError> {
    match inspect_skill_digest(skill_file)? {
        None => {
            if let Some(directory) = skill_file.parent()
                && directory.exists()
            {
                std::fs::remove_dir(directory)?;
            }
            return Ok(());
        }
        Some(actual) if actual == expected_sha256 => {}
        Some(_) => return Err(IntegrationError::StateDrift),
    }
    std::fs::remove_file(skill_file)?;
    std::fs::remove_dir(skill_file.parent().ok_or(IntegrationError::StateDrift)?)?;
    Ok(())
}

fn require_confirmation(confirm: bool) -> Result<(), IntegrationError> {
    if confirm && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(IntegrationError::ConfirmationRequired)
    }
}

fn validate_binding(value: &str, field: &str) -> Result<(), IntegrationError> {
    if value.is_empty()
        || value.len() > 256
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        return Err(IntegrationError::Invalid(format!(
            "{field} is empty, unnormalized, or exceeds 256 bytes"
        )));
    }
    Ok(())
}

fn tool_profile_name(profile: ToolProfile) -> &'static str {
    match profile {
        ToolProfile::Full => "full",
        ToolProfile::Vision => "vision",
    }
}

fn write_output(value: &impl Serialize, compact: bool) -> Result<(), IntegrationError> {
    if compact {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn receipt_ref(registration_id: &str) -> String {
    registration_id.replacen("registration_", "receipt_", 1)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use nexus_cua_platform::HostSignatureIdentity;
    use nexus_cua_protocol::{ActionKind, PermissionMode};
    use nexus_cua_provider::{SecretStore, SecretStoreError};

    use super::*;

    #[derive(Default)]
    struct MemorySecrets(Mutex<HashMap<String, Vec<u8>>>);

    impl SecretStore for MemorySecrets {
        fn put(&self, id: &str, secret: &[u8]) -> Result<(), SecretStoreError> {
            let mut values = self.0.lock().expect("secret lock");
            if values.contains_key(id) {
                return Err(SecretStoreError::AlreadyExists);
            }
            values.insert(id.to_owned(), secret.to_vec());
            Ok(())
        }

        fn get(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, SecretStoreError> {
            self.0
                .lock()
                .expect("secret lock")
                .get(id)
                .cloned()
                .map(Zeroizing::new)
                .ok_or(SecretStoreError::NotFound)
        }

        fn delete(&self, id: &str) -> Result<(), SecretStoreError> {
            self.0.lock().expect("secret lock").remove(id);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeState {
        generation: u64,
        alias: Option<(String, String)>,
        fail_after_add: bool,
    }

    struct FakeHost {
        executable: PathBuf,
        state: Mutex<FakeState>,
    }

    impl FakeHost {
        fn new(executable: PathBuf) -> Self {
            Self {
                executable,
                state: Mutex::new(FakeState::default()),
            }
        }

        fn has_alias(&self) -> bool {
            self.state.lock().expect("host lock").alias.is_some()
        }

        fn drift(&self) {
            self.state.lock().expect("host lock").generation += 1;
        }

        fn fail_after_add(&self) {
            self.state.lock().expect("host lock").fail_after_add = true;
        }
    }

    #[async_trait]
    impl McpHost for FakeHost {
        fn executable(&self) -> &Path {
            &self.executable
        }

        async fn inspect(&self) -> Result<HostSnapshot, IntegrationError> {
            let state = self.state.lock().expect("host lock");
            let alias = match &state.alias {
                Some((runtime, registration)) => AliasSnapshot {
                    present: true,
                    sha256: digest(format!("{runtime}\0{registration}").as_bytes()),
                    plain_stdio_command: Some(runtime.clone()),
                    plain_stdio_args: Some(
                        ["mcp", "launch", "--registration", registration]
                            .map(str::to_owned)
                            .to_vec(),
                    ),
                },
                None => AliasSnapshot {
                    present: false,
                    sha256: digest(b"absent"),
                    plain_stdio_command: None,
                    plain_stdio_args: None,
                },
            };
            Ok(HostSnapshot {
                configuration_sha256: digest(
                    format!("{}:{:?}", state.generation, state.alias).as_bytes(),
                ),
                alias,
            })
        }

        async fn add(&self, runtime: &Path, registration_id: &str) -> Result<(), IntegrationError> {
            let runtime = runtime.to_str().expect("UTF-8 test runtime").to_owned();
            let mut state = self.state.lock().expect("host lock");
            state.alias = Some((runtime, registration_id.to_owned()));
            if state.fail_after_add {
                Err(IntegrationError::HostCommand)
            } else {
                Ok(())
            }
        }

        async fn remove(&self) -> Result<(), IntegrationError> {
            self.state.lock().expect("host lock").alias = None;
            Ok(())
        }
    }

    struct FixedInspector(HostExecutableIdentity);

    impl ExecutableInspector for FixedInspector {
        fn inspect(&self, _path: &Path) -> Result<HostExecutableIdentity, IntegrationError> {
            Ok(self.0.clone())
        }
    }

    fn identity() -> HostExecutableIdentity {
        HostExecutableIdentity {
            path_sha256: "a".repeat(64),
            executable_sha256: "b".repeat(64),
            signature: HostSignatureIdentity::Macos {
                signing_team_id: Some("TEAM123456".to_owned()),
                designated_requirement: Some("identifier com.example.codex".to_owned()),
            },
        }
    }

    fn authority() -> AuthorityPolicy {
        AuthorityPolicy {
            schema_version: 1,
            mode: PermissionMode::Bounded,
            application_ids: vec!["com.example.Fixture".to_owned()],
            allowed_actions: vec![ActionKind::ClickPoint],
            allow_foreground_input: true,
            restore_foreground_after_input: true,
            allow_display_capture: false,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            allow_application_launch: true,
            allow_application_launch_activation: true,
            ttl_seconds: 60,
        }
    }

    struct Fixture {
        root: PathBuf,
        states: AuthenticatedIntegrationStore,
        registrations: RegistrationStore,
        host: FakeHost,
        inspector: FixedInspector,
        runtime: PathBuf,
        skill_root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("nexuscua-integration-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).expect("fixture root");
            let runtime = std::env::current_exe()
                .expect("test executable")
                .canonicalize()
                .expect("canonical test executable");
            let secrets = Arc::new(MemorySecrets::default());
            let states = AuthenticatedIntegrationStore::with_secret_store(
                &root.join("states"),
                secrets.clone(),
            )
            .expect("integration state");
            let registrations =
                RegistrationStore::with_secret_store(&root.join("registrations"), secrets)
                    .expect("registration state");
            Self {
                host: FakeHost::new(runtime.clone()),
                inspector: FixedInspector(identity()),
                skill_root: root.join("skills"),
                root,
                states,
                registrations,
                runtime,
            }
        }

        async fn plan(&self, now: u64) -> IntegrationPlan {
            create_plan(
                &self.states,
                &self.host,
                &self.inspector,
                &self.runtime,
                &self.skill_root,
                authority(),
                "local-user".to_owned(),
                "fixture-window".to_owned(),
                ToolProfile::Full,
                now,
            )
            .await
            .expect("create plan")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).expect("remove fixture root");
        }
    }

    #[test]
    fn codex_snapshot_tracks_full_configuration_and_exact_owned_alias() {
        let bytes = br#"[{"name":"nexuscua","enabled":true,"transport":{"type":"stdio","command":"/opt/nexuscua","args":["mcp","launch","--registration","registration_0123456789abcdef"],"env":null,"env_vars":[],"cwd":null}}]"#;
        let snapshot = parse_codex_snapshot(bytes).expect("Codex snapshot");
        assert!(snapshot.alias.present);
        assert!(
            snapshot
                .alias
                .is_owned(Path::new("/opt/nexuscua"), "registration_0123456789abcdef")
        );
        assert_eq!(snapshot.configuration_sha256.len(), 64);
    }

    #[tokio::test]
    async fn fresh_plan_applies_and_owned_remove_cleans_all_state() {
        let fixture = Fixture::new();
        let plan = fixture.plan(1_000).await;
        let receipt = apply_plan(
            &fixture.states,
            &fixture.registrations,
            &fixture.host,
            &fixture.inspector,
            &plan.plan_ref,
            1_001,
        )
        .await
        .expect("apply plan");
        assert!(fixture.host.has_alias());
        assert!(receipt.skill_file.is_file());
        fixture
            .registrations
            .inspect(&plan.registration_id)
            .expect("registered authority");
        remove_integration(
            &fixture.states,
            &fixture.registrations,
            &fixture.host,
            &fixture.inspector,
            &plan.registration_id,
        )
        .await
        .expect("remove integration");
        assert!(!fixture.host.has_alias());
        assert!(!receipt.skill_file.exists());
        assert!(
            fixture
                .registrations
                .inspect(&plan.registration_id)
                .is_err()
        );
    }

    #[tokio::test]
    async fn changed_host_configuration_rejects_and_retains_plan() {
        let fixture = Fixture::new();
        let plan = fixture.plan(2_000).await;
        fixture.host.drift();
        let error = apply_plan(
            &fixture.states,
            &fixture.registrations,
            &fixture.host,
            &fixture.inspector,
            &plan.plan_ref,
            2_001,
        )
        .await
        .expect_err("stale plan");
        assert!(matches!(error, IntegrationError::StalePlan));
        fixture.states.load(&plan.plan_ref).expect("plan retained");
        assert!(!fixture.host.has_alias());
    }

    #[tokio::test]
    async fn uncertain_host_add_rolls_back_all_owned_state() {
        let fixture = Fixture::new();
        let plan = fixture.plan(3_000).await;
        fixture.host.fail_after_add();
        let error = apply_plan(
            &fixture.states,
            &fixture.registrations,
            &fixture.host,
            &fixture.inspector,
            &plan.plan_ref,
            3_001,
        )
        .await
        .expect_err("host add failure");
        assert!(matches!(error, IntegrationError::HostCommand));
        assert!(!fixture.host.has_alias());
        assert!(!plan.skill_file.exists());
        assert!(
            fixture
                .registrations
                .inspect(&plan.registration_id)
                .is_err()
        );
        assert!(fixture.states.load(&plan.plan_ref).is_err());
    }
}
