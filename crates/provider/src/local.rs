//! Challenge-bound grant issuance for authenticated local registrations.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nexus_cua_mcp::{
    AuthorityChallenge, AuthorityGrant, AuthorityGrantMetadata, AuthorityProvider,
    AuthorityProviderError, current_unix_ms,
};
use nexus_cua_platform::HostExecutableIdentity;
use nexus_cua_protocol::AuthorizationToken;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::registration::{HostIdentitySource, RegistrationError, RegistrationStore};

const PROVIDER_GRANT_SCHEMA_VERSION: u32 = 1;
const MAX_ACTIVE_CHALLENGES: usize = 1_024;
const GRANT_RENEWAL_THRESHOLD_MS: u64 = 5_000;

/// Production source for the exact parent executable that launched the adapter.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemHostIdentitySource;

impl HostIdentitySource for SystemHostIdentitySource {
    fn current(&self) -> Result<HostExecutableIdentity, RegistrationError> {
        nexus_cua_platform::parent_host_identity()
            .map_err(|_| RegistrationError::AuthenticationFailed)
    }
}

/// Local authority provider backed by a MAC-authenticated registration and OS secret storage.
pub struct RegisteredAuthorityProvider {
    registrations: Arc<RegistrationStore>,
    host_identity: Arc<dyn HostIdentitySource>,
    active_challenges: Mutex<HashMap<String, ActiveGrant>>,
}

struct ActiveGrant {
    grant: AuthorityGrant,
    registration_digest: [u8; 32],
}

impl RegisteredAuthorityProvider {
    /// Creates a provider that verifies the real adapter parent executable.
    pub fn new(registrations: Arc<RegistrationStore>) -> Self {
        Self::with_host_identity(registrations, Arc::new(SystemHostIdentitySource))
    }

    /// Creates a provider with an injected identity source for conformance testing.
    pub fn with_host_identity(
        registrations: Arc<RegistrationStore>,
        host_identity: Arc<dyn HostIdentitySource>,
    ) -> Self {
        Self {
            registrations,
            host_identity,
            active_challenges: Mutex::new(HashMap::new()),
        }
    }

    fn admit_challenge(
        &self,
        challenge: &AuthorityChallenge,
        now_unix_ms: u64,
        grant: AuthorityGrant,
        registration_digest: [u8; 32],
    ) -> Result<(), AuthorityProviderError> {
        let mut active = self
            .active_challenges
            .lock()
            .map_err(|_| AuthorityProviderError::Unavailable)?;
        active.retain(|_, value| value.grant.expires_at_unix_ms > now_unix_ms);
        let key = format!("{}:{}", challenge.registration_id, challenge.adapter_nonce);
        if active.contains_key(&key) {
            return Err(AuthorityProviderError::Denied);
        }
        if active.len() >= MAX_ACTIVE_CHALLENGES {
            return Err(AuthorityProviderError::Unavailable);
        }
        active.insert(
            key,
            ActiveGrant {
                grant,
                registration_digest,
            },
        );
        Ok(())
    }

    /// Derives an HTTP adapter capability only after authenticating the real parent host.
    ///
    /// # Errors
    ///
    /// Fails closed for a host mismatch, a non-HTTP registration, or protected-state failure.
    pub fn http_authorization(
        &self,
        registration_id: &str,
    ) -> Result<AuthorizationToken, RegistrationError> {
        self.authenticate_host(registration_id)?;
        self.registrations.http_authorization(registration_id)
    }

    /// Derives an internal CLI-service capability only after authenticating the real parent host.
    ///
    /// # Errors
    ///
    /// Fails closed for a host mismatch, a non-CLI registration, or protected-state failure.
    pub fn cli_authorization(
        &self,
        registration_id: &str,
    ) -> Result<AuthorizationToken, RegistrationError> {
        self.authenticate_host(registration_id)?;
        self.registrations.cli_authorization(registration_id)
    }

    fn authenticate_host(&self, registration_id: &str) -> Result<(), RegistrationError> {
        let registration = self.registrations.load(registration_id)?;
        let actual_identity = self.host_identity.current()?;
        if same_identity(&registration.request.host_identity, &actual_identity) {
            Ok(())
        } else {
            Err(RegistrationError::AuthenticationFailed)
        }
    }
}

#[async_trait]
impl AuthorityProvider for RegisteredAuthorityProvider {
    async fn acquire(
        &self,
        challenge: AuthorityChallenge,
    ) -> Result<AuthorityGrant, AuthorityProviderError> {
        let registration = self
            .registrations
            .load(&challenge.registration_id)
            .map_err(|error| map_registration_error(&error))?;
        let actual_identity = self
            .host_identity
            .current()
            .map_err(|error| map_registration_error(&error))?;
        if !same_identity(&registration.request.host_identity, &actual_identity) {
            return Err(AuthorityProviderError::AuthenticationFailed);
        }
        let now_unix_ms = current_unix_ms().map_err(|_| AuthorityProviderError::Unavailable)?;
        let lifetime_ms = u64::from(registration.request.authority.ttl_seconds) * 1_000;
        let expires_at_unix_ms = now_unix_ms.saturating_add(lifetime_ms);
        let registration_digest = registration_digest(&registration.request)?;
        let grant = AuthorityGrant {
            schema_version: PROVIDER_GRANT_SCHEMA_VERSION,
            provider_id: registration.request.provider_id,
            registration_id: challenge.registration_id.clone(),
            adapter_nonce: challenge.adapter_nonce.clone(),
            runtime_epoch: challenge.runtime_epoch.clone(),
            worker_epoch: challenge.worker_epoch.clone(),
            principal: registration.request.principal,
            work_scope: registration.request.work_scope,
            request_namespace: format!("ns_{}", Uuid::new_v4().simple()),
            issued_at_unix_ms: now_unix_ms,
            expires_at_unix_ms,
            authority: registration.request.authority,
        };
        self.admit_challenge(&challenge, now_unix_ms, grant.clone(), registration_digest)?;
        Ok(grant)
    }

    async fn validate(
        &self,
        metadata: &AuthorityGrantMetadata,
    ) -> Result<(), AuthorityProviderError> {
        let now = current_unix_ms().map_err(|_| AuthorityProviderError::Unavailable)?;
        let expected_digest = {
            let active = self
                .active_challenges
                .lock()
                .map_err(|_| AuthorityProviderError::Unavailable)?;
            let issued = active
                .values()
                .find(|value| value.grant.request_namespace == metadata.request_namespace)
                .ok_or(AuthorityProviderError::Denied)?;
            let grant = &issued.grant;
            if now >= grant.expires_at_unix_ms
                || grant.registration_id != metadata.registration_id
                || grant.provider_id != metadata.provider_id
                || grant.principal != metadata.principal
                || grant.work_scope != metadata.work_scope
                || grant.runtime_epoch != metadata.runtime_epoch
                || grant.worker_epoch != metadata.worker_epoch
            {
                return Err(AuthorityProviderError::Denied);
            }
            issued.registration_digest
        };
        let registration = self
            .registrations
            .load(&metadata.registration_id)
            .map_err(|error| map_registration_error(&error))?;
        if registration_digest(&registration.request)? != expected_digest {
            return Err(AuthorityProviderError::AuthenticationFailed);
        }
        // Renew only the same admitted challenge, after rechecking the
        // authenticated registration. The request namespace and runtime/worker
        // bindings remain unchanged; removal or tampering fails above.
        let mut active = self
            .active_challenges
            .lock()
            .map_err(|_| AuthorityProviderError::Unavailable)?;
        let issued = active
            .values_mut()
            .find(|value| value.grant.request_namespace == metadata.request_namespace)
            .ok_or(AuthorityProviderError::Denied)?;
        if issued.grant.expires_at_unix_ms.saturating_sub(now) <= GRANT_RENEWAL_THRESHOLD_MS {
            let ttl_ms = u64::from(registration.request.authority.ttl_seconds) * 1_000;
            issued.grant.issued_at_unix_ms = now;
            issued.grant.expires_at_unix_ms = now.saturating_add(ttl_ms);
        }
        Ok(())
    }
}

fn registration_digest(
    request: &crate::RegistrationRequest,
) -> Result<[u8; 32], AuthorityProviderError> {
    let encoded =
        serde_json::to_vec(request).map_err(|_| AuthorityProviderError::InvalidResponse)?;
    Ok(Sha256::digest(encoded).into())
}

fn same_identity(expected: &HostExecutableIdentity, actual: &HostExecutableIdentity) -> bool {
    let Ok(expected) = serde_json::to_vec(expected) else {
        return false;
    };
    let Ok(actual) = serde_json::to_vec(actual) else {
        return false;
    };
    expected.len() == actual.len() && bool::from(expected.ct_eq(&actual))
}

fn map_registration_error(error: &RegistrationError) -> AuthorityProviderError {
    match error {
        RegistrationError::NotFound
        | RegistrationError::Secret(crate::SecretStoreError::NotFound) => {
            AuthorityProviderError::Unavailable
        }
        RegistrationError::AuthenticationFailed => AuthorityProviderError::AuthenticationFailed,
        RegistrationError::AlreadyExists
        | RegistrationError::CapacityExceeded
        | RegistrationError::Invalid(_)
        | RegistrationError::Io(_)
        | RegistrationError::Json(_)
        | RegistrationError::Secret(_) => AuthorityProviderError::InvalidResponse,
    }
}
