//! Exact parent-host executable identity for registered adapter launch.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use nexus_cua_protocol::SignatureStatus;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const MAX_HOST_EXECUTABLE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Platform signing identity bound together with an exact executable digest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostSignatureIdentity {
    /// macOS code-signing identity.
    Macos {
        /// Apple team identifier, when the executable is signed.
        signing_team_id: Option<String>,
        /// Designated requirement returned by Security.framework.
        designated_requirement: Option<String>,
    },
    /// Windows Authenticode identity.
    Windows {
        /// Publisher subject returned from the verified signing chain.
        publisher: Option<String>,
        /// Closed Authenticode validation state.
        signature_status: SignatureStatus,
    },
}

/// Exact host executable identity stored in a MAC-authenticated registration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostExecutableIdentity {
    /// SHA-256 of the normalized canonical executable path, not the path itself.
    pub path_sha256: String,
    /// SHA-256 of the exact executable bytes.
    pub executable_sha256: String,
    /// Platform signing identity for the same executable.
    pub signature: HostSignatureIdentity,
}

/// Inspects the exact executable of the process that launched this adapter.
///
/// # Errors
///
/// Returns [`HostIdentityError`] when the parent cannot be resolved, is not a
/// bounded regular executable, changes during hashing, or cannot be inspected.
pub fn parent_host_identity() -> Result<HostExecutableIdentity, HostIdentityError> {
    inspect_host_executable(&parent_executable_path()?)
}

/// Inspects one trusted-plan executable without retaining its public path.
///
/// # Errors
///
/// Returns [`HostIdentityError`] for an unsafe path, oversized executable,
/// read failure, concurrent replacement, or unsupported operating system.
pub fn inspect_host_executable(path: &Path) -> Result<HostExecutableIdentity, HostIdentityError> {
    let canonical = std::fs::canonicalize(path).map_err(HostIdentityError::Io)?;
    let before = std::fs::symlink_metadata(&canonical).map_err(HostIdentityError::Io)?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_HOST_EXECUTABLE_BYTES {
        return Err(HostIdentityError::UnsafeExecutable);
    }
    let executable_sha256 = hash_file(&canonical)?;
    let after = std::fs::symlink_metadata(&canonical).map_err(HostIdentityError::Io)?;
    if !same_file_generation(&before, &after) {
        return Err(HostIdentityError::ExecutableChanged);
    }
    let path_sha256 = hash_normalized_path(&canonical);
    let signature = platform_signature(&canonical)?;
    Ok(HostExecutableIdentity {
        path_sha256,
        executable_sha256,
        signature,
    })
}

fn hash_file(path: &Path) -> Result<String, HostIdentityError> {
    let mut file = File::open(path).map_err(HostIdentityError::Io)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES].into_boxed_slice();
    loop {
        let read = file.read(&mut buffer).map_err(HostIdentityError::Io)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

#[cfg(unix)]
fn same_file_generation(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

#[cfg(windows)]
fn same_file_generation(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    before.file_size() == after.file_size()
        && before.last_write_time() == after.last_write_time()
        && before.creation_time() == after.creation_time()
}

#[cfg(not(any(unix, windows)))]
fn same_file_generation(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.len() == after.len()
}

#[cfg(unix)]
fn hash_normalized_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;

    hex::encode(Sha256::digest(path.as_os_str().as_bytes()))
}

#[cfg(windows)]
fn hash_normalized_path(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .to_lowercase()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    hex::encode(Sha256::digest(&normalized))
}

#[cfg(not(any(unix, windows)))]
fn hash_normalized_path(path: &Path) -> String {
    hex::encode(Sha256::digest(path.to_string_lossy().as_bytes()))
}

#[cfg(target_os = "macos")]
fn parent_executable_path() -> Result<PathBuf, HostIdentityError> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    // SAFETY: `getppid` has no arguments and no ownership obligations.
    let parent = unsafe { libc::getppid() };
    if parent <= 1 {
        return Err(HostIdentityError::ParentUnavailable);
    }
    let mut buffer = vec![0_u8; usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).unwrap_or(4096)];
    // SAFETY: The buffer is writable for its full reported length.
    let length = unsafe {
        libc::proc_pidpath(
            parent,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).unwrap_or(u32::MAX),
        )
    };
    if length <= 0 {
        return Err(HostIdentityError::ParentUnavailable);
    }
    buffer.truncate(usize::try_from(length).map_err(|_| HostIdentityError::ParentUnavailable)?);
    Ok(PathBuf::from(OsString::from_vec(buffer)))
}

#[cfg(target_os = "macos")]
fn platform_signature(path: &Path) -> Result<HostSignatureIdentity, HostIdentityError> {
    let path = path.to_str().ok_or(HostIdentityError::UnsafeExecutable)?;
    let identity = crate::macos::provenance::inspect(path);
    Ok(HostSignatureIdentity::Macos {
        signing_team_id: identity.team_id,
        designated_requirement: identity.designated_requirement,
    })
}

#[cfg(target_os = "windows")]
fn parent_executable_path() -> Result<PathBuf, HostIdentityError> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    // SAFETY: Snapshot ownership is closed on every path below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|_| HostIdentityError::ParentUnavailable)?;
    let current = unsafe { GetCurrentProcessId() };
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
        ..Default::default()
    };
    let mut parent = None;
    let mut next = unsafe { Process32FirstW(snapshot, &raw mut entry) }.is_ok();
    while next {
        if entry.th32ProcessID == current {
            parent = Some(entry.th32ParentProcessID);
            break;
        }
        next = unsafe { Process32NextW(snapshot, &raw mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    let parent = parent
        .filter(|pid| *pid > 0)
        .ok_or(HostIdentityError::ParentUnavailable)?;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, parent) }
        .map_err(|_| HostIdentityError::ParentUnavailable)?;
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            Default::default(),
            PWSTR(buffer.as_mut_ptr()),
            &raw mut length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    queried.map_err(|_| HostIdentityError::ParentUnavailable)?;
    buffer.truncate(usize::try_from(length).map_err(|_| HostIdentityError::ParentUnavailable)?);
    Ok(PathBuf::from(String::from_utf16_lossy(&buffer)))
}

#[cfg(target_os = "windows")]
fn platform_signature(path: &Path) -> Result<HostSignatureIdentity, HostIdentityError> {
    let path = path.to_str().ok_or(HostIdentityError::UnsafeExecutable)?;
    let (publisher, signature_status) = crate::windows::provenance::inspect(path);
    Ok(HostSignatureIdentity::Windows {
        publisher,
        signature_status,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn parent_executable_path() -> Result<PathBuf, HostIdentityError> {
    Err(HostIdentityError::UnsupportedPlatform)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_signature(_path: &Path) -> Result<HostSignatureIdentity, HostIdentityError> {
    Err(HostIdentityError::UnsupportedPlatform)
}

/// Parent-host identity inspection failure without executable path disclosure.
#[derive(Debug, Error)]
pub enum HostIdentityError {
    /// Parent process or its image disappeared before it could be bound.
    #[error("the adapter parent executable is unavailable")]
    ParentUnavailable,
    /// Executable path was not a bounded regular file.
    #[error("the host executable is not a safe bounded regular file")]
    UnsafeExecutable,
    /// Executable identity changed while it was being inspected.
    #[error("the host executable changed during identity inspection")]
    ExecutableChanged,
    /// Host identity is not implemented for this operating system.
    #[error("registered host identity is unsupported on this platform")]
    UnsupportedPlatform,
    /// Filesystem inspection failed without retaining the sensitive path.
    #[error("host executable inspection failed: {0}")]
    Io(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_test_executable_has_stable_path_and_content_digests() {
        let executable = std::env::current_exe().unwrap();
        let first = inspect_host_executable(&executable).unwrap();
        let second = inspect_host_executable(&executable).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.path_sha256.len(), 64);
        assert_eq!(first.executable_sha256.len(), 64);
    }

    #[test]
    fn directory_is_not_an_executable_identity() {
        assert!(matches!(
            inspect_host_executable(Path::new(".")),
            Err(HostIdentityError::UnsafeExecutable)
        ));
    }
}
