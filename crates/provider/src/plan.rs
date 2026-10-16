//! Authenticated bounded storage for integration plans and receipts.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use zeroize::{Zeroize as _, Zeroizing};

use crate::registration::{
    RegistrationError, message_authentication, new_secret, validate_registration_id,
};
use crate::{SecretStore, SystemSecretStore};

const STATE_ENVELOPE_SCHEMA_VERSION: u32 = 1;
const MAX_STATE_PAYLOAD_BYTES: usize = 128 * 1024;
const MAX_STATE_ENVELOPE_BYTES: u64 = 320 * 1024;
const MAX_INTEGRATION_STATES: usize = 512;
const MAX_INTEGRATION_DIRECTORY_ENTRIES: usize = 640;
const INTEGRATION_STATE_LOCK_FILE: &str = ".integration-state.lock";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthenticatedState {
    schema_version: u32,
    payload_hex: String,
    mac_sha256: String,
}

/// Owner-private, OS-secret-authenticated integration state storage.
pub struct AuthenticatedIntegrationStore {
    root: PathBuf,
    secrets: Arc<dyn SecretStore>,
}

impl AuthenticatedIntegrationStore {
    /// Opens the production integration state store below the application state root.
    ///
    /// # Errors
    ///
    /// Fails closed when the directory or platform secret store is unavailable.
    pub fn system(state_root: &Path) -> Result<Self, RegistrationError> {
        let root = state_root.join("integration-state");
        crate::private_fs::secure_directory(&root).map_err(RegistrationError::Io)?;
        let secrets = Arc::new(SystemSecretStore::new(&root)?);
        Ok(Self { root, secrets })
    }

    /// Creates an authenticated state store with an injected protected secret backend.
    ///
    /// # Errors
    ///
    /// Fails when the state root cannot be made owner-private.
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

    /// Stores one create-new payload authenticated by an OS-protected secret.
    ///
    /// # Errors
    ///
    /// Rejects invalid identities, oversized state, collisions, and partial writes.
    pub fn install(&self, state_ref: &str, payload: &[u8]) -> Result<(), RegistrationError> {
        validate_registration_id(state_ref)?;
        if payload.is_empty() || payload.len() > MAX_STATE_PAYLOAD_BYTES {
            return Err(RegistrationError::Invalid(
                "integration state payload is empty or exceeds 128 KiB".to_owned(),
            ));
        }
        let secret_id = secret_id(state_ref);
        let mut secret = new_secret();
        let envelope = AuthenticatedState {
            schema_version: STATE_ENVELOPE_SCHEMA_VERSION,
            payload_hex: hex::encode(payload),
            mac_sha256: message_authentication(
                &secret,
                b"nexus.cua.integration-state.v1\0",
                payload,
            )?,
        };
        let mut bytes = serde_json::to_vec_pretty(&envelope).map_err(RegistrationError::Json)?;
        bytes.push(b'\n');
        let path = self.path(state_ref)?;
        let _lock = self.lock_exclusive()?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Err(RegistrationError::AlreadyExists),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(RegistrationError::Io(error)),
        }
        if !crate::private_fs::has_json_capacity(
            &self.root,
            MAX_INTEGRATION_STATES,
            MAX_INTEGRATION_DIRECTORY_ENTRIES,
        )
        .map_err(RegistrationError::Io)?
        {
            return Err(RegistrationError::CapacityExceeded);
        }
        self.secrets.put(&secret_id, &secret).map_err(|error| {
            if matches!(error, crate::SecretStoreError::AlreadyExists) {
                RegistrationError::AlreadyExists
            } else {
                RegistrationError::Secret(error)
            }
        })?;
        let result = (|| {
            let mut file = crate::private_fs::create_private_file(&path)
                .map_err(super::registration::map_create_registration_error)?;
            file.write_all(&bytes).map_err(RegistrationError::Io)?;
            file.sync_all().map_err(RegistrationError::Io)
        })();
        secret.zeroize();
        if let Err(error) = result {
            let _ = crate::private_fs::remove_private_file(&path);
            let _ = self.secrets.delete(&secret_id);
            return Err(error);
        }
        Ok(())
    }

    /// Loads and authenticates one bounded payload into zeroizing memory.
    ///
    /// # Errors
    ///
    /// Rejects missing, malformed, tampered, unsafe, or oversized state.
    pub fn load(&self, state_ref: &str) -> Result<Zeroizing<Vec<u8>>, RegistrationError> {
        let path = self.path(state_ref)?;
        let _lock = self.lock_shared()?;
        crate::private_fs::validate_private_regular_file(&path, MAX_STATE_ENVELOPE_BYTES)
            .map_err(super::registration::map_read_registration_error)?;
        let bytes =
            std::fs::read(path).map_err(super::registration::map_read_registration_error)?;
        let envelope: AuthenticatedState =
            serde_json::from_slice(&bytes).map_err(RegistrationError::Json)?;
        if envelope.schema_version != STATE_ENVELOPE_SCHEMA_VERSION {
            return Err(RegistrationError::AuthenticationFailed);
        }
        let mut payload = hex::decode(envelope.payload_hex)
            .map_err(|_| RegistrationError::AuthenticationFailed)?;
        if payload.is_empty() || payload.len() > MAX_STATE_PAYLOAD_BYTES {
            payload.zeroize();
            return Err(RegistrationError::AuthenticationFailed);
        }
        let secret = self.secrets.get(&secret_id(state_ref))?;
        let expected =
            message_authentication(&secret, b"nexus.cua.integration-state.v1\0", &payload)?;
        if expected.len() != envelope.mac_sha256.len()
            || !bool::from(expected.as_bytes().ct_eq(envelope.mac_sha256.as_bytes()))
        {
            payload.zeroize();
            return Err(RegistrationError::AuthenticationFailed);
        }
        Ok(Zeroizing::new(payload))
    }

    /// Removes the state file before deleting its authentication secret.
    ///
    /// # Errors
    ///
    /// Fails when either owner-private file or protected secret removal fails.
    pub fn remove(&self, state_ref: &str) -> Result<(), RegistrationError> {
        let path = self.path(state_ref)?;
        let _lock = self.lock_exclusive()?;
        crate::private_fs::remove_private_file(&path).map_err(RegistrationError::Io)?;
        self.secrets.delete(&secret_id(state_ref))?;
        Ok(())
    }

    fn path(&self, state_ref: &str) -> Result<PathBuf, RegistrationError> {
        validate_registration_id(state_ref)?;
        Ok(self.root.join(format!("{state_ref}.json")))
    }

    fn lock_exclusive(&self) -> Result<std::fs::File, RegistrationError> {
        let lock =
            crate::private_fs::open_private_lock(&self.root.join(INTEGRATION_STATE_LOCK_FILE))
                .map_err(RegistrationError::Io)?;
        fs2::FileExt::lock_exclusive(&lock).map_err(RegistrationError::Io)?;
        Ok(lock)
    }

    fn lock_shared(&self) -> Result<std::fs::File, RegistrationError> {
        let lock =
            crate::private_fs::open_private_lock(&self.root.join(INTEGRATION_STATE_LOCK_FILE))
                .map_err(RegistrationError::Io)?;
        fs2::FileExt::lock_shared(&lock).map_err(RegistrationError::Io)?;
        Ok(lock)
    }
}

fn secret_id(state_ref: &str) -> String {
    format!("integration-state-{state_ref}")
}
