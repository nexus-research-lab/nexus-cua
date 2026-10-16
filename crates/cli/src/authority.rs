//! Interactive, host-bound authority registration lifecycle.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use nexus_cua_mcp::{AuthorityPolicy, current_unix_ms};
use nexus_cua_platform::{HostExecutableIdentity, inspect_host_executable};
use nexus_cua_provider::{
    AuthenticatedIntegrationStore, RegisteredAdapterProfile, RegistrationError,
    RegistrationRequest, RegistrationStore, RegistrationSummary, SecretStoreError,
    default_state_root,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroizing;

const PLAN_SCHEMA_VERSION: u32 = 1;
const RECEIPT_SCHEMA_VERSION: u32 = 1;
const REVOCATION_SCHEMA_VERSION: u32 = 1;
const STATUS_SCHEMA_VERSION: u32 = 1;
const PLAN_TTL_MS: u64 = 10 * 60 * 1_000;
const PROVIDER_ID: &str = "nexuscua.local.v1";

/// Human authority-management command group.
#[derive(Debug, Args)]
pub(crate) struct AuthorityArgs {
    /// Authority lifecycle operation.
    #[command(subcommand)]
    pub(crate) command: AuthorityCommand,
}

/// Closed authority-management operations.
#[derive(Debug, Subcommand)]
pub(crate) enum AuthorityCommand {
    /// Create a short-lived, authenticated CLI authority plan.
    Plan(AuthorityPlanArgs),
    /// Apply one fresh plan after real-terminal confirmation.
    Apply(AuthorityApplyArgs),
    /// Inspect one authenticated CLI authority registration.
    Status(AuthorityStatusArgs),
    /// Revoke one registration after real-terminal confirmation.
    Revoke(AuthorityRevokeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AuthorityPlanArgs {
    /// Assert this human-only flow; stdin and stderr must also be terminals.
    #[arg(long)]
    interactive: bool,
    /// Exact host executable identity that may launch the future CLI adapter.
    #[arg(long)]
    host_executable: PathBuf,
    /// Owner-private, human-authored authority ceiling consumed only by planning.
    #[arg(long)]
    authority_policy_file: PathBuf,
    /// Authenticated local principal label displayed in the plan.
    #[arg(long)]
    principal: String,
    /// Bounded work scope displayed in the plan and bound into every grant.
    #[arg(long)]
    work_scope: String,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AuthorityApplyArgs {
    /// Opaque plan identity returned by `authority plan`.
    #[arg(long)]
    plan: String,
    /// Confirm the exact visible plan. Non-terminal invocations are rejected.
    #[arg(long)]
    confirm: bool,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AuthorityStatusArgs {
    /// Non-secret registration identity returned by apply.
    #[arg(long)]
    registration: String,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AuthorityRevokeArgs {
    /// Non-secret registration identity returned by apply.
    #[arg(long)]
    registration: String,
    /// Confirm revocation from a real terminal.
    #[arg(long)]
    confirm: bool,
    /// Emit one compact JSON object.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Error)]
pub(crate) enum AuthorityCliError {
    #[error("authority input is invalid: {0}")]
    Invalid(String),
    #[error("human confirmation from a real terminal is required")]
    ConfirmationRequired,
    #[error("authority plan is stale, consumed, or expired")]
    StalePlan,
    #[error("authority state no longer matches its authenticated plan or receipt")]
    StateDrift,
    #[error("authority apply failed and registration rollback was incomplete")]
    RollbackIncomplete,
    #[error("authority I/O failed")]
    Io(#[from] std::io::Error),
    #[error("authority JSON failed")]
    Json(#[from] serde_json::Error),
    #[error("registered authority state failed")]
    Provider(#[from] RegistrationError),
    #[error("authority policy is invalid")]
    Policy(#[from] nexus_cua_mcp::AuthorityError),
    #[error("host executable identity could not be established")]
    HostIdentity(#[from] nexus_cua_platform::HostIdentityError),
    #[error(transparent)]
    Service(#[from] crate::service::ServiceError),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorityPlan {
    schema_version: u32,
    plan_ref: String,
    registration_id: String,
    created_at_unix_ms: u64,
    expires_at_unix_ms: u64,
    product_version: String,
    host_executable: PathBuf,
    host_identity: HostExecutableIdentity,
    registration: RegistrationRequest,
    changes: Vec<AuthorityChange>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorityChange {
    operation: String,
    target: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorityReceipt {
    schema_version: u32,
    registration_id: String,
    installed_at_unix_ms: u64,
    product_version: String,
    host_executable: PathBuf,
    host_identity: HostExecutableIdentity,
    registration: RegistrationRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorityRevocation {
    schema_version: u32,
    registration_id: String,
    requested_at_unix_ms: u64,
    product_version: String,
    receipt_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorityStatus {
    schema_version: u32,
    state: &'static str,
    registration_id: String,
    product_version: String,
    provider_id: String,
    adapter_profile: RegisteredAdapterProfile,
    installed_at_unix_ms: u64,
    revocation_requested_at_unix_ms: Option<u64>,
    host_executable: PathBuf,
    host_identity: HostExecutableIdentity,
    principal: String,
    work_scope: String,
    authority: AuthorityPolicy,
}

trait ExecutableInspector: Send + Sync {
    fn inspect(&self, path: &Path) -> Result<HostExecutableIdentity, AuthorityCliError>;
}

struct SystemExecutableInspector;

impl ExecutableInspector for SystemExecutableInspector {
    fn inspect(&self, path: &Path) -> Result<HostExecutableIdentity, AuthorityCliError> {
        inspect_host_executable(path).map_err(Into::into)
    }
}

pub(crate) async fn run(args: AuthorityArgs) -> Result<(), AuthorityCliError> {
    match args.command {
        AuthorityCommand::Plan(args) => plan(args),
        AuthorityCommand::Apply(args) => apply(&args),
        AuthorityCommand::Status(args) => status(&args),
        AuthorityCommand::Revoke(args) => revoke(&args).await,
    }
}

fn plan(args: AuthorityPlanArgs) -> Result<(), AuthorityCliError> {
    require_confirmation(args.interactive)?;
    let states = AuthenticatedIntegrationStore::system(&default_state_root()?)?;
    let authority = AuthorityPolicy::load_plan_input_file(&args.authority_policy_file)?;
    let plan = create_plan(
        &states,
        &SystemExecutableInspector,
        &args.host_executable,
        authority,
        args.principal,
        args.work_scope,
        current_unix_ms()?,
    )?;
    write_output(&plan, args.json)
}

fn apply(args: &AuthorityApplyArgs) -> Result<(), AuthorityCliError> {
    require_confirmation(args.confirm)?;
    let state_root = default_state_root()?;
    let state_store = AuthenticatedIntegrationStore::system(&state_root)?;
    let registrations = RegistrationStore::system(&state_root)?;
    let receipt = apply_plan(
        &state_store,
        &registrations,
        &SystemExecutableInspector,
        &args.plan,
        current_unix_ms()?,
    )?;
    let authority_state = status_from_receipt(&registrations, &receipt, None)?;
    write_output(&authority_state, args.json)
}

fn status(args: &AuthorityStatusArgs) -> Result<(), AuthorityCliError> {
    let state_root = default_state_root()?;
    let state_store = AuthenticatedIntegrationStore::system(&state_root)?;
    let registrations = RegistrationStore::system(&state_root)?;
    let authority_state = inspect_status(&state_store, &registrations, &args.registration)?;
    write_output(&authority_state, args.json)
}

async fn revoke(args: &AuthorityRevokeArgs) -> Result<(), AuthorityCliError> {
    require_confirmation(args.confirm)?;
    let state_root = default_state_root()?;
    let state_store = AuthenticatedIntegrationStore::system(&state_root)?;
    let registrations = RegistrationStore::system(&state_root)?;
    let authority_state = revoke_registration(
        &state_store,
        &registrations,
        &args.registration,
        current_unix_ms()?,
    )?;
    crate::service::wait_for_registration_shutdown(&args.registration).await?;
    write_output(&authority_state, args.json)
}

fn create_plan(
    states: &AuthenticatedIntegrationStore,
    inspector: &dyn ExecutableInspector,
    host_executable: &Path,
    authority: AuthorityPolicy,
    principal: String,
    work_scope: String,
    now_unix_ms: u64,
) -> Result<AuthorityPlan, AuthorityCliError> {
    validate_binding(&principal, "principal")?;
    validate_binding(&work_scope, "work_scope")?;
    authority.validate()?;
    let host_executable = host_executable.canonicalize()?;
    let host_identity = inspector.inspect(&host_executable)?;
    let registration_id = format!("registration_{}", Uuid::new_v4().simple());
    let plan_ref = format!("authplan_{}", Uuid::new_v4().simple());
    let registration = RegistrationRequest {
        registration_id: registration_id.clone(),
        provider_id: PROVIDER_ID.to_owned(),
        product_version: env!("CARGO_PKG_VERSION").to_owned(),
        adapter_profile: RegisteredAdapterProfile::Cli,
        host_identity: host_identity.clone(),
        principal,
        work_scope,
        authority,
    };
    let plan = AuthorityPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_ref: plan_ref.clone(),
        registration_id,
        created_at_unix_ms: now_unix_ms,
        expires_at_unix_ms: now_unix_ms.saturating_add(PLAN_TTL_MS),
        product_version: env!("CARGO_PKG_VERSION").to_owned(),
        host_executable,
        host_identity,
        registration,
        changes: vec![AuthorityChange {
            operation: "install_authenticated_cli_registration".to_owned(),
            target: "owner-private provider state".to_owned(),
        }],
    };
    install_state(states, &plan_ref, &plan)?;
    Ok(plan)
}

fn apply_plan(
    states: &AuthenticatedIntegrationStore,
    registrations: &RegistrationStore,
    inspector: &dyn ExecutableInspector,
    plan_ref: &str,
    now_unix_ms: u64,
) -> Result<AuthorityReceipt, AuthorityCliError> {
    let plan = load_plan(states, plan_ref)?;
    validate_plan(&plan, plan_ref, now_unix_ms)?;
    let executable = plan.host_executable.canonicalize()?;
    if executable != plan.host_executable || inspector.inspect(&executable)? != plan.host_identity {
        return Err(AuthorityCliError::StateDrift);
    }
    states.remove(plan_ref)?;
    registrations.install(plan.registration.clone(), now_unix_ms)?;
    let receipt = AuthorityReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        registration_id: plan.registration_id.clone(),
        installed_at_unix_ms: now_unix_ms,
        product_version: plan.product_version,
        host_executable: plan.host_executable,
        host_identity: plan.host_identity,
        registration: plan.registration,
    };
    let install_receipt = install_state(states, &receipt_ref(&receipt.registration_id), &receipt);
    if let Err(error) = install_receipt {
        if registrations.remove(&receipt.registration_id).is_err() {
            return Err(AuthorityCliError::RollbackIncomplete);
        }
        return Err(error);
    }
    Ok(receipt)
}

fn inspect_status(
    states: &AuthenticatedIntegrationStore,
    registrations: &RegistrationStore,
    registration_id: &str,
) -> Result<AuthorityStatus, AuthorityCliError> {
    let receipt = load_receipt(states, registration_id)?;
    validate_receipt(&receipt, registration_id)?;
    let revocation = load_optional_revocation(states, registration_id)?;
    status_from_receipt(registrations, &receipt, revocation.as_ref())
}

fn status_from_receipt(
    registrations: &RegistrationStore,
    receipt: &AuthorityReceipt,
    revocation: Option<&AuthorityRevocation>,
) -> Result<AuthorityStatus, AuthorityCliError> {
    if let Some(revocation) = revocation {
        validate_revocation(revocation, receipt)?;
    }
    let (state, summary) = match registrations.inspect(&receipt.registration_id) {
        Ok(summary) => (
            if revocation.is_some() {
                "revocation_pending"
            } else {
                "active"
            },
            Some(summary),
        ),
        Err(error) if revocation.is_some() && is_missing_registration(&error) => ("revoked", None),
        Err(error) => return Err(error.into()),
    };
    if let Some(summary) = summary.as_ref() {
        validate_summary(summary, receipt)?;
    }
    Ok(AuthorityStatus {
        schema_version: STATUS_SCHEMA_VERSION,
        state,
        registration_id: receipt.registration_id.clone(),
        product_version: receipt.product_version.clone(),
        provider_id: receipt.registration.provider_id.clone(),
        adapter_profile: receipt.registration.adapter_profile.clone(),
        installed_at_unix_ms: receipt.installed_at_unix_ms,
        revocation_requested_at_unix_ms: revocation.map(|value| value.requested_at_unix_ms),
        host_executable: receipt.host_executable.clone(),
        host_identity: receipt.host_identity.clone(),
        principal: receipt.registration.principal.clone(),
        work_scope: receipt.registration.work_scope.clone(),
        authority: receipt.registration.authority.clone(),
    })
}

fn revoke_registration(
    states: &AuthenticatedIntegrationStore,
    registrations: &RegistrationStore,
    registration_id: &str,
    now_unix_ms: u64,
) -> Result<AuthorityStatus, AuthorityCliError> {
    let receipt = load_receipt(states, registration_id)?;
    validate_receipt(&receipt, registration_id)?;
    match registrations.inspect(registration_id) {
        Ok(summary) => validate_summary(&summary, &receipt)?,
        Err(error) if is_missing_registration(&error) => {}
        Err(error) => return Err(error.into()),
    }
    let receipt_sha256 = serialized_digest(&receipt)?;
    let revocation = if let Some(existing) = load_optional_revocation(states, registration_id)? {
        validate_revocation(&existing, &receipt)?;
        existing
    } else {
        let value = AuthorityRevocation {
            schema_version: REVOCATION_SCHEMA_VERSION,
            registration_id: registration_id.to_owned(),
            requested_at_unix_ms: now_unix_ms,
            product_version: env!("CARGO_PKG_VERSION").to_owned(),
            receipt_sha256,
        };
        install_state(states, &revocation_ref(registration_id), &value)?;
        value
    };
    registrations.remove(registration_id)?;
    status_from_receipt(registrations, &receipt, Some(&revocation))
}

fn load_plan(
    states: &AuthenticatedIntegrationStore,
    plan_ref: &str,
) -> Result<AuthorityPlan, AuthorityCliError> {
    if !plan_ref.starts_with("authplan_") {
        return Err(AuthorityCliError::StalePlan);
    }
    let bytes = states.load(plan_ref)?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

fn load_receipt(
    states: &AuthenticatedIntegrationStore,
    registration_id: &str,
) -> Result<AuthorityReceipt, AuthorityCliError> {
    if !registration_id.starts_with("registration_") {
        return Err(AuthorityCliError::StateDrift);
    }
    let bytes = states.load(&receipt_ref(registration_id))?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

fn load_optional_revocation(
    states: &AuthenticatedIntegrationStore,
    registration_id: &str,
) -> Result<Option<AuthorityRevocation>, AuthorityCliError> {
    match states.load(&revocation_ref(registration_id)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(Into::into),
        Err(RegistrationError::NotFound) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn validate_plan(
    plan: &AuthorityPlan,
    plan_ref: &str,
    now_unix_ms: u64,
) -> Result<(), AuthorityCliError> {
    if plan.schema_version != PLAN_SCHEMA_VERSION
        || plan.plan_ref != plan_ref
        || now_unix_ms < plan.created_at_unix_ms
        || now_unix_ms > plan.expires_at_unix_ms
        || plan.expires_at_unix_ms != plan.created_at_unix_ms.saturating_add(PLAN_TTL_MS)
        || plan.product_version != env!("CARGO_PKG_VERSION")
        || plan.registration_id != plan.registration.registration_id
        || plan.registration.provider_id != PROVIDER_ID
        || plan.registration.product_version != plan.product_version
        || plan.registration.adapter_profile != RegisteredAdapterProfile::Cli
        || plan.registration.host_identity != plan.host_identity
        || plan.changes.len() != 1
        || plan.changes[0].operation != "install_authenticated_cli_registration"
        || plan.changes[0].target != "owner-private provider state"
    {
        return Err(AuthorityCliError::StalePlan);
    }
    plan.registration.authority.validate()?;
    validate_binding(&plan.registration.principal, "principal")?;
    validate_binding(&plan.registration.work_scope, "work_scope")
}

fn validate_receipt(
    receipt: &AuthorityReceipt,
    registration_id: &str,
) -> Result<(), AuthorityCliError> {
    if receipt.schema_version != RECEIPT_SCHEMA_VERSION
        || receipt.registration_id != registration_id
        || receipt.product_version != env!("CARGO_PKG_VERSION")
        || receipt.registration.registration_id != registration_id
        || receipt.registration.product_version != receipt.product_version
        || receipt.registration.provider_id != PROVIDER_ID
        || receipt.registration.adapter_profile != RegisteredAdapterProfile::Cli
        || receipt.registration.host_identity != receipt.host_identity
    {
        return Err(AuthorityCliError::StateDrift);
    }
    receipt.registration.authority.validate()?;
    Ok(())
}

fn validate_revocation(
    revocation: &AuthorityRevocation,
    receipt: &AuthorityReceipt,
) -> Result<(), AuthorityCliError> {
    if revocation.schema_version != REVOCATION_SCHEMA_VERSION
        || revocation.registration_id != receipt.registration_id
        || revocation.product_version != receipt.product_version
        || revocation.receipt_sha256 != serialized_digest(receipt)?
    {
        return Err(AuthorityCliError::StateDrift);
    }
    Ok(())
}

fn validate_summary(
    summary: &RegistrationSummary,
    receipt: &AuthorityReceipt,
) -> Result<(), AuthorityCliError> {
    if summary.registration_id != receipt.registration_id
        || summary.provider_id != receipt.registration.provider_id
        || summary.product_version != receipt.product_version
        || summary.adapter_profile != RegisteredAdapterProfile::Cli
        || summary.installed_at_unix_ms != receipt.installed_at_unix_ms
        || summary.authority_ttl_seconds != receipt.registration.authority.ttl_seconds
    {
        return Err(AuthorityCliError::StateDrift);
    }
    Ok(())
}

fn is_missing_registration(error: &RegistrationError) -> bool {
    matches!(
        error,
        RegistrationError::NotFound | RegistrationError::Secret(SecretStoreError::NotFound)
    )
}

fn require_confirmation(confirm: bool) -> Result<(), AuthorityCliError> {
    if confirm && std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        Ok(())
    } else {
        Err(AuthorityCliError::ConfirmationRequired)
    }
}

fn validate_binding(value: &str, field: &str) -> Result<(), AuthorityCliError> {
    if value.is_empty()
        || value.len() > 256
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        return Err(AuthorityCliError::Invalid(format!(
            "{field} is empty, unnormalized, or exceeds 256 bytes"
        )));
    }
    Ok(())
}

fn write_output(value: &impl Serialize, compact: bool) -> Result<(), AuthorityCliError> {
    if compact {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

fn install_state(
    states: &AuthenticatedIntegrationStore,
    state_ref: &str,
    value: &impl Serialize,
) -> Result<(), AuthorityCliError> {
    let bytes = Zeroizing::new(serde_json::to_vec(value)?);
    states.install(state_ref, &bytes)?;
    Ok(())
}

fn serialized_digest(value: &impl Serialize) -> Result<String, AuthorityCliError> {
    let bytes = Zeroizing::new(serde_json::to_vec(value)?);
    Ok(digest(&bytes))
}

fn receipt_ref(registration_id: &str) -> String {
    registration_id.replacen("registration_", "authrcpt_", 1)
}

fn revocation_ref(registration_id: &str) -> String {
    registration_id.replacen("registration_", "authrevoke_", 1)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
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

    struct FixedInspector(HostExecutableIdentity);

    impl ExecutableInspector for FixedInspector {
        fn inspect(&self, _path: &Path) -> Result<HostExecutableIdentity, AuthorityCliError> {
            Ok(self.0.clone())
        }
    }

    fn identity(digest_byte: char) -> HostExecutableIdentity {
        HostExecutableIdentity {
            path_sha256: digest_byte.to_string().repeat(64),
            executable_sha256: digest_byte.to_string().repeat(64),
            signature: HostSignatureIdentity::Macos {
                signing_team_id: Some("TEAM123456".to_owned()),
                designated_requirement: Some("identifier com.example.host".to_owned()),
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
        executable: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("nexuscua-authority-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).expect("fixture root");
            let secrets = Arc::new(MemorySecrets::default());
            Self {
                states: AuthenticatedIntegrationStore::with_secret_store(
                    &root.join("states"),
                    secrets.clone(),
                )
                .expect("state store"),
                registrations: RegistrationStore::with_secret_store(
                    &root.join("registrations"),
                    secrets,
                )
                .expect("registration store"),
                executable: std::env::current_exe()
                    .expect("current executable")
                    .canonicalize()
                    .expect("canonical executable"),
                root,
            }
        }

        fn plan(&self, now: u64) -> AuthorityPlan {
            create_plan(
                &self.states,
                &FixedInspector(identity('a')),
                &self.executable,
                authority(),
                "local-user".to_owned(),
                "fixture-window".to_owned(),
                now,
            )
            .expect("create plan")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).expect("remove fixture root");
        }
    }

    #[test]
    fn plan_is_cli_bound_and_authenticated() {
        let fixture = Fixture::new();
        let plan = fixture.plan(1_000);
        assert_eq!(
            plan.registration.adapter_profile,
            RegisteredAdapterProfile::Cli
        );
        assert_eq!(
            plan.registration.authority.application_ids,
            ["com.example.Fixture"]
        );
        load_plan(&fixture.states, &plan.plan_ref).expect("authenticated plan");
    }

    #[test]
    fn fresh_plan_applies_and_status_cross_checks_registration() {
        let fixture = Fixture::new();
        let plan = fixture.plan(2_000);
        let receipt = apply_plan(
            &fixture.states,
            &fixture.registrations,
            &FixedInspector(identity('a')),
            &plan.plan_ref,
            2_001,
        )
        .expect("apply plan");
        assert!(fixture.states.load(&plan.plan_ref).is_err());
        let status = inspect_status(
            &fixture.states,
            &fixture.registrations,
            &receipt.registration_id,
        )
        .expect("active status");
        assert_eq!(status.state, "active");
        assert_eq!(status.adapter_profile, RegisteredAdapterProfile::Cli);
    }

    #[test]
    fn changed_host_identity_rejects_and_retains_plan() {
        let fixture = Fixture::new();
        let plan = fixture.plan(3_000);
        let error = apply_plan(
            &fixture.states,
            &fixture.registrations,
            &FixedInspector(identity('b')),
            &plan.plan_ref,
            3_001,
        )
        .expect_err("changed identity");
        assert!(matches!(error, AuthorityCliError::StateDrift));
        fixture.states.load(&plan.plan_ref).expect("plan retained");
    }

    #[test]
    fn expired_plan_is_not_consumed() {
        let fixture = Fixture::new();
        let plan = fixture.plan(4_000);
        let error = apply_plan(
            &fixture.states,
            &fixture.registrations,
            &FixedInspector(identity('a')),
            &plan.plan_ref,
            4_000 + PLAN_TTL_MS + 1,
        )
        .expect_err("expired plan");
        assert!(matches!(error, AuthorityCliError::StalePlan));
        fixture.states.load(&plan.plan_ref).expect("plan retained");
    }

    #[test]
    fn revocation_intent_and_missing_registration_prove_revoked_state() {
        let fixture = Fixture::new();
        let plan = fixture.plan(5_000);
        let receipt = apply_plan(
            &fixture.states,
            &fixture.registrations,
            &FixedInspector(identity('a')),
            &plan.plan_ref,
            5_001,
        )
        .expect("apply plan");
        let status = revoke_registration(
            &fixture.states,
            &fixture.registrations,
            &receipt.registration_id,
            5_002,
        )
        .expect("revoke registration");
        assert_eq!(status.state, "revoked");
        assert_eq!(status.revocation_requested_at_unix_ms, Some(5_002));
        assert!(
            fixture
                .registrations
                .inspect(&receipt.registration_id)
                .is_err()
        );
        assert_eq!(
            inspect_status(
                &fixture.states,
                &fixture.registrations,
                &receipt.registration_id
            )
            .expect("persisted revoked status")
            .state,
            "revoked"
        );
    }
}
