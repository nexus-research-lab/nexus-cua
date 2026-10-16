//! Registered, model-neutral local authority provider.
//!
//! Registration content is authenticated with a secret stored by the
//! operating system, and every grant revalidates the exact parent-host
//! executable identity before binding a fresh adapter challenge.

mod local;
mod plan;
mod private_fs;
mod registration;
mod secret;

pub use local::{RegisteredAuthorityProvider, SystemHostIdentitySource};
pub use plan::AuthenticatedIntegrationStore;
pub use registration::{
    HostIdentitySource, McpRegisteredTransport, RegisteredAdapterProfile, RegistrationError,
    RegistrationRequest, RegistrationStore, RegistrationSummary, default_state_root,
    default_user_home,
};
pub use secret::{SecretStore, SecretStoreError, SystemSecretStore};
