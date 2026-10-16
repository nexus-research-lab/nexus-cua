//! Immutable managed/user dispatch policy composition.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use nexus_cua_protocol::{
    Action, ActionKind, Command, CuaError, DeliveryPreference, ErrorCode, MutationStatus,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::error::public_error;

const POLICY_SCHEMA_VERSION: u32 = 1;
const MAX_POLICY_BYTES: u64 = 256 * 1024;
const MAX_POLICY_ENTRIES: usize = 128;

/// Closed command categories understood by the runtime policy engine.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOperation {
    /// Read driver capabilities.
    GetCapabilities,
    /// Read operating-system permission state.
    GetPermissionStatus,
    /// Discover running applications.
    DiscoverApplications,
    /// Discover installed applications.
    DiscoverLaunchableApplications,
    /// Launch one opaque installed-application reference.
    LaunchApplication,
    /// Open one capability session.
    OpenSession,
    /// Close one capability session.
    CloseSession,
    /// List session applications.
    ListApps,
    /// List session windows.
    ListWindows,
    /// List session physical displays.
    ListDisplays,
    /// Observe one guarded window.
    ObserveWindow,
    /// Capture one exact physical display.
    ObserveDisplay,
    /// Read bounded plain text from the system clipboard.
    ReadClipboard,
    /// Replace system clipboard text under a fresh change guard.
    WriteClipboard,
    /// Perform one guarded mutation.
    PerformAction,
    /// Read one dispatcher-owned mutation result.
    ReconcileMutation,
    /// Verify fresh state.
    VerifyState,
}

impl PolicyOperation {
    fn of(command: &Command) -> Self {
        match command {
            Command::GetCapabilities => Self::GetCapabilities,
            Command::GetPermissionStatus => Self::GetPermissionStatus,
            Command::DiscoverApplications => Self::DiscoverApplications,
            Command::DiscoverLaunchableApplications => Self::DiscoverLaunchableApplications,
            Command::LaunchApplication(_) => Self::LaunchApplication,
            Command::OpenSession(_) => Self::OpenSession,
            Command::CloseSession(_) => Self::CloseSession,
            Command::ListApps(_) => Self::ListApps,
            Command::ListWindows(_) => Self::ListWindows,
            Command::ListDisplays(_) => Self::ListDisplays,
            Command::ObserveWindow(_) => Self::ObserveWindow,
            Command::ObserveDisplay(_) => Self::ObserveDisplay,
            Command::ReadClipboard(_) => Self::ReadClipboard,
            Command::WriteClipboard(_) => Self::WriteClipboard,
            Command::PerformAction(_) => Self::PerformAction,
            Command::ReconcileMutation(_) => Self::ReconcileMutation,
            Command::VerifyState(_) => Self::VerifyState,
        }
    }
}

/// Optional argument limits for one action kind.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyActionRule {
    /// Action category admitted when every configured constraint matches.
    pub action: ActionKind,
    /// Maximum UTF-8 bytes accepted by `type_text` or `set_value`.
    #[serde(default)]
    pub max_text_bytes: Option<usize>,
    /// Maximum key count accepted by `press_keys`.
    #[serde(default)]
    pub max_keys: Option<usize>,
    /// Maximum click count accepted by `click_point`.
    #[serde(default)]
    pub max_click_count: Option<u8>,
    /// Maximum duration accepted by pointer movement, hold, or drag.
    #[serde(default)]
    pub max_duration_ms: Option<u32>,
    /// Maximum absolute delta on each scroll axis.
    #[serde(default)]
    pub max_abs_scroll_delta: Option<f64>,
    /// Closed click delivery preferences admitted by this rule. Empty means
    /// both routes, but only for `click_point`.
    #[serde(default)]
    pub deliveries: Vec<DeliveryPreference>,
}

impl PolicyActionRule {
    fn validate(&self) -> Result<(), PolicyLoadError> {
        if self.max_text_bytes == Some(0)
            || self.max_keys == Some(0)
            || self.max_click_count == Some(0)
            || self.max_duration_ms == Some(0)
            || self
                .max_abs_scroll_delta
                .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(PolicyLoadError::Invalid(
                "action-rule bounds must be finite and non-zero".to_owned(),
            ));
        }
        let deliveries = self.deliveries.iter().copied().collect::<HashSet<_>>();
        if deliveries.len() != self.deliveries.len() {
            return Err(PolicyLoadError::Invalid(
                "action rule contains duplicate delivery values".to_owned(),
            ));
        }
        let fields_are_relevant = match self.action {
            ActionKind::ClickPoint => {
                self.max_text_bytes.is_none()
                    && self.max_keys.is_none()
                    && self.max_duration_ms.is_none()
                    && self.max_abs_scroll_delta.is_none()
            }
            ActionKind::PressPoint | ActionKind::MovePointer | ActionKind::Drag => {
                self.max_text_bytes.is_none()
                    && self.max_keys.is_none()
                    && self.max_click_count.is_none()
                    && self.max_abs_scroll_delta.is_none()
                    && self.deliveries.is_empty()
            }
            ActionKind::TypeText | ActionKind::SetValue => {
                self.max_keys.is_none()
                    && self.max_click_count.is_none()
                    && self.max_duration_ms.is_none()
                    && self.max_abs_scroll_delta.is_none()
                    && self.deliveries.is_empty()
            }
            ActionKind::PressKeys => {
                self.max_text_bytes.is_none()
                    && self.max_click_count.is_none()
                    && self.max_duration_ms.is_none()
                    && self.max_abs_scroll_delta.is_none()
                    && self.deliveries.is_empty()
            }
            ActionKind::Scroll => {
                self.max_text_bytes.is_none()
                    && self.max_keys.is_none()
                    && self.max_click_count.is_none()
                    && self.max_duration_ms.is_none()
                    && self.deliveries.is_empty()
            }
            _ => {
                self.max_text_bytes.is_none()
                    && self.max_keys.is_none()
                    && self.max_click_count.is_none()
                    && self.max_duration_ms.is_none()
                    && self.max_abs_scroll_delta.is_none()
                    && self.deliveries.is_empty()
            }
        };
        if !fields_are_relevant {
            return Err(PolicyLoadError::Invalid(
                "action rule contains a constraint that does not apply to its action".to_owned(),
            ));
        }
        Ok(())
    }

    fn matches(&self, action: &Action) -> bool {
        if action.kind() != self.action {
            return false;
        }
        match action {
            Action::ClickPoint {
                count, delivery, ..
            } => {
                self.max_click_count.is_none_or(|maximum| *count <= maximum)
                    && (self.deliveries.is_empty() || self.deliveries.contains(delivery))
            }
            Action::PressPoint { duration_ms, .. }
            | Action::MovePointer { duration_ms, .. }
            | Action::Drag { duration_ms, .. } => self
                .max_duration_ms
                .is_none_or(|maximum| *duration_ms <= maximum),
            Action::TypeText { text } => self
                .max_text_bytes
                .is_none_or(|maximum| text.expose().len() <= maximum),
            Action::SetValue { value, .. } => self
                .max_text_bytes
                .is_none_or(|maximum| value.expose().len() <= maximum),
            Action::PressKeys { keys } => self.max_keys.is_none_or(|maximum| keys.len() <= maximum),
            Action::Scroll {
                delta_x, delta_y, ..
            } => self
                .max_abs_scroll_delta
                .is_none_or(|maximum| delta_x.abs() <= maximum && delta_y.abs() <= maximum),
            _ => true,
        }
    }
}

/// Deny-by-default allow set for one immutable policy layer.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAllow {
    /// Unconditionally admitted command categories.
    #[serde(default)]
    pub operations: Vec<PolicyOperation>,
    /// Unconditionally admitted action categories.
    #[serde(default)]
    pub actions: Vec<ActionKind>,
    /// Argument-constrained alternate action grants.
    #[serde(default)]
    pub action_rules: Vec<PolicyActionRule>,
}

/// Hard deny overrides for one immutable policy layer.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyDeny {
    /// Command categories rejected before allow evaluation.
    #[serde(default)]
    pub operations: Vec<PolicyOperation>,
    /// Action categories rejected before allow evaluation.
    #[serde(default)]
    pub actions: Vec<ActionKind>,
}

/// One immutable runtime policy layer.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePolicyLayer {
    /// Closed policy schema version. Must be `1`.
    pub schema_version: u32,
    /// Deny-by-default grants.
    #[serde(default)]
    pub allow: PolicyAllow,
    /// Hard deny overrides.
    #[serde(default)]
    pub deny: PolicyDeny,
}

impl RuntimePolicyLayer {
    fn validate(&self) -> Result<(), PolicyLoadError> {
        if self.schema_version != POLICY_SCHEMA_VERSION {
            return Err(PolicyLoadError::Invalid(
                "unsupported policy schema_version".to_owned(),
            ));
        }
        let total = self.allow.operations.len()
            + self.allow.actions.len()
            + self.allow.action_rules.len()
            + self.deny.operations.len()
            + self.deny.actions.len();
        if total > MAX_POLICY_ENTRIES {
            return Err(PolicyLoadError::Invalid(
                "policy contains more than 128 entries".to_owned(),
            ));
        }
        require_unique(&self.allow.operations, "allow operations")?;
        require_unique(&self.allow.actions, "allow actions")?;
        require_unique(&self.deny.operations, "deny operations")?;
        require_unique(&self.deny.actions, "deny actions")?;
        for rule in &self.allow.action_rules {
            rule.validate()?;
        }
        Ok(())
    }

    fn authorize(&self, command: &Command) -> bool {
        let operation = PolicyOperation::of(command);
        if self.deny.operations.contains(&operation) || !self.allow.operations.contains(&operation)
        {
            return false;
        }
        match command {
            Command::PerformAction(input) => self.authorizes_action(&input.action),
            Command::OpenSession(input) => {
                input
                    .manifest
                    .allowed_actions
                    .iter()
                    .all(|action| self.authorizes_action_kind(*action))
                    && (!input.manifest.allow_clipboard_read
                        || self.authorizes_operation(PolicyOperation::ReadClipboard))
                    && (!input.manifest.allow_clipboard_write
                        || self.authorizes_operation(PolicyOperation::WriteClipboard))
            }
            _ => true,
        }
    }

    fn authorizes_operation(&self, operation: PolicyOperation) -> bool {
        !self.deny.operations.contains(&operation) && self.allow.operations.contains(&operation)
    }

    fn authorizes_action_kind(&self, action: ActionKind) -> bool {
        !self.deny.actions.contains(&action)
            && (self.allow.actions.contains(&action)
                || self
                    .allow
                    .action_rules
                    .iter()
                    .any(|rule| rule.action == action))
    }

    fn authorizes_action(&self, action: &Action) -> bool {
        let kind = action.kind();
        if self.deny.actions.contains(&kind) {
            return false;
        }
        self.allow.actions.contains(&kind)
            || self
                .allow
                .action_rules
                .iter()
                .any(|rule| rule.matches(action))
    }
}

#[derive(Clone, Debug)]
struct LoadedPolicy {
    layer: RuntimePolicyLayer,
    sha256: String,
}

/// Immutable managed and user policy stack shared by every runtime path.
#[derive(Clone, Debug, Default)]
pub struct RuntimePolicyStack {
    managed: Option<LoadedPolicy>,
    user: Option<LoadedPolicy>,
}

impl RuntimePolicyStack {
    /// Builds a stack from already-reviewed layers.
    ///
    /// # Errors
    ///
    /// Returns an error when either layer is invalid or exceeds closed bounds.
    pub fn from_layers(
        managed: Option<RuntimePolicyLayer>,
        user: Option<RuntimePolicyLayer>,
    ) -> Result<Self, PolicyLoadError> {
        Ok(Self {
            managed: managed.map(LoadedPolicy::from_layer).transpose()?,
            user: user.map(LoadedPolicy::from_layer).transpose()?,
        })
    }

    /// Loads immutable JSON snapshots from optional managed and user files.
    /// Explicit paths fail closed when missing, unsafe, or invalid.
    ///
    /// # Errors
    ///
    /// Returns a bounded public setup error without policy contents.
    pub fn from_files(
        managed: Option<&Path>,
        user: Option<&Path>,
    ) -> Result<Self, PolicyLoadError> {
        Ok(Self {
            managed: managed.map(load_policy).transpose()?,
            user: user.map(load_policy).transpose()?,
        })
    }

    /// Returns immutable content hashes without exposing policy contents.
    pub fn summary(&self) -> RuntimePolicySummary {
        RuntimePolicySummary {
            managed_sha256: self.managed.as_ref().map(|policy| policy.sha256.clone()),
            user_sha256: self.user.as_ref().map(|policy| policy.sha256.clone()),
        }
    }

    pub(crate) fn authorize(&self, command: &Command) -> Result<(), CuaError> {
        if self
            .managed
            .iter()
            .chain(self.user.iter())
            .all(|policy| policy.layer.authorize(command))
        {
            return Ok(());
        }
        let mutation = matches!(
            command,
            Command::LaunchApplication(_) | Command::WriteClipboard(_) | Command::PerformAction(_)
        );
        let error = public_error(
            ErrorCode::CapabilityDenied,
            "runtime policy denied the requested operation",
            false,
            Some("request_host_policy"),
        );
        Err(if mutation {
            error.with_mutation_status(MutationStatus::NotDispatched)
        } else {
            error
        })
    }
}

impl LoadedPolicy {
    fn from_layer(layer: RuntimePolicyLayer) -> Result<Self, PolicyLoadError> {
        layer.validate()?;
        let canonical = serde_json::to_vec(&layer)?;
        Ok(Self {
            layer,
            sha256: hex::encode(Sha256::digest(canonical)),
        })
    }
}

/// Non-sensitive runtime policy status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimePolicySummary {
    /// Immutable managed policy content digest, when configured.
    pub managed_sha256: Option<String>,
    /// Immutable user policy content digest, when configured.
    pub user_sha256: Option<String>,
}

/// Policy file or schema setup failure.
#[derive(Debug, Error)]
pub enum PolicyLoadError {
    /// Policy file I/O failed.
    #[error("cannot load runtime policy {path}: {source}")]
    Io {
        /// Configured path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// JSON is malformed or contains unknown fields.
    #[error("runtime policy JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    /// Closed semantic validation failed.
    #[error("runtime policy is invalid: {0}")]
    Invalid(String),
    /// The configured file is unsafe or outside the size bound.
    #[error("runtime policy file is unsafe: {0}")]
    UnsafeFile(String),
}

fn load_policy(path: &Path) -> Result<LoadedPolicy, PolicyLoadError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| PolicyLoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PolicyLoadError::UnsafeFile(
            "configured path must be a regular file".to_owned(),
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_POLICY_BYTES {
        return Err(PolicyLoadError::UnsafeFile(
            "configured file must contain between 1 byte and 256 KiB".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(PolicyLoadError::UnsafeFile(
                "configured file must not be group- or world-writable".to_owned(),
            ));
        }
    }
    let bytes = std::fs::read(path).map_err(|source| PolicyLoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let layer: RuntimePolicyLayer = serde_json::from_slice(&bytes)?;
    layer.validate()?;
    Ok(LoadedPolicy {
        layer,
        sha256: hex::encode(Sha256::digest(bytes)),
    })
}

fn require_unique<T: Eq + std::hash::Hash>(
    values: &[T],
    label: &str,
) -> Result<(), PolicyLoadError> {
    if values.iter().collect::<HashSet<_>>().len() == values.len() {
        Ok(())
    } else {
        Err(PolicyLoadError::Invalid(format!(
            "policy contains duplicate {label}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use nexus_cua_protocol::{
        Action, Command, DeliveryPreference, PerformActionInput, PointerButton, ScreenshotPoint,
        SensitiveText,
    };

    use super::*;

    fn action_command(action: Action) -> Command {
        Command::PerformAction(PerformActionInput {
            session_id: nexus_cua_protocol::SessionId::new("session"),
            window_ref: nexus_cua_protocol::WindowRef::new("window"),
            observation_id: nexus_cua_protocol::ObservationId::new("observation"),
            action,
        })
    }

    #[test]
    fn managed_and_user_layers_only_narrow() {
        let managed = RuntimePolicyLayer {
            schema_version: 1,
            allow: PolicyAllow {
                operations: vec![PolicyOperation::PerformAction],
                actions: vec![ActionKind::ClickPoint, ActionKind::TypeText],
                action_rules: Vec::new(),
            },
            deny: PolicyDeny::default(),
        };
        let user = RuntimePolicyLayer {
            schema_version: 1,
            allow: PolicyAllow {
                operations: vec![PolicyOperation::PerformAction],
                actions: vec![ActionKind::ClickPoint],
                action_rules: Vec::new(),
            },
            deny: PolicyDeny::default(),
        };
        let stack = RuntimePolicyStack::from_layers(Some(managed), Some(user)).unwrap();
        assert!(
            stack
                .authorize(&action_command(Action::ClickPoint {
                    point: ScreenshotPoint { x: 0, y: 0 },
                    button: PointerButton::Left,
                    count: 1,
                    delivery: DeliveryPreference::Background,
                }))
                .is_ok()
        );
        assert!(
            stack
                .authorize(&action_command(Action::TypeText {
                    text: SensitiveText::new("blocked by narrower user layer"),
                }))
                .is_err()
        );
    }

    #[test]
    fn action_rule_checks_sensitive_length_without_exposing_text() {
        let layer = RuntimePolicyLayer {
            schema_version: 1,
            allow: PolicyAllow {
                operations: vec![PolicyOperation::PerformAction],
                actions: Vec::new(),
                action_rules: vec![PolicyActionRule {
                    action: ActionKind::TypeText,
                    max_text_bytes: Some(4),
                    max_keys: None,
                    max_click_count: None,
                    max_duration_ms: None,
                    max_abs_scroll_delta: None,
                    deliveries: Vec::new(),
                }],
            },
            deny: PolicyDeny::default(),
        };
        let stack = RuntimePolicyStack::from_layers(Some(layer), None).unwrap();
        assert!(
            stack
                .authorize(&action_command(Action::TypeText {
                    text: SensitiveText::new("1234"),
                }))
                .is_ok()
        );
        let error = stack
            .authorize(&action_command(Action::TypeText {
                text: SensitiveText::new("do-not-log-this-secret"),
            }))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::CapabilityDenied);
        assert_eq!(error.mutation_status, MutationStatus::NotDispatched);
        assert!(!error.message.contains("secret"));
    }
}
