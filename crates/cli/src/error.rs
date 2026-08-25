//! CLI error boundary.

use thiserror::Error;

/// Human-readable terminal failure without sensitive command payloads.
#[derive(Debug, Error)]
pub enum CliError {
    /// CLI arguments form an invalid configuration.
    #[error("invalid configuration: {0}")]
    InvalidConfiguration(String),
    /// Local filesystem or terminal I/O failed.
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A JSON diagnostic or command payload is invalid.
    #[error("JSON processing failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Native platform driver setup failed.
    #[error("native driver setup failed: {0}")]
    Driver(#[from] nexus_cua_runtime::DriverError),
    /// Runtime setup rejected the local configuration.
    #[error("runtime setup failed: {0:?}")]
    Runtime(nexus_cua_protocol::CuaError),
    /// Private local IPC failed.
    #[error(transparent)]
    Transport(#[from] nexus_cua_transport::TransportError),
    /// Structured terminal logging could not be installed.
    #[error("logging setup failed: {0}")]
    Logging(String),
}
