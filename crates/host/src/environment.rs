//! Truthful desktop environment policy and optional lifecycle provider boundary.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Mutex;

/// Physical isolation profile behind one desktop worker.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    /// The current user's real interactive desktop.
    LocalUserDesktop,
    /// A distinct physical or logged-in desktop reached through a trusted host.
    RemoteDesktop,
    /// A provider-owned disposable VM or container desktop.
    Isolated,
}

/// Direct network enforcement below the application/UI layer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkPolicy {
    /// No claim is made about application or host network behavior.
    NotEnforced,
    /// The environment blocks direct network access.
    DenyAll,
    /// Only normalized host names in this bounded set are admitted.
    AllowHosts {
        /// DNS host names; ports and credentials are deliberately absent.
        hosts: Vec<String>,
    },
    /// The isolated provider explicitly permits unrestricted networking.
    Unrestricted,
}

/// One provider-defined mount capability without exposing a host path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MountPolicy {
    /// Opaque mount name resolved only by the environment provider.
    pub mount_ref: String,
    /// Whether writes below this mount are blocked by the provider.
    pub read_only: bool,
}

/// Direct filesystem enforcement below the application/UI layer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum FilesystemPolicy {
    /// No claim is made about host filesystem behavior. GUI-visible state is
    /// still visible through separately authorized applications.
    NotEnforced,
    /// No direct filesystem capability is mounted into the environment.
    NoDirectAccess,
    /// Only the provider-resolved opaque mounts are present.
    Mounts {
        /// Bounded provider-owned mount capabilities.
        mounts: Vec<MountPolicy>,
    },
}

/// One machine-lifecycle or lower-layer policy capability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCapability {
    /// Network policy is enforced below the application layer.
    NetworkPolicy,
    /// Filesystem policy is enforced below the application layer.
    FilesystemPolicy,
    /// A provider can capture immutable machine snapshots.
    Snapshot,
    /// A provider can reset the machine to a bound snapshot.
    Reset,
    /// A provider can clone a new independent machine from a snapshot.
    Clone,
}

/// Immutable environment description fixed for one worker epoch.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDescriptor {
    /// Physical environment class.
    pub kind: EnvironmentKind,
    /// Direct network policy, distinct from an application's own UI behavior.
    pub network: NetworkPolicy,
    /// Direct filesystem policy, distinct from GUI-visible files.
    pub filesystem: FilesystemPolicy,
    /// Truthful provider capabilities.
    pub capabilities: Vec<EnvironmentCapability>,
    /// Immutable base image digest for isolated environments.
    pub base_image_sha256: Option<String>,
}

impl EnvironmentDescriptor {
    /// Lightweight profile for the user's current real desktop.
    pub fn local_user_desktop() -> Self {
        Self {
            kind: EnvironmentKind::LocalUserDesktop,
            network: NetworkPolicy::NotEnforced,
            filesystem: FilesystemPolicy::NotEnforced,
            capabilities: Vec::new(),
            base_image_sha256: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), EnvironmentError> {
        if self.kind != EnvironmentKind::Isolated
            && (self.capabilities.contains(&EnvironmentCapability::Snapshot)
                || self.capabilities.contains(&EnvironmentCapability::Reset)
                || self.capabilities.contains(&EnvironmentCapability::Clone)
                || self.base_image_sha256.is_some())
        {
            return Err(EnvironmentError::Invalid(
                "only isolated environments may advertise image lifecycle".to_owned(),
            ));
        }
        if self
            .capabilities
            .contains(&EnvironmentCapability::NetworkPolicy)
            == matches!(self.network, NetworkPolicy::NotEnforced)
            || self
                .capabilities
                .contains(&EnvironmentCapability::FilesystemPolicy)
                == matches!(self.filesystem, FilesystemPolicy::NotEnforced)
        {
            return Err(EnvironmentError::Invalid(
                "policy capability flags must match their enforcement modes".to_owned(),
            ));
        }
        if self.capabilities.iter().collect::<HashSet<_>>().len() != self.capabilities.len() {
            return Err(EnvironmentError::Invalid(
                "environment capabilities contain duplicates".to_owned(),
            ));
        }
        if let NetworkPolicy::AllowHosts { hosts } = &self.network {
            if hosts.is_empty() || hosts.len() > 64 {
                return Err(EnvironmentError::Invalid(
                    "network allowlist must contain between 1 and 64 hosts".to_owned(),
                ));
            }
            let unique = hosts.iter().collect::<HashSet<_>>();
            if unique.len() != hosts.len()
                || hosts.iter().any(|host| {
                    host.is_empty()
                        || host.len() > 253
                        || host != &host.to_ascii_lowercase()
                        || host.starts_with('.')
                        || host.ends_with('.')
                        || host.split('.').any(|label| {
                            label.is_empty()
                                || label.len() > 63
                                || label.starts_with('-')
                                || label.ends_with('-')
                                || !label.bytes().all(|byte| {
                                    byte.is_ascii_lowercase()
                                        || byte.is_ascii_digit()
                                        || byte == b'-'
                                })
                        })
                })
            {
                return Err(EnvironmentError::Invalid(
                    "network allowlist hosts must be unique normalized DNS names".to_owned(),
                ));
            }
        }
        if let FilesystemPolicy::Mounts { mounts } = &self.filesystem {
            if mounts.is_empty() || mounts.len() > 32 {
                return Err(EnvironmentError::Invalid(
                    "filesystem policy must contain between 1 and 32 mounts".to_owned(),
                ));
            }
            let unique = mounts
                .iter()
                .map(|mount| mount.mount_ref.as_str())
                .collect::<HashSet<_>>();
            if unique.len() != mounts.len()
                || mounts.iter().any(|mount| {
                    mount.mount_ref.is_empty()
                        || mount.mount_ref.len() > 128
                        || mount.mount_ref.trim() != mount.mount_ref
                })
            {
                return Err(EnvironmentError::Invalid(
                    "mount references must be unique, normalized, and bounded".to_owned(),
                ));
            }
        }
        if self.kind == EnvironmentKind::Isolated {
            let Some(digest) = &self.base_image_sha256 else {
                return Err(EnvironmentError::Invalid(
                    "isolated environments require a base image digest".to_owned(),
                ));
            };
            if !valid_sha256(digest) {
                return Err(EnvironmentError::Invalid(
                    "base image digest must be lowercase SHA-256 hex".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Provider-owned immutable machine snapshot descriptor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSnapshot {
    /// Opaque provider snapshot capability.
    pub snapshot_ref: String,
    /// Worker identity whose exact epoch was captured.
    pub source_worker_id: String,
    /// Exact source worker epoch.
    pub source_worker_epoch: String,
    /// Immutable base image digest.
    pub base_image_sha256: String,
    /// RFC 3339 creation time.
    pub created_at: String,
}

/// Result of a provider reset or clone operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionedEnvironment {
    /// Opaque provider environment capability.
    pub environment_ref: String,
    /// Fresh provider generation after reset or clone.
    pub generation: String,
    /// Snapshot used as the immutable source.
    pub snapshot_ref: String,
    /// Immutable base image digest.
    pub base_image_sha256: String,
}

/// Optional provider operations. The local desktop implementation returns
/// `Unsupported` for every machine-lifecycle method.
#[async_trait]
pub trait EnvironmentProvider: Send + Sync {
    /// Truthful immutable provider descriptor.
    fn descriptor(&self) -> &EnvironmentDescriptor;

    /// Capture one provider-owned snapshot bound to worker epoch and base image.
    async fn snapshot(
        &self,
        worker_id: &str,
        worker_epoch: &str,
        label: &str,
    ) -> Result<MachineSnapshot, EnvironmentError>;

    /// Reset one provider environment to the exact snapshot.
    async fn reset(
        &self,
        environment_ref: &str,
        snapshot: &MachineSnapshot,
    ) -> Result<ProvisionedEnvironment, EnvironmentError>;

    /// Clone an independent provider environment from the exact snapshot.
    async fn clone_environment(
        &self,
        snapshot: &MachineSnapshot,
    ) -> Result<ProvisionedEnvironment, EnvironmentError>;
}

/// Lightweight local desktop provider with truthful unsupported lifecycle.
pub struct LocalDesktopEnvironment {
    descriptor: EnvironmentDescriptor,
}

/// Hard retention and in-flight bounds around an environment provider.
#[derive(Clone, Copy, Debug)]
pub struct EnvironmentManagerConfig {
    /// Maximum retained immutable snapshots.
    pub max_snapshots: usize,
    /// Maximum retained clone environment capabilities.
    pub max_clones: usize,
    /// Maximum normalized snapshot label bytes.
    pub max_label_bytes: usize,
}

impl Default for EnvironmentManagerConfig {
    fn default() -> Self {
        Self {
            max_snapshots: 32,
            max_clones: 64,
            max_label_bytes: 128,
        }
    }
}

#[derive(Default)]
struct EnvironmentManagerState {
    snapshots: HashMap<String, MachineSnapshot>,
    clones: HashMap<String, ProvisionedEnvironment>,
    pending_snapshots: usize,
    pending_clones: usize,
}

/// Bounded snapshot/reset/clone coordinator above one optional provider.
pub struct EnvironmentManager {
    provider: Arc<dyn EnvironmentProvider>,
    config: EnvironmentManagerConfig,
    state: Mutex<EnvironmentManagerState>,
}

impl EnvironmentManager {
    /// Wraps a provider with hard capacity and identity validation.
    ///
    /// # Errors
    ///
    /// Returns an error when bounds are zero or the provider descriptor is
    /// internally inconsistent.
    pub fn new(
        provider: Arc<dyn EnvironmentProvider>,
        config: EnvironmentManagerConfig,
    ) -> Result<Self, EnvironmentError> {
        if config.max_snapshots == 0 || config.max_clones == 0 || config.max_label_bytes == 0 {
            return Err(EnvironmentError::Invalid(
                "environment manager bounds must be non-zero".to_owned(),
            ));
        }
        provider.descriptor().validate()?;
        Ok(Self {
            provider,
            config,
            state: Mutex::new(EnvironmentManagerState::default()),
        })
    }

    /// Captures and retains one immutable snapshot bound to worker epoch and
    /// base-image digest. Capacity is reserved before the provider call.
    ///
    /// # Errors
    ///
    /// Returns unsupported, busy, invalid-provider, or provider errors.
    pub async fn snapshot(
        &self,
        worker_id: &str,
        worker_epoch: &str,
        label: &str,
    ) -> Result<MachineSnapshot, EnvironmentError> {
        require_capability(self.provider.descriptor(), EnvironmentCapability::Snapshot)?;
        validate_opaque(worker_id, "worker identity", 128)?;
        validate_opaque(worker_epoch, "worker epoch", 128)?;
        validate_opaque(label, "snapshot label", self.config.max_label_bytes)?;
        {
            let mut state = self.state.lock().await;
            if state.snapshots.len() + state.pending_snapshots >= self.config.max_snapshots {
                return Err(EnvironmentError::Busy);
            }
            state.pending_snapshots += 1;
        }
        let result = self.provider.snapshot(worker_id, worker_epoch, label).await;
        let mut state = self.state.lock().await;
        state.pending_snapshots = state.pending_snapshots.saturating_sub(1);
        let snapshot = result?;
        validate_snapshot(
            self.provider.descriptor(),
            worker_id,
            worker_epoch,
            &snapshot,
        )?;
        if state.snapshots.contains_key(&snapshot.snapshot_ref) {
            return Err(EnvironmentError::Invalid(
                "provider reused a live snapshot reference".to_owned(),
            ));
        }
        state
            .snapshots
            .insert(snapshot.snapshot_ref.clone(), snapshot.clone());
        Ok(snapshot)
    }

    /// Resets one provider environment to a retained exact snapshot.
    ///
    /// # Errors
    ///
    /// Returns unsupported, stale snapshot, invalid-provider, or provider
    /// errors.
    pub async fn reset(
        &self,
        environment_ref: &str,
        snapshot_ref: &str,
    ) -> Result<ProvisionedEnvironment, EnvironmentError> {
        require_capability(self.provider.descriptor(), EnvironmentCapability::Reset)?;
        validate_opaque(environment_ref, "environment reference", 128)?;
        let snapshot = self.retained_snapshot(snapshot_ref).await?;
        let result = self.provider.reset(environment_ref, &snapshot).await?;
        validate_provisioned(self.provider.descriptor(), &snapshot, &result)?;
        if result.environment_ref != environment_ref {
            return Err(EnvironmentError::Invalid(
                "provider reset changed the target environment reference".to_owned(),
            ));
        }
        Ok(result)
    }

    /// Clones one independent environment from a retained exact snapshot.
    /// Capacity is reserved before the provider call.
    ///
    /// # Errors
    ///
    /// Returns unsupported, busy, stale snapshot, invalid-provider, or provider
    /// errors.
    pub async fn clone_environment(
        &self,
        snapshot_ref: &str,
    ) -> Result<ProvisionedEnvironment, EnvironmentError> {
        require_capability(self.provider.descriptor(), EnvironmentCapability::Clone)?;
        let snapshot = self.retained_snapshot(snapshot_ref).await?;
        {
            let mut state = self.state.lock().await;
            if state.clones.len() + state.pending_clones >= self.config.max_clones {
                return Err(EnvironmentError::Busy);
            }
            state.pending_clones += 1;
        }
        let result = self.provider.clone_environment(&snapshot).await;
        let mut state = self.state.lock().await;
        state.pending_clones = state.pending_clones.saturating_sub(1);
        let clone = result?;
        validate_provisioned(self.provider.descriptor(), &snapshot, &clone)?;
        if state.clones.contains_key(&clone.environment_ref) {
            return Err(EnvironmentError::Invalid(
                "provider reused a live clone environment reference".to_owned(),
            ));
        }
        state
            .clones
            .insert(clone.environment_ref.clone(), clone.clone());
        Ok(clone)
    }

    /// Releases one retained clone capability after the product has destroyed
    /// or detached the provider environment.
    pub async fn release_clone(&self, environment_ref: &str) -> bool {
        self.state
            .lock()
            .await
            .clones
            .remove(environment_ref)
            .is_some()
    }

    /// Removes a retained snapshot only when no retained clone still names it.
    ///
    /// # Errors
    ///
    /// Returns busy while a retained clone depends on the snapshot.
    pub async fn remove_snapshot(&self, snapshot_ref: &str) -> Result<bool, EnvironmentError> {
        let mut state = self.state.lock().await;
        if state
            .clones
            .values()
            .any(|clone| clone.snapshot_ref == snapshot_ref)
        {
            return Err(EnvironmentError::Busy);
        }
        Ok(state.snapshots.remove(snapshot_ref).is_some())
    }

    async fn retained_snapshot(
        &self,
        snapshot_ref: &str,
    ) -> Result<MachineSnapshot, EnvironmentError> {
        validate_opaque(snapshot_ref, "snapshot reference", 128)?;
        self.state
            .lock()
            .await
            .snapshots
            .get(snapshot_ref)
            .cloned()
            .ok_or(EnvironmentError::StaleSnapshot)
    }
}

impl Default for LocalDesktopEnvironment {
    fn default() -> Self {
        Self {
            descriptor: EnvironmentDescriptor::local_user_desktop(),
        }
    }
}

#[async_trait]
impl EnvironmentProvider for LocalDesktopEnvironment {
    fn descriptor(&self) -> &EnvironmentDescriptor {
        &self.descriptor
    }

    async fn snapshot(
        &self,
        _worker_id: &str,
        _worker_epoch: &str,
        _label: &str,
    ) -> Result<MachineSnapshot, EnvironmentError> {
        Err(EnvironmentError::Unsupported)
    }

    async fn reset(
        &self,
        _environment_ref: &str,
        _snapshot: &MachineSnapshot,
    ) -> Result<ProvisionedEnvironment, EnvironmentError> {
        Err(EnvironmentError::Unsupported)
    }

    async fn clone_environment(
        &self,
        _snapshot: &MachineSnapshot,
    ) -> Result<ProvisionedEnvironment, EnvironmentError> {
        Err(EnvironmentError::Unsupported)
    }
}

/// Environment validation or provider failure.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum EnvironmentError {
    /// Descriptor or request is invalid.
    #[error("invalid desktop environment: {0}")]
    Invalid(String),
    /// The selected environment does not implement machine lifecycle.
    #[error("machine lifecycle is unsupported by this desktop environment")]
    Unsupported,
    /// A configured snapshot/clone capacity is exhausted; nothing was queued.
    #[error("environment lifecycle capacity is busy")]
    Busy,
    /// Snapshot capability expired or was not retained by this manager.
    #[error("machine snapshot capability is stale")]
    StaleSnapshot,
    /// The provider failed without exposing credentials or host paths.
    #[error("environment provider failed: {0}")]
    Provider(String),
}

fn require_capability(
    descriptor: &EnvironmentDescriptor,
    capability: EnvironmentCapability,
) -> Result<(), EnvironmentError> {
    if descriptor.capabilities.contains(&capability) {
        Ok(())
    } else {
        Err(EnvironmentError::Unsupported)
    }
}

fn validate_opaque(value: &str, label: &str, max_bytes: usize) -> Result<(), EnvironmentError> {
    if value.is_empty() || value.len() > max_bytes || value.trim() != value {
        Err(EnvironmentError::Invalid(format!(
            "{label} is empty, too long, or not normalized"
        )))
    } else {
        Ok(())
    }
}

fn validate_snapshot(
    descriptor: &EnvironmentDescriptor,
    worker_id: &str,
    worker_epoch: &str,
    snapshot: &MachineSnapshot,
) -> Result<(), EnvironmentError> {
    validate_opaque(&snapshot.snapshot_ref, "snapshot reference", 128)?;
    if snapshot.source_worker_id != worker_id
        || snapshot.source_worker_epoch != worker_epoch
        || descriptor.base_image_sha256.as_deref() != Some(&snapshot.base_image_sha256)
        || !valid_sha256(&snapshot.base_image_sha256)
        || OffsetDateTime::parse(&snapshot.created_at, &Rfc3339).is_err()
    {
        return Err(EnvironmentError::Invalid(
            "provider snapshot identity does not match worker epoch and base image".to_owned(),
        ));
    }
    Ok(())
}

fn validate_provisioned(
    descriptor: &EnvironmentDescriptor,
    snapshot: &MachineSnapshot,
    provisioned: &ProvisionedEnvironment,
) -> Result<(), EnvironmentError> {
    validate_opaque(&provisioned.environment_ref, "environment reference", 128)?;
    validate_opaque(&provisioned.generation, "environment generation", 128)?;
    if provisioned.snapshot_ref != snapshot.snapshot_ref
        || provisioned.base_image_sha256 != snapshot.base_image_sha256
        || descriptor.base_image_sha256.as_deref() != Some(&provisioned.base_image_sha256)
    {
        return Err(EnvironmentError::Invalid(
            "provider environment does not match the retained snapshot and base image".to_owned(),
        ));
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct IsolatedProvider {
        descriptor: EnvironmentDescriptor,
        clones: AtomicUsize,
    }

    impl IsolatedProvider {
        fn new() -> Self {
            Self {
                descriptor: EnvironmentDescriptor {
                    kind: EnvironmentKind::Isolated,
                    network: NetworkPolicy::DenyAll,
                    filesystem: FilesystemPolicy::NoDirectAccess,
                    capabilities: vec![
                        EnvironmentCapability::NetworkPolicy,
                        EnvironmentCapability::FilesystemPolicy,
                        EnvironmentCapability::Snapshot,
                        EnvironmentCapability::Reset,
                        EnvironmentCapability::Clone,
                    ],
                    base_image_sha256: Some("a".repeat(64)),
                },
                clones: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl EnvironmentProvider for IsolatedProvider {
        fn descriptor(&self) -> &EnvironmentDescriptor {
            &self.descriptor
        }

        async fn snapshot(
            &self,
            worker_id: &str,
            worker_epoch: &str,
            _label: &str,
        ) -> Result<MachineSnapshot, EnvironmentError> {
            Ok(MachineSnapshot {
                snapshot_ref: "snapshot_fixture".to_owned(),
                source_worker_id: worker_id.to_owned(),
                source_worker_epoch: worker_epoch.to_owned(),
                base_image_sha256: "a".repeat(64),
                created_at: "2026-08-27T00:00:00Z".to_owned(),
            })
        }

        async fn reset(
            &self,
            environment_ref: &str,
            snapshot: &MachineSnapshot,
        ) -> Result<ProvisionedEnvironment, EnvironmentError> {
            Ok(ProvisionedEnvironment {
                environment_ref: environment_ref.to_owned(),
                generation: "generation_reset".to_owned(),
                snapshot_ref: snapshot.snapshot_ref.clone(),
                base_image_sha256: snapshot.base_image_sha256.clone(),
            })
        }

        async fn clone_environment(
            &self,
            snapshot: &MachineSnapshot,
        ) -> Result<ProvisionedEnvironment, EnvironmentError> {
            let clone = self.clones.fetch_add(1, Ordering::SeqCst);
            Ok(ProvisionedEnvironment {
                environment_ref: format!("environment_clone_{clone}"),
                generation: format!("generation_clone_{clone}"),
                snapshot_ref: snapshot.snapshot_ref.clone(),
                base_image_sha256: snapshot.base_image_sha256.clone(),
            })
        }
    }

    #[tokio::test]
    async fn local_desktop_never_claims_machine_lifecycle() {
        let provider = LocalDesktopEnvironment::default();
        provider.descriptor().validate().unwrap();
        assert_eq!(
            provider.snapshot("worker", "epoch", "checkpoint").await,
            Err(EnvironmentError::Unsupported)
        );
    }

    #[test]
    fn real_desktop_cannot_advertise_snapshot_or_direct_policy_enforcement() {
        let mut invalid = EnvironmentDescriptor::local_user_desktop();
        invalid.capabilities.push(EnvironmentCapability::Snapshot);
        assert!(invalid.validate().is_err());

        let mut mismatched = EnvironmentDescriptor::local_user_desktop();
        mismatched.network = NetworkPolicy::DenyAll;
        assert!(mismatched.validate().is_err());
    }

    #[tokio::test]
    async fn manager_bounds_and_revalidates_snapshot_reset_clone() {
        let manager = EnvironmentManager::new(
            Arc::new(IsolatedProvider::new()),
            EnvironmentManagerConfig {
                max_snapshots: 1,
                max_clones: 1,
                max_label_bytes: 32,
            },
        )
        .unwrap();
        let snapshot = manager
            .snapshot("worker_fixture", "epoch_fixture", "prepared")
            .await
            .unwrap();
        let reset = manager
            .reset("environment_original", &snapshot.snapshot_ref)
            .await
            .unwrap();
        assert_eq!(reset.environment_ref, "environment_original");
        let clone = manager
            .clone_environment(&snapshot.snapshot_ref)
            .await
            .unwrap();
        assert_eq!(
            manager.clone_environment(&snapshot.snapshot_ref).await,
            Err(EnvironmentError::Busy)
        );
        assert_eq!(
            manager.remove_snapshot(&snapshot.snapshot_ref).await,
            Err(EnvironmentError::Busy)
        );
        assert!(manager.release_clone(&clone.environment_ref).await);
        assert!(
            manager
                .remove_snapshot(&snapshot.snapshot_ref)
                .await
                .unwrap()
        );
    }
}
