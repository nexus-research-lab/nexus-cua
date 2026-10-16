//! Bounded desktop-worker registration, placement, lease, and epoch affinity.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nexus_cua_protocol::{
    Command, CuaError, Platform, RequestId, ResponseEnvelope, ScreenshotArtifact,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Mutex;
use tokio::time::Instant;
use uuid::Uuid;

use crate::{EnvironmentCapability, EnvironmentDescriptor, EnvironmentError, EnvironmentKind};

macro_rules! opaque_host_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub(crate) fn random(prefix: &str) -> Self {
                Self(format!("{prefix}_{}", Uuid::new_v4().simple()))
            }

            /// Returns the opaque identifier as a string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

opaque_host_id!(WorkerId, "Opaque desktop-worker identity.");
opaque_host_id!(WorkerEpoch, "Opaque worker health epoch.");
opaque_host_id!(WorkerLeaseId, "Opaque finite worker lease capability.");

/// One independently schedulable desktop runtime endpoint.
#[async_trait]
pub trait DesktopWorker: Send + Sync {
    /// Native operating system hosting this worker.
    fn platform(&self) -> Platform;

    /// Immutable environment descriptor for this endpoint generation.
    fn environment(&self) -> &EnvironmentDescriptor;

    /// Dispatch one idempotent core protocol request.
    async fn dispatch(
        &self,
        request_id: RequestId,
        timeout_ms: u32,
        command: Command,
    ) -> ResponseEnvelope;

    /// Read one runtime-owned artifact after exact metadata revalidation.
    async fn read_artifact(
        &self,
        artifact: &ScreenshotArtifact,
        max_bytes: u64,
    ) -> Result<Vec<u8>, CuaError>;
}

/// Public worker state fixed for one registration epoch.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDescriptor {
    /// Opaque registry identity.
    pub worker_id: WorkerId,
    /// Fresh health epoch invalidating all prior leases on reconnect.
    pub epoch: WorkerEpoch,
    /// Native operating system.
    pub platform: Platform,
    /// Truthful environment and policy enforcement profile.
    pub environment: EnvironmentDescriptor,
    /// Maximum simultaneous host leases for this desktop runtime.
    pub max_leases: usize,
    /// Current active lease count.
    pub active_leases: usize,
}

/// Capability-aware worker placement request.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequirements {
    /// Optional exact native platform.
    pub platform: Option<Platform>,
    /// Optional environment class.
    pub environment_kind: Option<EnvironmentKind>,
    /// Every lower-layer capability required from the selected environment.
    pub capabilities: Vec<EnvironmentCapability>,
    /// Optional exact worker affinity for reconciliation or continued work.
    pub worker_id: Option<WorkerId>,
}

impl WorkerRequirements {
    fn matches(&self, descriptor: &WorkerDescriptor) -> bool {
        self.platform
            .is_none_or(|value| value == descriptor.platform)
            && self
                .environment_kind
                .is_none_or(|value| value == descriptor.environment.kind)
            && self
                .capabilities
                .iter()
                .all(|capability| descriptor.environment.capabilities.contains(capability))
            && self
                .worker_id
                .as_ref()
                .is_none_or(|value| value == &descriptor.worker_id)
    }
}

/// Exact worker/epoch affinity capability with a finite lifetime.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerLease {
    /// Opaque lease capability.
    pub lease_id: WorkerLeaseId,
    /// Exact worker selected by bounded placement.
    pub worker_id: WorkerId,
    /// Exact health epoch; reconnect/reset invalidates it.
    pub worker_epoch: WorkerEpoch,
    /// RFC 3339 host wall-clock projection of the monotonic expiry.
    pub expires_at: String,
}

/// Registry resource and lifetime bounds.
#[derive(Clone, Debug)]
pub struct WorkerRegistryConfig {
    /// Maximum registered worker endpoints.
    pub max_workers: usize,
    /// Maximum simultaneous leases across the registry.
    pub max_leases: usize,
    /// Maximum lease lifetime.
    pub max_lease_ttl: Duration,
}

impl Default for WorkerRegistryConfig {
    fn default() -> Self {
        Self {
            max_workers: 32,
            max_leases: 128,
            max_lease_ttl: Duration::from_secs(60 * 60),
        }
    }
}

struct WorkerEntry {
    descriptor: WorkerDescriptor,
    endpoint: Arc<dyn DesktopWorker>,
}

struct LeaseEntry {
    lease: WorkerLease,
    expires_at: Instant,
}

#[derive(Default)]
struct RegistryState {
    workers: HashMap<WorkerId, WorkerEntry>,
    leases: HashMap<WorkerLeaseId, LeaseEntry>,
}

/// Bounded fail-fast scheduler for independent OS desktop workers.
pub struct WorkerRegistry {
    config: WorkerRegistryConfig,
    state: Mutex<RegistryState>,
}

impl WorkerRegistry {
    /// Creates a registry with non-zero hard resource bounds.
    ///
    /// # Errors
    ///
    /// Returns an error when any bound is zero.
    pub fn new(config: WorkerRegistryConfig) -> Result<Self, WorkerError> {
        if config.max_workers == 0 || config.max_leases == 0 || config.max_lease_ttl.is_zero() {
            return Err(WorkerError::Invalid(
                "worker registry bounds must be non-zero".to_owned(),
            ));
        }
        Ok(Self {
            config,
            state: Mutex::new(RegistryState::default()),
        })
    }

    /// Registers one endpoint under a fresh worker identity and health epoch.
    ///
    /// # Errors
    ///
    /// Returns fail-fast capacity or descriptor validation errors.
    pub async fn register(
        &self,
        endpoint: Arc<dyn DesktopWorker>,
        max_leases: usize,
    ) -> Result<WorkerDescriptor, WorkerError> {
        if max_leases == 0 || max_leases > self.config.max_leases {
            return Err(WorkerError::Invalid(
                "worker lease capacity is outside registry bounds".to_owned(),
            ));
        }
        endpoint.environment().validate()?;
        let mut state = self.state.lock().await;
        purge_expired(&mut state);
        if state.workers.len() >= self.config.max_workers {
            return Err(WorkerError::Busy);
        }
        let descriptor = WorkerDescriptor {
            worker_id: WorkerId::random("worker"),
            epoch: WorkerEpoch::random("epoch"),
            platform: endpoint.platform(),
            environment: endpoint.environment().clone(),
            max_leases,
            active_leases: 0,
        };
        state.workers.insert(
            descriptor.worker_id.clone(),
            WorkerEntry {
                descriptor: descriptor.clone(),
                endpoint,
            },
        );
        Ok(descriptor)
    }

    /// Removes a worker and invalidates every lease from its epoch.
    pub async fn unregister(&self, worker_id: &WorkerId) -> bool {
        let mut state = self.state.lock().await;
        let removed = state.workers.remove(worker_id).is_some();
        if removed {
            state
                .leases
                .retain(|_, lease| &lease.lease.worker_id != worker_id);
        }
        removed
    }

    /// Acquires a finite lease without creating a pending placement queue.
    ///
    /// # Errors
    ///
    /// Returns `Busy` immediately when no matching worker has capacity.
    pub async fn acquire(
        &self,
        requirements: &WorkerRequirements,
        ttl: Duration,
    ) -> Result<WorkerLease, WorkerError> {
        if ttl.is_zero() || ttl > self.config.max_lease_ttl {
            return Err(WorkerError::Invalid(
                "worker lease TTL is outside registry bounds".to_owned(),
            ));
        }
        let mut state = self.state.lock().await;
        purge_expired(&mut state);
        if state.leases.len() >= self.config.max_leases {
            return Err(WorkerError::Busy);
        }
        let selected = state
            .workers
            .values()
            .filter(|entry| {
                requirements.matches(&entry.descriptor)
                    && entry.descriptor.active_leases < entry.descriptor.max_leases
            })
            .min_by(|left, right| {
                left.descriptor
                    .active_leases
                    .cmp(&right.descriptor.active_leases)
                    .then_with(|| {
                        left.descriptor
                            .worker_id
                            .as_str()
                            .cmp(right.descriptor.worker_id.as_str())
                    })
            })
            .map(|entry| {
                (
                    entry.descriptor.worker_id.clone(),
                    entry.descriptor.epoch.clone(),
                )
            })
            .ok_or(WorkerError::Busy)?;
        let expires_at = Instant::now() + ttl;
        let lease = WorkerLease {
            lease_id: WorkerLeaseId::random("lease"),
            worker_id: selected.0.clone(),
            worker_epoch: selected.1,
            expires_at: wall_expiry(ttl)?,
        };
        let Some(worker) = state.workers.get_mut(&selected.0) else {
            return Err(WorkerError::Internal);
        };
        worker.descriptor.active_leases += 1;
        state.leases.insert(
            lease.lease_id.clone(),
            LeaseEntry {
                lease: lease.clone(),
                expires_at,
            },
        );
        Ok(lease)
    }

    /// Releases one lease capability. Releasing an unknown lease is harmless.
    pub async fn release(&self, lease_id: &WorkerLeaseId) -> bool {
        let mut state = self.state.lock().await;
        let removed = state.leases.remove(lease_id);
        if let Some(lease) = &removed {
            decrement_worker(&mut state, &lease.lease.worker_id);
        }
        removed.is_some()
    }

    /// Lists non-secret worker descriptors after monotonic expiry cleanup.
    pub async fn list(&self) -> Vec<WorkerDescriptor> {
        let mut state = self.state.lock().await;
        purge_expired(&mut state);
        let mut workers = state
            .workers
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect::<Vec<_>>();
        workers.sort_by(|left, right| left.worker_id.as_str().cmp(right.worker_id.as_str()));
        workers
    }

    pub(crate) async fn resolve(
        &self,
        lease_id: &WorkerLeaseId,
        expected_epoch: &WorkerEpoch,
    ) -> Result<(WorkerLease, Arc<dyn DesktopWorker>), WorkerError> {
        let mut state = self.state.lock().await;
        purge_expired(&mut state);
        let lease = state
            .leases
            .get(lease_id)
            .ok_or(WorkerError::StaleLease)?
            .lease
            .clone();
        if &lease.worker_epoch != expected_epoch {
            return Err(WorkerError::StaleLease);
        }
        let worker = state
            .workers
            .get(&lease.worker_id)
            .filter(|entry| entry.descriptor.epoch == lease.worker_epoch)
            .ok_or(WorkerError::StaleLease)?;
        Ok((lease, Arc::clone(&worker.endpoint)))
    }
}

fn purge_expired(state: &mut RegistryState) {
    let now = Instant::now();
    let expired = state
        .leases
        .iter()
        .filter(|(_, lease)| lease.expires_at <= now)
        .map(|(lease_id, lease)| (lease_id.clone(), lease.lease.worker_id.clone()))
        .collect::<Vec<_>>();
    for (lease_id, worker_id) in expired {
        state.leases.remove(&lease_id);
        decrement_worker(state, &worker_id);
    }
}

fn decrement_worker(state: &mut RegistryState, worker_id: &WorkerId) {
    if let Some(worker) = state.workers.get_mut(worker_id) {
        worker.descriptor.active_leases = worker.descriptor.active_leases.saturating_sub(1);
    }
}

fn wall_expiry(ttl: Duration) -> Result<String, WorkerError> {
    let seconds = i64::try_from(ttl.as_secs()).map_err(|_| {
        WorkerError::Invalid("worker lease TTL does not fit wall-clock projection".to_owned())
    })?;
    (OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
        .format(&Rfc3339)
        .map_err(|_| WorkerError::Internal)
}

/// Worker registration, placement, or lease failure.
#[derive(Debug, Error)]
pub enum WorkerError {
    /// Configuration or request is invalid.
    #[error("invalid desktop worker request: {0}")]
    Invalid(String),
    /// No bounded capacity is currently available; nothing was queued.
    #[error("desktop worker capacity is busy")]
    Busy,
    /// Lease expired, worker reconnected, or epoch no longer matches.
    #[error("desktop worker lease is stale")]
    StaleLease,
    /// Environment descriptor validation failed.
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    /// Internal clock projection failed.
    #[error("desktop worker registry is unavailable")]
    Internal,
}

#[cfg(test)]
mod tests {
    use nexus_cua_protocol::{
        CaptureMode, CommandResult, DriverCapabilities, InputRoute, ResponseOutcome,
    };

    use super::*;

    struct FixtureWorker {
        environment: EnvironmentDescriptor,
    }

    #[async_trait]
    impl DesktopWorker for FixtureWorker {
        fn platform(&self) -> Platform {
            Platform::Macos
        }

        fn environment(&self) -> &EnvironmentDescriptor {
            &self.environment
        }

        async fn dispatch(
            &self,
            request_id: RequestId,
            _timeout_ms: u32,
            _command: Command,
        ) -> ResponseEnvelope {
            ResponseEnvelope {
                protocol_version: nexus_cua_protocol::PROTOCOL_VERSION.to_owned(),
                request_id,
                outcome: ResponseOutcome::Success {
                    result: CommandResult::Capabilities(DriverCapabilities {
                        protocol_version: nexus_cua_protocol::PROTOCOL_VERSION.to_owned(),
                        runtime_version: "fixture".to_owned(),
                        platform: Platform::Macos,
                        capture_modes: vec![CaptureMode::Window],
                        accessibility_tree: false,
                        input_routes: vec![InputRoute::Foreground],
                        application_lifecycle: Vec::new(),
                        clipboard: Vec::new(),
                        actions: Vec::new(),
                    }),
                },
            }
        }

        async fn read_artifact(
            &self,
            _artifact: &ScreenshotArtifact,
            _max_bytes: u64,
        ) -> Result<Vec<u8>, CuaError> {
            Err(CuaError {
                code: nexus_cua_protocol::ErrorCode::ReferenceNotFound,
                message: "fixture has no artifact".to_owned(),
                retryable: false,
                recovery_action: None,
                mutation_status: nexus_cua_protocol::MutationStatus::NotApplicable,
            })
        }
    }

    #[tokio::test]
    async fn placement_is_bounded_balanced_and_epoch_affine() {
        let registry = WorkerRegistry::new(WorkerRegistryConfig {
            max_workers: 2,
            max_leases: 2,
            max_lease_ttl: Duration::from_secs(60),
        })
        .unwrap();
        let first = registry
            .register(
                Arc::new(FixtureWorker {
                    environment: EnvironmentDescriptor::local_user_desktop(),
                }),
                1,
            )
            .await
            .unwrap();
        let second = registry
            .register(
                Arc::new(FixtureWorker {
                    environment: EnvironmentDescriptor::local_user_desktop(),
                }),
                1,
            )
            .await
            .unwrap();

        let lease_one = registry
            .acquire(&WorkerRequirements::default(), Duration::from_secs(30))
            .await
            .unwrap();
        let lease_two = registry
            .acquire(&WorkerRequirements::default(), Duration::from_secs(30))
            .await
            .unwrap();
        assert_ne!(lease_one.worker_id, lease_two.worker_id);
        assert!(matches!(
            registry
                .acquire(&WorkerRequirements::default(), Duration::from_secs(30))
                .await,
            Err(WorkerError::Busy)
        ));

        assert!(registry.release(&lease_one.lease_id).await);
        let affinity = WorkerRequirements {
            worker_id: Some(lease_one.worker_id.clone()),
            ..WorkerRequirements::default()
        };
        let rebound = registry
            .acquire(&affinity, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(rebound.worker_id, lease_one.worker_id);
        assert!(registry.unregister(&rebound.worker_id).await);
        assert!(matches!(
            registry
                .resolve(&rebound.lease_id, &rebound.worker_epoch)
                .await,
            Err(WorkerError::StaleLease)
        ));
        assert!(first.worker_id == lease_one.worker_id || second.worker_id == lease_one.worker_id);
    }
}
