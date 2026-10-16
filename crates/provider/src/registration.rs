//! Authenticated owner-private host registration storage.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nexus_cua_mcp::AuthorityPolicy;
use nexus_cua_platform::HostExecutableIdentity;
use nexus_cua_protocol::AuthorizationToken;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::secret::{SecretStore, SecretStoreError, SystemSecretStore};

const REGISTRATION_SCHEMA_VERSION: u32 = 1;
const MAX_REGISTRATION_BYTES: u64 = 64 * 1024;
const MIN_REGISTRATION_ID_BYTES: usize = 16;
const MAX_REGISTRATION_ID_BYTES: usize = 64;
const MAX_BINDING_BYTES: usize = 256;
const MAX_REGISTRATIONS: usize = 128;
const MAX_STATE_ROOT_ENTRIES: usize = 512;
const REGISTRATION_LOCK_FILE: &str = ".registration.lock";

/// MCP transport fixed by one host registration.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRegisteredTransport {
    /// Host launches one adapter over standard input/output.
    Stdio,
    /// Host connects to the current Streamable HTTP endpoint.
    StreamableHttp,
}

/// Exactly one model-facing adapter binding selected at installation time.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "binding", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegisteredAdapterProfile {
    /// MCP binding with one immutable transport and tool profile.
    Mcp {
        /// Transport selected by the trusted integration plan.
        transport: McpRegisteredTransport,
        /// Canonical closed MCP tool profile (`full` or `vision`).
        tool_profile: String,
    },
    /// CLI binding used by a trusted scoped broker.
    Cli,
}

/// Trusted input installed after an interactive or administrator-approved plan.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationRequest {
    /// Non-secret opaque registration identifier.
    pub registration_id: String,
    /// Stable non-secret provider identity.
    pub provider_id: String,
    /// Exact product version installed for this registration.
    pub product_version: String,
    /// Single adapter binding exposed to the registered host.
    pub adapter_profile: RegisteredAdapterProfile,
    /// Exact executable identity approved by the user or administrator.
    pub host_identity: HostExecutableIdentity,
    /// Authenticated local principal label.
    pub principal: String,
    /// Bounded provider-defined work scope.
    pub work_scope: String,
    /// Maximum desktop authority that this registration may grant.
    pub authority: AuthorityPolicy,
}

impl RegistrationRequest {
    fn validate(&self) -> Result<(), RegistrationError> {
        validate_registration_id(&self.registration_id)?;
        validate_binding(&self.provider_id, "provider_id")?;
        validate_binding(&self.product_version, "product_version")?;
        validate_binding(&self.principal, "principal")?;
        validate_binding(&self.work_scope, "work_scope")?;
        if let RegisteredAdapterProfile::Mcp { tool_profile, .. } = &self.adapter_profile
            && !matches!(tool_profile.as_str(), "full" | "vision")
        {
            return Err(RegistrationError::Invalid(
                "MCP tool_profile must be full or vision".to_owned(),
            ));
        }
        validate_digest(&self.host_identity.path_sha256, "host path digest")?;
        validate_digest(
            &self.host_identity.executable_sha256,
            "host executable digest",
        )?;
        self.authority
            .validate()
            .map_err(|error| RegistrationError::Invalid(error.to_string()))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistrationPayload {
    pub(crate) schema_version: u32,
    pub(crate) installed_at_unix_ms: u64,
    pub(crate) request: RegistrationRequest,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthenticatedRegistration {
    payload: RegistrationPayload,
    mac_sha256: String,
}

/// Non-sensitive registration metadata suitable for status and removal flows.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationSummary {
    /// Non-secret opaque registration identifier.
    pub registration_id: String,
    /// Stable non-secret provider identity.
    pub provider_id: String,
    /// Exact installed product version.
    pub product_version: String,
    /// Single adapter binding selected for the host.
    pub adapter_profile: RegisteredAdapterProfile,
    /// Registration creation time in Unix milliseconds.
    pub installed_at_unix_ms: u64,
    /// Maximum lifetime of each provider grant issued from this registration.
    pub authority_ttl_seconds: u32,
}

/// Source of the currently launching host identity, replaceable in tests.
pub trait HostIdentitySource: Send + Sync {
    /// Returns the exact executable identity that launched the adapter.
    ///
    /// # Errors
    ///
    /// Fails closed when the parent process cannot be resolved or its exact
    /// executable identity cannot be authenticated.
    fn current(&self) -> Result<HostExecutableIdentity, RegistrationError>;
}

/// Owner-private registration files plus an OS-protected secret store.
pub struct RegistrationStore {
    root: PathBuf,
    secrets: Arc<dyn SecretStore>,
}

impl RegistrationStore {
    /// Opens system-backed registration storage below an owner-private root.
    ///
    /// # Errors
    ///
    /// Fails when the root cannot be made owner-private or platform credential
    /// storage is unavailable.
    pub fn system(root: &Path) -> Result<Self, RegistrationError> {
        crate::private_fs::secure_directory(root).map_err(RegistrationError::Io)?;
        let secrets = Arc::new(SystemSecretStore::new(root)?);
        Ok(Self {
            root: root.to_path_buf(),
            secrets,
        })
    }

    /// Creates storage with an injected protected secret implementation.
    ///
    /// This constructor supports deterministic conformance providers without
    /// weakening the system store used by production launchers.
    ///
    /// # Errors
    ///
    /// Fails when the registration root cannot be made owner-private.
    pub fn with_secret_store(
        root: &Path,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self, RegistrationError> {
        crate::private_fs::secure_directory(root).map_err(RegistrationError::Io)?;
        Ok(Self {
            root: root.to_path_buf(),
            secrets,
        })
    }

    /// Installs one new authenticated registration without overwriting state.
    ///
    /// # Errors
    ///
    /// Fails closed for invalid input, collisions, credential-store failures,
    /// or a partial owner-private filesystem write.
    pub fn install(
        &self,
        request: RegistrationRequest,
        now_unix_ms: u64,
    ) -> Result<RegistrationSummary, RegistrationError> {
        request.validate()?;
        let payload = RegistrationPayload {
            schema_version: REGISTRATION_SCHEMA_VERSION,
            installed_at_unix_ms: now_unix_ms,
            request,
        };
        let payload_bytes = serde_json::to_vec(&payload).map_err(RegistrationError::Json)?;
        if u64::try_from(payload_bytes.len()).unwrap_or(u64::MAX) > MAX_REGISTRATION_BYTES {
            return Err(RegistrationError::Invalid(
                "registration exceeds the 64 KiB bound".to_owned(),
            ));
        }

        let mut secret = new_secret();
        let mac_sha256 = authenticate(&secret, &payload_bytes)?;
        let registration = AuthenticatedRegistration {
            payload,
            mac_sha256,
        };
        let mut bytes =
            serde_json::to_vec_pretty(&registration).map_err(RegistrationError::Json)?;
        bytes.push(b'\n');
        let registration_id = registration.payload.request.registration_id.clone();
        let path = self.path(&registration_id)?;
        let _lock = self.lock_exclusive()?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Err(RegistrationError::AlreadyExists),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(RegistrationError::Io(error)),
        }
        if !crate::private_fs::has_json_capacity(
            &self.root,
            MAX_REGISTRATIONS,
            MAX_STATE_ROOT_ENTRIES,
        )
        .map_err(RegistrationError::Io)?
        {
            return Err(RegistrationError::CapacityExceeded);
        }

        self.secrets.put(&registration_id, &secret)?;
        let result = (|| {
            let mut file = crate::private_fs::create_private_file(&path)
                .map_err(map_create_registration_error)?;
            file.write_all(&bytes).map_err(RegistrationError::Io)?;
            file.sync_all().map_err(RegistrationError::Io)
        })();
        secret.zeroize();
        if let Err(error) = result {
            let _ = crate::private_fs::remove_private_file(&path);
            let _ = self.secrets.delete(&registration_id);
            return Err(error);
        }
        Ok(summary(&registration.payload))
    }

    /// Authenticates and returns non-sensitive registration status.
    ///
    /// # Errors
    ///
    /// Fails when the file or protected secret is missing, malformed, unsafe,
    /// or has an invalid MAC.
    pub fn inspect(&self, registration_id: &str) -> Result<RegistrationSummary, RegistrationError> {
        self.load(registration_id).map(|payload| summary(&payload))
    }

    /// Derives a host-only bearer capability for a registered HTTP adapter.
    ///
    /// The capability is deterministic for the protected registration secret,
    /// never persisted in the registration file, and never exposed by status.
    ///
    /// # Errors
    ///
    /// Fails when the registration is missing, unauthenticated, or not bound
    /// to Streamable HTTP.
    pub(crate) fn http_authorization(
        &self,
        registration_id: &str,
    ) -> Result<AuthorizationToken, RegistrationError> {
        let payload = self.load(registration_id)?;
        if !matches!(
            payload.request.adapter_profile,
            RegisteredAdapterProfile::Mcp {
                transport: McpRegisteredTransport::StreamableHttp,
                ..
            }
        ) {
            return Err(RegistrationError::Invalid(
                "registration is not bound to Streamable HTTP".to_owned(),
            ));
        }
        let secret = self.secrets.get(registration_id)?;
        let token = message_authentication(
            &secret,
            b"nexus.cua.mcp.http.authorization.v1\0",
            registration_id.as_bytes(),
        )?;
        Ok(AuthorizationToken::new(token))
    }

    /// Derives the internal transport capability for a CLI-bound service.
    pub(crate) fn cli_authorization(
        &self,
        registration_id: &str,
    ) -> Result<AuthorizationToken, RegistrationError> {
        let payload = self.load(registration_id)?;
        if payload.request.adapter_profile != RegisteredAdapterProfile::Cli {
            return Err(RegistrationError::Invalid(
                "registration is not bound to the CLI adapter".to_owned(),
            ));
        }
        let secret = self.secrets.get(registration_id)?;
        let token = message_authentication(
            &secret,
            b"nexus.cua.cli.transport.authorization.v1\0",
            registration_id.as_bytes(),
        )?;
        Ok(AuthorizationToken::new(token))
    }

    /// Removes the authority-bearing registration before deleting its secret.
    ///
    /// # Errors
    ///
    /// Fails if owner-private state cannot be removed. A missing registration
    /// file is treated as already revoked.
    pub fn remove(&self, registration_id: &str) -> Result<(), RegistrationError> {
        let path = self.path(registration_id)?;
        let _lock = self.lock_exclusive()?;
        crate::private_fs::remove_private_file(&path).map_err(RegistrationError::Io)?;
        self.secrets.delete(registration_id)?;
        Ok(())
    }

    pub(crate) fn load(
        &self,
        registration_id: &str,
    ) -> Result<RegistrationPayload, RegistrationError> {
        let path = self.path(registration_id)?;
        let _lock = self.lock_shared()?;
        crate::private_fs::validate_private_regular_file(&path, MAX_REGISTRATION_BYTES)
            .map_err(map_read_registration_error)?;
        let bytes = std::fs::read(path).map_err(map_read_registration_error)?;
        let registration: AuthenticatedRegistration =
            serde_json::from_slice(&bytes).map_err(RegistrationError::Json)?;
        if registration.payload.schema_version != REGISTRATION_SCHEMA_VERSION
            || registration.payload.request.registration_id != registration_id
        {
            return Err(RegistrationError::AuthenticationFailed);
        }
        registration.payload.request.validate()?;
        let payload_bytes =
            serde_json::to_vec(&registration.payload).map_err(RegistrationError::Json)?;
        let secret = self.secrets.get(registration_id)?;
        let expected = authenticate(&secret, &payload_bytes)?;
        let expected = expected.as_bytes();
        let candidate = registration.mac_sha256.as_bytes();
        if expected.len() != candidate.len() || !bool::from(expected.ct_eq(candidate)) {
            return Err(RegistrationError::AuthenticationFailed);
        }
        Ok(registration.payload)
    }

    fn path(&self, registration_id: &str) -> Result<PathBuf, RegistrationError> {
        validate_registration_id(registration_id)?;
        Ok(self.root.join(format!("{registration_id}.json")))
    }

    fn lock_exclusive(&self) -> Result<std::fs::File, RegistrationError> {
        let lock = crate::private_fs::open_private_lock(&self.root.join(REGISTRATION_LOCK_FILE))
            .map_err(RegistrationError::Io)?;
        fs2::FileExt::lock_exclusive(&lock).map_err(RegistrationError::Io)?;
        Ok(lock)
    }

    fn lock_shared(&self) -> Result<std::fs::File, RegistrationError> {
        let lock = crate::private_fs::open_private_lock(&self.root.join(REGISTRATION_LOCK_FILE))
            .map_err(RegistrationError::Io)?;
        fs2::FileExt::lock_shared(&lock).map_err(RegistrationError::Io)?;
        Ok(lock)
    }
}

fn summary(payload: &RegistrationPayload) -> RegistrationSummary {
    RegistrationSummary {
        registration_id: payload.request.registration_id.clone(),
        provider_id: payload.request.provider_id.clone(),
        product_version: payload.request.product_version.clone(),
        adapter_profile: payload.request.adapter_profile.clone(),
        installed_at_unix_ms: payload.installed_at_unix_ms,
        authority_ttl_seconds: payload.request.authority.ttl_seconds,
    }
}

pub(crate) fn new_secret() -> Zeroizing<Vec<u8>> {
    let mut value = Vec::with_capacity(32);
    value.extend_from_slice(Uuid::new_v4().as_bytes());
    value.extend_from_slice(Uuid::new_v4().as_bytes());
    Zeroizing::new(value)
}

fn authenticate(secret: &[u8], payload: &[u8]) -> Result<String, RegistrationError> {
    message_authentication(secret, b"nexus.cua.registration.v1\0", payload)
}

pub(crate) fn message_authentication(
    secret: &[u8],
    domain: &[u8],
    payload: &[u8],
) -> Result<String, RegistrationError> {
    if secret.len() != 32 {
        return Err(RegistrationError::Secret(SecretStoreError::InvalidSecret));
    }
    let mut inner_key = [0x36_u8; 64];
    let mut outer_key = [0x5c_u8; 64];
    for (index, byte) in secret.iter().enumerate() {
        inner_key[index] ^= byte;
        outer_key[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_key);
    inner.update(domain);
    inner.update(payload);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_key);
    outer.update(inner_digest);
    inner_key.zeroize();
    outer_key.zeroize();
    Ok(hex::encode(outer.finalize()))
}

/// Returns the owner-private system state root without consulting model input.
///
/// # Errors
///
/// Fails closed when the supported operating-system profile has no absolute
/// per-user application-data directory.
pub fn default_state_root() -> Result<PathBuf, RegistrationError> {
    #[cfg(target_os = "macos")]
    let root = Some(
        PathBuf::from(objc2_foundation::NSHomeDirectory().to_string())
            .join("Library")
            .join("Application Support")
            .join("Nexus Computer Use"),
    );
    #[cfg(target_os = "windows")]
    let root = windows_local_state_root()?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let root: Option<PathBuf> = None;
    root.filter(|path| path.is_absolute()).ok_or_else(|| {
        RegistrationError::Invalid(
            "owner-private system application-data root is unavailable".to_owned(),
        )
    })
}

/// Returns the absolute current-user profile directory from the operating system.
///
/// This is used only to resolve documented per-user integration locations. It
/// does not consult model-controlled environment variables.
///
/// # Errors
///
/// Fails closed when the supported operating system cannot resolve a profile.
pub fn default_user_home() -> Result<PathBuf, RegistrationError> {
    #[cfg(target_os = "macos")]
    let root = Some(PathBuf::from(
        objc2_foundation::NSHomeDirectory().to_string(),
    ));
    #[cfg(target_os = "windows")]
    let root = windows_known_folder(&windows::Win32::UI::Shell::FOLDERID_Profile)?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let root: Option<PathBuf> = None;
    root.filter(|path| path.is_absolute()).ok_or_else(|| {
        RegistrationError::Invalid("current-user profile directory is unavailable".to_owned())
    })
}

#[cfg(target_os = "windows")]
fn windows_local_state_root() -> Result<Option<PathBuf>, RegistrationError> {
    windows_known_folder(&windows::Win32::UI::Shell::FOLDERID_LocalAppData)
        .map(|root| root.map(|path| path.join("Nexus Research").join("Nexus Computer Use")))
}

#[cfg(target_os = "windows")]
fn windows_known_folder(
    folder_id: &windows::core::GUID,
) -> Result<Option<PathBuf>, RegistrationError> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    let value =
        unsafe { SHGetKnownFolderPath(folder_id, KF_FLAG_DEFAULT, None) }.map_err(|_| {
            RegistrationError::Invalid("current-user known folder is unavailable".to_owned())
        })?;
    let text = unsafe { value.to_string() }
        .map_err(|_| RegistrationError::Invalid("current-user known folder is invalid".to_owned()));
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    Ok(Some(PathBuf::from(text?)))
}

pub(crate) fn validate_registration_id(value: &str) -> Result<(), RegistrationError> {
    if !(MIN_REGISTRATION_ID_BYTES..=MAX_REGISTRATION_ID_BYTES).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(RegistrationError::Invalid(
            "registration_id must be 16 to 64 normalized ASCII bytes".to_owned(),
        ));
    }
    Ok(())
}

fn validate_binding(value: &str, field: &str) -> Result<(), RegistrationError> {
    if value.is_empty()
        || value.len() > MAX_BINDING_BYTES
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        return Err(RegistrationError::Invalid(format!(
            "{field} is empty, unnormalized, or exceeds the bounded size"
        )));
    }
    Ok(())
}

fn validate_digest(value: &str, field: &str) -> Result<(), RegistrationError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RegistrationError::Invalid(format!(
            "{field} must be one SHA-256 hex digest"
        )));
    }
    Ok(())
}

pub(crate) fn map_create_registration_error(error: std::io::Error) -> RegistrationError {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        RegistrationError::AlreadyExists
    } else {
        RegistrationError::Io(error)
    }
}

pub(crate) fn map_read_registration_error(error: std::io::Error) -> RegistrationError {
    if error.kind() == std::io::ErrorKind::NotFound {
        RegistrationError::NotFound
    } else {
        RegistrationError::Io(error)
    }
}

/// Authenticated registration lifecycle failure without secrets or host paths.
#[derive(Debug, Error)]
pub enum RegistrationError {
    /// A registration with the same opaque identity already exists.
    #[error("registration already exists")]
    AlreadyExists,
    /// The bounded owner-private state store has no free entry.
    #[error("registered authority state capacity is exhausted")]
    CapacityExceeded,
    /// No registration exists for the supplied opaque identity.
    #[error("registration was not found")]
    NotFound,
    /// Closed input validation rejected the registration.
    #[error("invalid registration: {0}")]
    Invalid(String),
    /// The registration MAC or exact host binding did not authenticate.
    #[error("registration authentication failed")]
    AuthenticationFailed,
    /// Owner-private registration state failed.
    #[error("registration filesystem operation failed: {0}")]
    Io(#[source] std::io::Error),
    /// Registration JSON failed its closed schema.
    #[error("registration JSON is invalid: {0}")]
    Json(#[source] serde_json::Error),
    /// Platform-protected secret storage failed.
    #[error(transparent)]
    Secret(#[from] SecretStoreError),
}
