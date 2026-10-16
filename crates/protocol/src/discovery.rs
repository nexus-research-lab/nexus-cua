//! Trusted-host application discovery without ambient execution authority.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{DeliveryMode, DiscoveryRef, LaunchRef};

/// Best-effort executable signature state reported by a native driver.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    /// The operating system verified the executable signature.
    Verified,
    /// A signature exists but did not validate.
    Invalid,
    /// The executable has no platform signature.
    Unsigned,
    /// The driver could not determine signature state safely.
    Unknown,
}

/// Public, non-authoritative provenance summary for one running application.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationProvenance {
    /// macOS bundle and code-signing identity when available.
    Macos {
        /// Bundle identifier reported by the running application.
        bundle_id: Option<String>,
        /// Canonical executable path when available.
        executable_path: Option<String>,
        /// Signing team identifier when available.
        signing_team_id: Option<String>,
        /// Designated code-signing requirement when available.
        designated_requirement: Option<String>,
    },
    /// Windows image path and Authenticode identity when available.
    Windows {
        /// Normalized executable image path.
        executable_path: String,
        /// Authenticode publisher subject when available.
        publisher: Option<String>,
        /// Best-effort Authenticode state.
        signature_status: SignatureStatus,
    },
    /// Explicit fallback for a driver without a supported provenance format.
    Unsupported {
        /// Executable path when the driver can report one safely.
        executable_path: Option<String>,
    },
}

/// One running application visible to an authenticated policy host.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredApplication {
    /// Short-lived, runtime-local reference used to request session authority.
    pub discovery_ref: DiscoveryRef,
    /// Human-readable application name.
    pub name: String,
    /// Stable bundle or executable identity for policy display and matching.
    pub application_id: String,
    /// Whether this process generation currently owns foreground input.
    pub foreground: bool,
    /// Non-authoritative platform provenance summary.
    pub provenance: ApplicationProvenance,
    /// RFC 3339 deadline after which the reference must be rediscovered.
    pub expires_at: String,
}

/// Bounded result of trusted-host application discovery.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverApplicationsOutput {
    /// Runtime-issued descriptors ordered by stable application identity.
    pub applications: Vec<DiscoveredApplication>,
    /// False when the configured discovery result bound truncated the snapshot.
    pub complete: bool,
}

/// One installed application selected from bounded native discovery.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchableApplication {
    /// Short-lived, single-use runtime reference. It is not a filesystem path.
    pub launch_ref: LaunchRef,
    /// Human-readable installed application name.
    pub name: String,
    /// Stable bundle, package, or executable identity used for policy review.
    pub application_id: String,
    /// Non-authoritative provenance summary captured during discovery.
    pub provenance: ApplicationProvenance,
    /// RFC 3339 deadline after which the application must be rediscovered.
    pub expires_at: String,
}

/// Bounded result of trusted-host installed-application discovery.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoverLaunchableApplicationsOutput {
    /// Runtime-issued descriptors ordered by stable application identity.
    pub applications: Vec<LaunchableApplication>,
    /// False when the configured discovery result bound truncated the snapshot.
    pub complete: bool,
}

/// Explicit host authority for one cold application launch.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchApplicationInput {
    /// Fresh opaque identity from installed-application discovery.
    pub launch_ref: LaunchRef,
    /// Whether the launched application may become the foreground owner.
    pub allow_foreground_activation: bool,
}

/// Successful cold-launch result and fresh running-generation reference.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationLaunchOutput {
    /// Newly launched exact process generation, usable to open a bounded session.
    pub application: DiscoveredApplication,
    /// Native route observed for the launch.
    pub delivery_mode: DeliveryMode,
    /// Whether the platform reported successful dispatch and generation discovery.
    pub dispatched: bool,
}
