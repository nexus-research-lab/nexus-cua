//! Native desktop driver selection.
//!
//! Platform implementations live behind one runtime-facing interface so the
//! protocol, authority checks, and stale-observation rules stay identical on
//! macOS and Windows.

use std::sync::Arc;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use nexus_cua_runtime::DriverErrorKind;
use nexus_cua_runtime::{DesktopDriver, DriverError};

mod host_identity;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod input_text;
#[cfg(target_os = "macos")]
mod macos;
mod observation;
#[cfg(target_os = "windows")]
mod windows;

pub use host_identity::{
    HostExecutableIdentity, HostIdentityError, HostSignatureIdentity, inspect_host_executable,
    parent_host_identity,
};

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

/// Runs a host future while servicing bounded native main-thread requests.
/// Call on the process main thread around the runtime host's async entry point.
/// macOS input hit testing requires this dispatcher; other platforms pass through.
pub async fn with_native_main_thread<F: std::future::Future>(future: F) -> F::Output {
    #[cfg(target_os = "macos")]
    {
        macos::main_thread::run(future).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        future.await
    }
}
