//! Terminal-friendly structured diagnostics.

use clap::ValueEnum;
use tracing_subscriber::EnvFilter;

use crate::error::CliError;

/// Rendering mode for service logs.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum LogFormat {
    /// Compact, readable local development output.
    Pretty,
    /// One machine-readable JSON object per event.
    Json,
}

pub(crate) fn init_logging(format: LogFormat, level: &str) -> Result<(), CliError> {
    let filter = EnvFilter::try_new(level).map_err(|error| CliError::Logging(error.to_string()))?;
    match format {
        LogFormat::Pretty => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .compact()
            .try_init()
            .map_err(|error| CliError::Logging(error.to_string())),
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .flatten_event(true)
            .try_init()
            .map_err(|error| CliError::Logging(error.to_string())),
    }
}
