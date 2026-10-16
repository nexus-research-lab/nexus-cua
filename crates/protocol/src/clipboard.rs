//! Separately authorized, bounded plain-text clipboard contract.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ActionEffectStatus, ActionEvidenceKind, ClipboardRef, DeliveryMode, SensitiveText, SessionId,
};

/// Clipboard capabilities advertised by the selected native driver.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardOperation {
    /// Read bounded UTF-8 plain text from the interactive desktop clipboard.
    ReadText,
    /// Replace the clipboard with bounded plain text after a change guard.
    WriteText,
}

/// Input for one separately authorized clipboard read.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadClipboardInput {
    /// Runtime-issued session identity with clipboard-read authority.
    pub session_id: SessionId,
}

/// One transient clipboard snapshot. The text is deliberately redacted from
/// debug formatting and must not be logged or retained by the runtime.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClipboardSnapshot {
    /// Session-scoped guard for a later compare-and-replace write.
    pub clipboard_ref: ClipboardRef,
    /// Plain text, or `null` when the clipboard has no supported text format.
    pub text: Option<SensitiveText>,
    /// Exact UTF-8 byte length of `text`, or zero when text is unavailable.
    pub byte_length: u32,
}

/// Input for a guarded plain-text clipboard replacement.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteClipboardInput {
    /// Runtime-issued session identity with clipboard-write authority.
    pub session_id: SessionId,
    /// Fresh snapshot guard proving the clipboard was inspected before write.
    pub clipboard_ref: ClipboardRef,
    /// Replacement text. Debug output is always redacted.
    pub text: SensitiveText,
}

/// Successful guarded clipboard replacement.
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClipboardWriteOutput {
    /// Fresh guard describing the clipboard immediately after the write.
    pub clipboard_ref: ClipboardRef,
    /// Native system clipboard route.
    pub delivery_mode: DeliveryMode,
    /// Whether the native provider accepted the replacement.
    pub dispatched: bool,
    /// Truthful effect classification.
    pub effect_status: ActionEffectStatus,
    /// Evidence supporting `effect_status`.
    pub evidence_kind: ActionEvidenceKind,
}
