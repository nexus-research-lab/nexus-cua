//! Owner-private registration filesystem helpers.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub(crate) fn secure_directory(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private state root is not a real directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)?;
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "private state root is not a real directory",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    apply_owner_only(path, true)
}

pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    apply_owner_only(path, false)?;
    Ok(file)
}

pub(crate) fn open_private_lock(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state lock is not a real file",
        ));
    }
    apply_owner_only(path, false)?;
    Ok(file)
}

pub(crate) fn has_json_capacity(
    root: &Path,
    maximum_json_files: usize,
    maximum_directory_entries: usize,
) -> io::Result<bool> {
    let mut json_files = 0_usize;
    let mut directory_entries = 0_usize;
    for entry in std::fs::read_dir(root)?.take(maximum_directory_entries.saturating_add(1)) {
        let entry = entry?;
        directory_entries = directory_entries.saturating_add(1);
        if entry
            .path()
            .extension()
            .is_some_and(|value| value == "json")
        {
            json_files = json_files.saturating_add(1);
        }
    }
    Ok(directory_entries <= maximum_directory_entries && json_files < maximum_json_files)
}

pub(crate) fn validate_private_regular_file(path: &Path, max_bytes: u64) -> io::Result<u64> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state path is not a bounded regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private state file permissions are too broad",
            ));
        }
    }
    Ok(metadata.len())
}

#[cfg(unix)]
fn apply_owner_only(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let mode = if directory { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
fn apply_owner_only(path: &Path, _directory: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };
    use windows::core::PCWSTR;

    const OWNER_SYSTEM_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;OW)";
    let sddl = OWNER_SYSTEM_SDDL
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &raw mut descriptor,
            None,
        )
    };
    converted.map_err(|_| io::Error::last_os_error())?;
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let applied = unsafe {
        SetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let _ = unsafe { LocalFree(Some(windows::Win32::Foundation::HLOCAL(descriptor.0))) };
    if applied.as_bool() {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(unix, windows)))]
fn apply_owner_only(_path: &Path, _directory: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owner-private state is unsupported on this platform",
    ))
}

pub(crate) fn remove_private_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
