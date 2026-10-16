//! Host-authored authority policy loading and validation.

use std::collections::HashSet;
use std::fmt;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nexus_cua_protocol::{ActionKind, PermissionMode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const AUTHORITY_SCHEMA_VERSION: u32 = 1;
const PROVIDER_GRANT_SCHEMA_VERSION: u32 = 1;
const MAX_AUTHORITY_BYTES: u64 = 64 * 1024;
const MAX_APPLICATIONS: usize = 32;
const MAX_APPLICATION_ID_BYTES: usize = 512;
const MAX_ACTIONS: usize = 32;
const MAX_TTL_SECONDS: u32 = 60 * 60;
const MAX_BINDING_BYTES: usize = 256;
const MAX_REQUEST_NAMESPACE_BYTES: usize = 64;
const MAX_CLOCK_SKEW_MS: u64 = 30_000;
const MIN_REMAINING_GRANT_MS: u64 = 1_000;

/// A host-authored, model-inaccessible ceiling for one MCP process.
#[allow(clippy::struct_excessive_bools)] // Closed JSON policy flags remain independently reviewable.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityPolicy {
    /// Authority file schema version.
    pub schema_version: u32,
    /// Runtime session mode.
    pub mode: PermissionMode,
    /// Exact stable application identities allowed for this MCP process.
    pub application_ids: Vec<String>,
    /// Exact mutation categories admitted by the runtime.
    pub allowed_actions: Vec<ActionKind>,
    /// Whether foreground input routes are admitted.
    pub allow_foreground_input: bool,
    /// Whether temporary foreground input restores the exact prior owner.
    pub restore_foreground_after_input: bool,
    /// Whether the model may capture one exact physical display for read-only overview.
    #[serde(default)]
    pub allow_display_capture: bool,
    /// Whether the model may read bounded plain text from the system clipboard.
    #[serde(default)]
    pub allow_clipboard_read: bool,
    /// Whether the model may replace clipboard text after presenting a fresh read guard.
    #[serde(default)]
    pub allow_clipboard_write: bool,
    /// Whether the model may cold-launch an exact configured installed identity.
    pub allow_application_launch: bool,
    /// Whether a cold-launched application may become foreground.
    pub allow_application_launch_activation: bool,
    /// Finite runtime session lifetime.
    pub ttl_seconds: u32,
}

impl AuthorityPolicy {
    /// Loads one regular, bounded authority ceiling as trusted installer input.
    ///
    /// The returned policy is not itself a runtime trust root. An integration
    /// plan must authenticate it and require human confirmation before a
    /// registered provider can grant any capability.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] when the file is unsafe, unreadable, too
    /// large, malformed, or outside the closed policy bounds.
    pub fn load_plan_input_file(path: &Path) -> Result<Self, AuthorityError> {
        Self::load_private_file(path)
    }

    /// Loads one regular, bounded unsigned development authority file.
    ///
    /// This is not a production trust root. Public launchers must use an
    /// authenticated [`AuthorityProvider`] and [`AuthorityGrant`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] when the file is unsafe, unreadable, too
    /// large, malformed, or outside the closed policy bounds.
    pub fn load_development_file(path: &Path) -> Result<Self, AuthorityError> {
        Self::load_private_file(path)
    }

    fn load_private_file(path: &Path) -> Result<Self, AuthorityError> {
        let metadata = std::fs::symlink_metadata(path).map_err(|source| AuthorityError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AuthorityError::UnsafeFile(
                "authority path must be a regular file, not a symlink".to_owned(),
            ));
        }
        if metadata.len() > MAX_AUTHORITY_BYTES {
            return Err(AuthorityError::UnsafeFile(
                "authority file exceeds the 64 KiB limit".to_owned(),
            ));
        }
        validate_file_permissions(&metadata)?;
        let bytes = std::fs::read(path).map_err(|source| AuthorityError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let policy: Self = serde_json::from_slice(&bytes)?;
        policy.validate()?;
        Ok(policy)
    }

    /// Validates the closed desktop authority ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] when identities, actions, modes, or lifetime
    /// exceed the product bounds or contain an incoherent authority grant.
    pub fn validate(&self) -> Result<(), AuthorityError> {
        if self.schema_version != AUTHORITY_SCHEMA_VERSION {
            return Err(AuthorityError::Invalid(
                "unsupported authority schema_version".to_owned(),
            ));
        }
        if (!self.allow_display_capture
            && !self.allow_clipboard_read
            && !self.allow_clipboard_write
            && self.application_ids.is_empty())
            || self.application_ids.len() > MAX_APPLICATIONS
        {
            return Err(AuthorityError::Invalid(
                "application_ids must contain 1 to 32 identities unless display or clipboard-only access is authorized"
                    .to_owned(),
            ));
        }
        if self.application_ids.iter().any(|application_id| {
            application_id.is_empty()
                || application_id != application_id.trim()
                || application_id.len() > MAX_APPLICATION_ID_BYTES
        }) {
            return Err(AuthorityError::Invalid(
                "application_ids must be normalized non-empty values of at most 512 bytes"
                    .to_owned(),
            ));
        }
        let unique_applications = self.application_ids.iter().collect::<HashSet<_>>();
        if unique_applications.len() != self.application_ids.len() {
            return Err(AuthorityError::Invalid(
                "application_ids contains duplicates".to_owned(),
            ));
        }
        if self.allowed_actions.len() > MAX_ACTIONS {
            return Err(AuthorityError::Invalid(
                "allowed_actions exceeds the supported bound".to_owned(),
            ));
        }
        let unique_actions = self.allowed_actions.iter().collect::<HashSet<_>>();
        if unique_actions.len() != self.allowed_actions.len() {
            return Err(AuthorityError::Invalid(
                "allowed_actions contains duplicates".to_owned(),
            ));
        }
        if self.ttl_seconds == 0 || self.ttl_seconds > MAX_TTL_SECONDS {
            return Err(AuthorityError::Invalid(
                "ttl_seconds must be between 1 and 3600".to_owned(),
            ));
        }
        if self.mode == PermissionMode::ReadOnly
            && (!self.allowed_actions.is_empty()
                || self.allow_foreground_input
                || self.restore_foreground_after_input
                || self.allow_clipboard_write
                || self.allow_application_launch)
        {
            return Err(AuthorityError::Invalid(
                "read_only authority cannot contain actions or foreground input".to_owned(),
            ));
        }
        if self.allow_clipboard_write && !self.allow_clipboard_read {
            return Err(AuthorityError::Invalid(
                "clipboard write authority requires clipboard read authority".to_owned(),
            ));
        }
        if self.allow_application_launch && self.application_ids.is_empty() {
            return Err(AuthorityError::Invalid(
                "application launch authority requires at least one application identity"
                    .to_owned(),
            ));
        }
        if self.allow_application_launch_activation && !self.allow_application_launch {
            return Err(AuthorityError::Invalid(
                "launch activation requires application launch authority".to_owned(),
            ));
        }
        if self.restore_foreground_after_input && !self.allow_foreground_input {
            return Err(AuthorityError::Invalid(
                "foreground restoration requires foreground input authority".to_owned(),
            ));
        }
        Ok(())
    }
}

/// One fresh adapter-instance challenge sent through an authenticated provider channel.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityChallenge {
    /// Non-secret host registration selected by the launcher.
    pub registration_id: String,
    /// Random identity for this exact adapter process instance.
    pub adapter_nonce: String,
    /// Runtime generation that will own every granted capability session.
    pub runtime_epoch: String,
    /// Optional remote worker generation selected by a trusted host.
    pub worker_epoch: Option<String>,
}

impl AuthorityChallenge {
    /// Creates a bounded challenge with a fresh adapter nonce.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] when a binding is empty, malformed, or too large.
    pub fn new(
        registration_id: impl Into<String>,
        runtime_epoch: impl Into<String>,
        worker_epoch: Option<String>,
    ) -> Result<Self, AuthorityError> {
        let challenge = Self {
            registration_id: registration_id.into(),
            adapter_nonce: format!("adapter_{}", Uuid::new_v4().simple()),
            runtime_epoch: runtime_epoch.into(),
            worker_epoch,
        };
        validate_binding(&challenge.registration_id, "registration_id")?;
        validate_binding(&challenge.adapter_nonce, "adapter_nonce")?;
        validate_binding(&challenge.runtime_epoch, "runtime_epoch")?;
        if let Some(worker_epoch) = challenge.worker_epoch.as_deref() {
            validate_binding(worker_epoch, "worker_epoch")?;
        }
        Ok(challenge)
    }
}

/// Authenticated provider response bound to one adapter challenge and short lifetime.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityGrant {
    /// Provider grant contract revision.
    pub schema_version: u32,
    /// Stable non-secret provider identity recorded in acceptance evidence.
    pub provider_id: String,
    /// Exact registration from the adapter challenge.
    pub registration_id: String,
    /// Exact nonce from the adapter challenge.
    pub adapter_nonce: String,
    /// Exact local runtime generation from the adapter challenge.
    pub runtime_epoch: String,
    /// Exact remote worker generation, when one was selected.
    pub worker_epoch: Option<String>,
    /// Authenticated host or user principal receiving the grant.
    pub principal: String,
    /// Provider-defined bounded work scope for this adapter instance.
    pub work_scope: String,
    /// Prefix required on every model-facing mutation request identity.
    pub request_namespace: String,
    /// Provider wall-clock issue time in Unix milliseconds.
    pub issued_at_unix_ms: u64,
    /// Provider wall-clock expiry time in Unix milliseconds.
    pub expires_at_unix_ms: u64,
    /// Exact desktop authority ceiling authenticated by the provider channel.
    pub authority: AuthorityPolicy,
}

impl fmt::Debug for AuthorityGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorityGrant")
            .field("provider_id", &self.provider_id)
            .field("registration_id", &self.registration_id)
            .field("runtime_epoch", &self.runtime_epoch)
            .field("worker_epoch", &self.worker_epoch)
            .field("expires_at_unix_ms", &self.expires_at_unix_ms)
            .field("grant", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl AuthorityGrant {
    /// Validates every binding and converts the grant into adapter authority.
    ///
    /// The authenticated provider transport must call this immediately after
    /// receiving the response. The returned session TTL never exceeds the
    /// remaining grant lifetime.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityError`] for replay, mismatched bindings, expiry, or
    /// authority values outside the closed product limits.
    pub fn resolve(
        mut self,
        challenge: &AuthorityChallenge,
        now_unix_ms: u64,
    ) -> Result<AuthorityContext, AuthorityError> {
        if self.schema_version != PROVIDER_GRANT_SCHEMA_VERSION {
            return Err(AuthorityError::InvalidGrant(
                "unsupported provider grant schema_version".to_owned(),
            ));
        }
        validate_binding(&self.provider_id, "provider_id")?;
        validate_binding(&self.principal, "principal")?;
        validate_binding(&self.work_scope, "work_scope")?;
        validate_request_namespace(&self.request_namespace)?;
        if self.registration_id != challenge.registration_id
            || self.adapter_nonce != challenge.adapter_nonce
            || self.runtime_epoch != challenge.runtime_epoch
            || self.worker_epoch != challenge.worker_epoch
        {
            return Err(AuthorityError::InvalidGrant(
                "provider grant does not match the adapter challenge".to_owned(),
            ));
        }
        if self.issued_at_unix_ms > now_unix_ms.saturating_add(MAX_CLOCK_SKEW_MS)
            || self.expires_at_unix_ms <= self.issued_at_unix_ms
            || self
                .expires_at_unix_ms
                .saturating_sub(self.issued_at_unix_ms)
                > u64::from(MAX_TTL_SECONDS) * 1_000
            || self.expires_at_unix_ms.saturating_sub(now_unix_ms) < MIN_REMAINING_GRANT_MS
        {
            return Err(AuthorityError::InvalidGrant(
                "provider grant timestamps are expired or outside the bounded lifetime".to_owned(),
            ));
        }
        self.authority.validate()?;
        let remaining_seconds =
            u32::try_from(self.expires_at_unix_ms.saturating_sub(now_unix_ms) / 1_000)
                .unwrap_or(MAX_TTL_SECONDS)
                .min(MAX_TTL_SECONDS);
        self.authority.ttl_seconds = self.authority.ttl_seconds.min(remaining_seconds);
        if self.authority.ttl_seconds == 0 {
            return Err(AuthorityError::InvalidGrant(
                "provider grant has no usable session lifetime".to_owned(),
            ));
        }
        Ok(AuthorityContext {
            policy: self.authority,
            provider: Some(AuthorityGrantMetadata {
                provider_id: self.provider_id,
                registration_id: self.registration_id,
                principal: self.principal,
                work_scope: self.work_scope,
                request_namespace: self.request_namespace,
                runtime_epoch: self.runtime_epoch,
                worker_epoch: self.worker_epoch,
                expires_at_unix_ms: self.expires_at_unix_ms,
            }),
            provider_client: None,
        })
    }
}

/// Non-secret provider bindings safe to return from an adapter status call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityGrantMetadata {
    /// Stable provider identity.
    pub provider_id: String,
    /// Non-secret host registration identity.
    pub registration_id: String,
    /// Authenticated host or user principal.
    pub principal: String,
    /// Provider-defined work scope.
    pub work_scope: String,
    /// Mutation request identity prefix for this adapter instance.
    pub request_namespace: String,
    /// Bound runtime generation.
    pub runtime_epoch: String,
    /// Bound remote worker generation, when present.
    pub worker_epoch: Option<String>,
    /// Absolute provider-grant expiry in Unix milliseconds.
    pub expires_at_unix_ms: u64,
}

/// Validated authority plus optional authenticated provider bindings.
#[derive(Clone)]
pub struct AuthorityContext {
    policy: AuthorityPolicy,
    provider: Option<AuthorityGrantMetadata>,
    provider_client: Option<Arc<dyn AuthorityProvider>>,
}

impl AuthorityContext {
    /// Marks an unsigned policy as an explicit source-build-only fixture.
    pub fn unsigned_development(policy: AuthorityPolicy) -> Self {
        Self {
            policy,
            provider: None,
            provider_client: None,
        }
    }

    /// Returns authenticated provider bindings, or `None` for an unsigned fixture.
    pub fn provider_metadata(&self) -> Option<&AuthorityGrantMetadata> {
        self.provider.as_ref()
    }

    /// Binds the authenticated provider to the running adapter.
    #[must_use]
    pub fn with_provider(mut self, provider: Arc<dyn AuthorityProvider>) -> Self {
        self.provider_client = Some(provider);
        self
    }

    /// Revalidates the provider grant and registration for a live adapter.
    ///
    /// # Errors
    ///
    /// Returns a provider error when the grant, registration, or authenticated
    /// provider channel is unavailable or no longer valid.
    pub async fn validate_provider(&self) -> Result<(), AuthorityProviderError> {
        match (&self.provider, &self.provider_client) {
            (Some(metadata), Some(provider)) => provider.validate(metadata).await,
            (Some(_), None) => Err(AuthorityProviderError::Unavailable),
            (None, _) => Ok(()),
        }
    }

    /// Requires a provider-bound request identity namespace when one exists.
    pub fn accepts_request_id(&self, request_id: &str) -> bool {
        self.provider.as_ref().is_none_or(|metadata| {
            request_id
                .strip_prefix(&metadata.request_namespace)
                .is_some_and(|suffix| suffix.starts_with(':') && suffix.len() > 1)
        })
    }
}

impl Deref for AuthorityContext {
    type Target = AuthorityPolicy;

    fn deref(&self) -> &Self::Target {
        &self.policy
    }
}

impl fmt::Debug for AuthorityContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorityContext")
            .field(
                "source",
                &if self.provider.is_some() {
                    "authenticated_provider"
                } else {
                    "unsigned_development"
                },
            )
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// Model-neutral provider interface; implementations authenticate their transport.
#[async_trait]
pub trait AuthorityProvider: Send + Sync {
    /// Acquires one challenge-bound grant without exposing credentials to the caller.
    async fn acquire(
        &self,
        challenge: AuthorityChallenge,
    ) -> Result<AuthorityGrant, AuthorityProviderError>;

    /// Revalidates an issued grant for a running adapter. Unsupported providers
    /// fail closed instead of silently keeping startup-only authority alive.
    async fn validate(
        &self,
        _grant: &AuthorityGrantMetadata,
    ) -> Result<(), AuthorityProviderError> {
        Err(AuthorityProviderError::Denied)
    }
}

/// Closed provider handoff failures that never contain grant or credential data.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AuthorityProviderError {
    /// Registered provider or protected channel is unavailable.
    #[error("registered authority provider is unavailable")]
    Unavailable,
    /// Provider authenticated the caller but denied this work scope.
    #[error("authority provider denied the adapter challenge")]
    Denied,
    /// Provider identity or protected-channel authentication failed.
    #[error("authority provider authentication failed")]
    AuthenticationFailed,
    /// Provider returned a malformed or unsupported bounded response.
    #[error("authority provider returned an invalid response")]
    InvalidResponse,
}

/// Returns current Unix milliseconds for provider grant validation.
///
/// # Errors
///
/// Returns [`AuthorityError`] if the system clock predates the Unix epoch.
pub fn current_unix_ms() -> Result<u64, AuthorityError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthorityError::InvalidGrant("system clock predates Unix epoch".to_owned()))?
        .as_millis();
    u64::try_from(milliseconds)
        .map_err(|_| AuthorityError::InvalidGrant("system time is out of range".to_owned()))
}

fn validate_binding(value: &str, field: &str) -> Result<(), AuthorityError> {
    if value.is_empty()
        || value.len() > MAX_BINDING_BYTES
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        return Err(AuthorityError::InvalidGrant(format!(
            "{field} is empty, unnormalized, or exceeds the bounded size"
        )));
    }
    Ok(())
}

fn validate_request_namespace(value: &str) -> Result<(), AuthorityError> {
    if value.len() < 16
        || value.len() > MAX_REQUEST_NAMESPACE_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AuthorityError::InvalidGrant(
            "request_namespace must be 16 to 64 normalized ASCII bytes".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_file_permissions(metadata: &std::fs::Metadata) -> Result<(), AuthorityError> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(AuthorityError::UnsafeFile(
            "authority file must not be writable by group or other users".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_file_permissions(_metadata: &std::fs::Metadata) -> Result<(), AuthorityError> {
    Ok(())
}

/// Authority loading failure that never includes file contents.
#[derive(Debug, Error)]
pub enum AuthorityError {
    /// Filesystem operation failed.
    #[error("failed to read authority file {}: {source}", path.display())]
    Io {
        /// Requested authority path.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// JSON did not match the closed authority schema.
    #[error("authority JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    /// File type, size, or permissions were unsafe.
    #[error("unsafe authority file: {0}")]
    UnsafeFile(String),
    /// Parsed values exceeded authority bounds.
    #[error("invalid authority policy: {0}")]
    Invalid(String),
    /// Authenticated provider refused or failed the bounded handoff.
    #[error(transparent)]
    Provider(#[from] AuthorityProviderError),
    /// Provider grant failed closed binding or lifetime validation.
    #[error("invalid authority provider grant: {0}")]
    InvalidGrant(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> AuthorityPolicy {
        AuthorityPolicy {
            schema_version: 1,
            mode: PermissionMode::Bounded,
            application_ids: vec!["com.example.Fixture".to_owned()],
            allowed_actions: vec![ActionKind::ClickPoint],
            allow_foreground_input: true,
            restore_foreground_after_input: true,
            allow_display_capture: true,
            allow_clipboard_read: false,
            allow_clipboard_write: false,
            allow_application_launch: true,
            allow_application_launch_activation: true,
            ttl_seconds: 300,
        }
    }

    #[test]
    fn bounded_policy_accepts_exact_normalized_values() {
        assert!(policy().validate().is_ok());
    }

    #[test]
    fn read_only_policy_cannot_smuggle_mutation_authority() {
        let mut policy = policy();
        policy.mode = PermissionMode::ReadOnly;
        assert!(policy.validate().is_err());
    }

    #[test]
    fn duplicate_application_identity_is_rejected() {
        let mut policy = policy();
        policy
            .application_ids
            .push("com.example.Fixture".to_owned());
        assert!(policy.validate().is_err());
    }

    #[test]
    fn read_only_display_authority_does_not_require_an_application_identity() {
        let mut policy = policy();
        policy.mode = PermissionMode::ReadOnly;
        policy.application_ids.clear();
        policy.allowed_actions.clear();
        policy.allow_foreground_input = false;
        policy.restore_foreground_after_input = false;
        policy.allow_application_launch = false;
        policy.allow_application_launch_activation = false;
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn clipboard_only_authority_does_not_require_an_application_identity() {
        let mut policy = policy();
        policy.application_ids.clear();
        policy.allowed_actions.clear();
        policy.allow_foreground_input = false;
        policy.restore_foreground_after_input = false;
        policy.allow_display_capture = false;
        policy.allow_clipboard_read = true;
        policy.allow_clipboard_write = true;
        policy.allow_application_launch = false;
        policy.allow_application_launch_activation = false;
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn clipboard_write_requires_read_authority() {
        let mut policy = policy();
        policy.allow_clipboard_write = true;
        assert!(policy.validate().is_err());
    }

    fn grant(challenge: &AuthorityChallenge, now: u64) -> AuthorityGrant {
        AuthorityGrant {
            schema_version: 1,
            provider_id: "test-provider".to_owned(),
            registration_id: challenge.registration_id.clone(),
            adapter_nonce: challenge.adapter_nonce.clone(),
            runtime_epoch: challenge.runtime_epoch.clone(),
            worker_epoch: challenge.worker_epoch.clone(),
            principal: "test-principal".to_owned(),
            work_scope: "test-work-scope".to_owned(),
            request_namespace: "test_namespace_0123456789".to_owned(),
            issued_at_unix_ms: now,
            expires_at_unix_ms: now + 60_000,
            authority: policy(),
        }
    }

    #[test]
    fn provider_grant_binds_nonce_epoch_scope_and_request_namespace() {
        let challenge =
            AuthorityChallenge::new("registration-1", "runtime-1", Some("worker-1".to_owned()))
                .unwrap();
        let now = 1_000_000;
        let context = grant(&challenge, now).resolve(&challenge, now).unwrap();
        assert_eq!(
            context.provider_metadata().unwrap().work_scope,
            "test-work-scope"
        );
        assert!(context.accepts_request_id("test_namespace_0123456789:mutation-1"));
        assert!(!context.accepts_request_id("other:mutation-1"));
        assert_eq!(context.ttl_seconds, 60);
    }

    #[test]
    fn provider_grant_rejects_replay_and_expiry() {
        let challenge = AuthorityChallenge::new("registration-1", "runtime-1", None).unwrap();
        let other = AuthorityChallenge::new("registration-1", "runtime-1", None).unwrap();
        let now = 1_000_000;
        assert!(grant(&challenge, now).resolve(&other, now).is_err());
        assert!(
            grant(&challenge, now)
                .resolve(&challenge, now + 60_000)
                .is_err()
        );
    }

    #[test]
    fn grant_debug_redacts_scope_and_authority() {
        let challenge = AuthorityChallenge::new("registration-1", "runtime-1", None).unwrap();
        let debug = format!("{:?}", grant(&challenge, 1_000_000));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("test-work-scope"));
        assert!(!debug.contains("com.example.Fixture"));
    }
}
