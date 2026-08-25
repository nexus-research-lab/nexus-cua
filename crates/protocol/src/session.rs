//! Session authorization and lifecycle types.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ActionKind, SessionId};

/// Capability mode fixed for the lifetime of a session.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Observation and deterministic verification only.
    ReadOnly,
    /// Only exact applications and action categories declared in the manifest.
    Bounded,
}

/// Exact, host-reviewed session authority.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityManifest {
    /// Session mode. There is deliberately no unrestricted value.
    pub mode: PermissionMode,
    /// Exact application identifiers allowed for observation or mutation.
    pub allowed_application_ids: Vec<String>,
    /// Exact mutation categories allowed in bounded mode.
    pub allowed_actions: Vec<ActionKind>,
    /// Whether any foreground input route is allowed.
    pub allow_foreground_input: bool,
    /// Whether complete-desktop capture is allowed.
    pub allow_desktop_capture: bool,
    /// Finite lifetime in seconds.
    pub ttl_seconds: u32,
}

/// Input for opening one isolated runtime session.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenSessionInput {
    /// Host-reviewed capability manifest.
    pub manifest: CapabilityManifest,
}

/// Result of opening one isolated runtime session.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenSessionOutput {
    /// Runtime-issued session identity.
    pub session_id: SessionId,
    /// RFC 3339 expiration timestamp.
    pub expires_at: String,
}

/// Input selecting one existing session.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInput {
    /// Runtime-issued session identity.
    pub session_id: SessionId,
}
