//! Cross-platform private endpoint configuration.

use std::fmt;

use crate::TransportError;

const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Unix-domain socket path on macOS or named-pipe path on Windows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEndpoint(String);

impl LocalEndpoint {
    /// Creates a non-empty endpoint without platform-specific rewriting.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty endpoint or one containing a NUL byte.
    pub fn new(value: impl Into<String>) -> Result<Self, TransportError> {
        let value = value.into();
        if value.is_empty() || value.contains('\0') {
            return Err(TransportError::InvalidConfiguration(
                "endpoint must be non-empty and contain no NUL bytes".to_owned(),
            ));
        }
        Ok(Self(value))
    }

    /// Returns the exact operating-system endpoint value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LocalEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Bounds and identity for one local service endpoint.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Private local socket or pipe address.
    pub endpoint: LocalEndpoint,
    /// Maximum request or response payload length.
    pub max_frame_bytes: usize,
    /// Maximum completed idempotency records retained in memory.
    pub max_completed_requests: usize,
    /// Maximum distinct admitted requests that have not completed.
    pub max_inflight_requests: usize,
    /// Maximum caller-supplied end-to-end deadline.
    pub max_request_timeout_ms: u32,
}

impl ServerConfig {
    /// Creates a config with a one-megabyte frame limit.
    pub fn new(endpoint: LocalEndpoint) -> Self {
        Self {
            endpoint,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            max_completed_requests: 4_096,
            max_inflight_requests: 64,
            max_request_timeout_ms: 120_000,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), TransportError> {
        if self.max_frame_bytes == 0 || self.max_frame_bytes > u32::MAX as usize {
            return Err(TransportError::InvalidConfiguration(
                "max_frame_bytes must fit in a non-zero u32".to_owned(),
            ));
        }
        if self.max_completed_requests == 0 || self.max_inflight_requests == 0 {
            return Err(TransportError::InvalidConfiguration(
                "request ledger bounds must be non-zero".to_owned(),
            ));
        }
        if self.max_request_timeout_ms == 0 {
            return Err(TransportError::InvalidConfiguration(
                "max_request_timeout_ms must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }
}
