//! Native desktop driver selection.
//!
//! Platform implementations live behind one runtime-facing interface so the
//! protocol, authority checks, and stale-observation rules stay identical on
//! macOS and Windows.

use std::sync::Arc;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use nexus_cua_runtime::DriverErrorKind;
use nexus_cua_runtime::{DesktopDriver, DriverError};

#[cfg(target_os = "macos")]
mod macos;
mod observation;
#[cfg(target_os = "windows")]
mod windows;

/// Build the native driver for the current operating system.
///
/// # Errors
///
/// Returns an error when a required native actor cannot be initialized.
#[cfg(target_os = "macos")]
pub fn system_driver() -> Result<Arc<dyn DesktopDriver>, DriverError> {
    Ok(Arc::new(macos::MacosDriver::new()?))
}

/// Build the native driver for the current operating system.
///
/// # Errors
///
/// Returns an error when a required native actor cannot be initialized.
#[cfg(target_os = "windows")]
pub fn system_driver() -> Result<Arc<dyn DesktopDriver>, DriverError> {
    Ok(Arc::new(windows::WindowsDriver::new()?))
}

/// Return an explicit error on unsupported build targets.
///
/// # Errors
///
/// Always returns an unsupported-platform error on this build target.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn system_driver() -> Result<Arc<dyn DesktopDriver>, DriverError> {
    Err(DriverError::new(
        DriverErrorKind::Unsupported,
        "the native driver is not available in this build",
    ))
}
