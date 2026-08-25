//! Transport setup and framing errors.

use thiserror::Error;

/// Local IPC failure that is not part of the public CUA command outcome.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Endpoint or token configuration is invalid.
    #[error("invalid transport configuration: {0}")]
    InvalidConfiguration(String),
    /// Local operating-system I/O failed.
    #[error("local transport I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A frame exceeded the configured limit or was truncated.
    #[error("invalid local transport frame: {0}")]
    InvalidFrame(String),
    /// A response could not be decoded as the versioned protocol.
    #[error("invalid protocol payload: {0}")]
    InvalidPayload(#[from] serde_json::Error),
}
