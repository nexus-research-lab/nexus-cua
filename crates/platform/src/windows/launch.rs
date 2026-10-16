//! Bounded Start Menu discovery and Shell launch actor.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::fs::Metadata;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;

use nexus_cua_protocol::{ApplicationProvenance, SignatureStatus};
use nexus_cua_runtime::{DriverError, DriverErrorKind, DriverLaunchableApplication};
use tokio::sync::oneshot;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, OPEN_EXISTING, WIN32_FIND_DATAW,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_Programs, IShellLinkW, KNOWN_FOLDER_FLAG,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_UNICODE,
    SHELLEXECUTEINFOW, SHGetKnownFolderPath, SLGP_RAWPATH, ShellExecuteExW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, SW_SHOWNORMAL};
use windows::core::{Interface, PCWSTR};

use super::discovery::normalize_executable_identity;
use super::provenance;

const COMMAND_CAPACITY: usize = 16;
const MAX_SCAN_ENTRIES: usize = 16_384;
const MAX_SCAN_DEPTH: usize = 4;
const MAX_DISCOVERED_APPLICATIONS: usize = 2_049;
const MAX_PATH_UNITS: usize = 32_768;
const MAX_PROVENANCE_CACHE: usize = 2_048;

#[derive(Clone)]
pub(super) struct LaunchActor {
    sender: SyncSender<LaunchCommand>,
}

impl LaunchActor {
    pub(super) fn spawn() -> Self {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        thread::Builder::new()
            .name("nexus-cua-windows-launch".to_owned())
            .spawn(move || actor_thread(&receiver))
            .expect("create Windows launch actor thread");
        Self { sender }
    }

    pub(super) async fn list(&self) -> Result<Vec<DriverLaunchableApplication>, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.send(LaunchCommand::List { reply }, false)?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn launch(
        &self,
        application: DriverLaunchableApplication,
        allow_foreground_activation: bool,
    ) -> Result<(), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            LaunchCommand::Launch {
                application: Box::new(application),
                allow_foreground_activation,
                reply,
            },
            true,
        )?;
        receiver.await.map_err(actor_stopped)?
    }

    fn send(&self, command: LaunchCommand, mutation: bool) -> Result<(), DriverError> {
        self.sender.try_send(command).map_err(|error| {
            let error = match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "Windows launch actor is busy")
                        .retryable("retry_with_backoff")
                }
                TrySendError::Disconnected(_) => actor_stopped(()),
            };
            if mutation {
                error.mutation_not_dispatched()
            } else {
                error
            }
        })
    }
}

enum LaunchCommand {
    List {
        reply: oneshot::Sender<Result<Vec<DriverLaunchableApplication>, DriverError>>,
    },
    Launch {
        application: Box<DriverLaunchableApplication>,
        allow_foreground_activation: bool,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
}

struct ProvenanceCache {
    values: HashMap<String, (Option<String>, SignatureStatus)>,
    order: VecDeque<String>,
}

impl ProvenanceCache {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn inspect(&mut self, key: String, executable_path: &str) -> (Option<String>, SignatureStatus) {
        if let Some(value) = self.values.get(&key) {
            return value.clone();
        }
        while self.values.len() >= MAX_PROVENANCE_CACHE {
            if let Some(expired) = self.order.pop_front() {
                self.values.remove(&expired);
            } else {
                break;
            }
        }
        let value = provenance::inspect(executable_path);
        self.order.push_back(key.clone());
        self.values.insert(key, value.clone());
        value
    }
}

fn actor_thread(receiver: &Receiver<LaunchCommand>) {
    // SAFETY: The actor is a fresh dedicated thread and owns every COM object
    // it creates until it uninitializes the apartment below.
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .is_ok();
    let mut cache = ProvenanceCache::new();
    while let Ok(command) = receiver.recv() {
        match command {
            LaunchCommand::List { reply } => {
                let result = if initialized {
                    list_launchable_applications(&mut cache)
                } else {
                    Err(actor_stopped(()))
                };
                let _ = reply.send(result);
            }
            LaunchCommand::Launch {
                application,
                allow_foreground_activation,
                reply,
            } => {
                let result = if initialized {
                    launch_application(&application, allow_foreground_activation, &mut cache)
                } else {
                    Err(actor_stopped(()).mutation_not_dispatched())
                };
                let _ = reply.send(result);
            }
        }
    }
    if initialized {
        // SAFETY: Balances the successful initialization on this same thread.
        unsafe { CoUninitialize() };
    }
}

fn list_launchable_applications(
    cache: &mut ProvenanceCache,
) -> Result<Vec<DriverLaunchableApplication>, DriverError> {
    let roots = start_menu_roots()?;
    let mut queue = roots
        .iter()
        .cloned()
        .map(|root| (root.clone(), root, 0_usize))
        .collect::<VecDeque<_>>();
    let mut scanned = 0_usize;
    let mut applications = Vec::new();
    while let Some((directory, root, depth)) = queue.pop_front() {
        let mut entries = std::fs::read_dir(&directory)
            .map_err(|_| discovery_failure("cannot enumerate a Windows Start Menu directory"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| discovery_failure("cannot read a Windows Start Menu entry"))?;
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            scanned = scanned.saturating_add(1);
            if scanned > MAX_SCAN_ENTRIES {
                return Err(DriverError::new(
                    DriverErrorKind::Busy,
                    "Windows Start Menu discovery exceeded its bounded scan limit",
                )
                .retryable("narrow_installed_application_scope"));
            }
            let path = entry.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) if !is_reparse_point(&metadata) => metadata,
                Ok(_) | Err(_) => continue,
            };
            if metadata.is_dir() {
                if depth < MAX_SCAN_DEPTH {
                    queue.push_back((path, root.clone(), depth + 1));
                }
                continue;
            }
            if applications.len() >= MAX_DISCOVERED_APPLICATIONS {
                return Ok(applications);
            }
            if let Some(application) = describe_shortcut(&path, &root, cache)? {
                applications.push(application);
            }
        }
    }
    Ok(applications)
}

fn launch_application(
    expected: &DriverLaunchableApplication,
    allow_foreground_activation: bool,
    cache: &mut ProvenanceCache,
) -> Result<(), DriverError> {
    let shortcut = PathBuf::from(&expected.launch_target);
    let roots = start_menu_roots().map_err(DriverError::mutation_not_dispatched)?;
    let mut current = None;
    for root in &roots {
        if let Some(application) = describe_shortcut(&shortcut, root, cache)
            .map_err(DriverError::mutation_not_dispatched)?
        {
            current = Some(application);
            break;
        }
    }
    let current = current.ok_or_else(|| {
        DriverError::new(
            DriverErrorKind::TargetUnavailable,
            "Windows Start Menu registration is no longer launchable",
        )
        .retryable("discover_launchable_applications")
        .mutation_not_dispatched()
    })?;
    if !expected.matches_identity(&current) {
        return Err(DriverError::new(
            DriverErrorKind::TargetUnavailable,
            "Windows Start Menu registration changed before launch",
        )
        .retryable("discover_launchable_applications")
        .mutation_not_dispatched());
    }

    let wide = wide_path(&shortcut);
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>()).unwrap_or(u32::MAX),
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC | SEE_MASK_NOCLOSEPROCESS | SEE_MASK_UNICODE,
        lpFile: PCWSTR(wide.as_ptr()),
        nShow: if allow_foreground_activation {
            SW_SHOWNORMAL.0
        } else {
            SW_SHOWNOACTIVATE.0
        },
        ..Default::default()
    };
    // SAFETY: The structure and UTF-16 path remain live for the synchronous
    // call. The process handle, when returned, is closed immediately because
    // exact process/window confirmation is performed by the discovery actor.
    unsafe {
        ShellExecuteExW(&raw mut execute).map_err(|_| {
            DriverError::new(
                DriverErrorKind::Platform,
                "Windows Shell rejected the reviewed Start Menu registration",
            )
            .retryable("discover_launchable_applications")
            .mutation_not_dispatched()
        })?;
        if !execute.hProcess.is_invalid() {
            let _ = CloseHandle(execute.hProcess);
        }
    }
    Ok(())
}

fn describe_shortcut(
    path: &Path,
    root: &Path,
    cache: &mut ProvenanceCache,
) -> Result<Option<DriverLaunchableApplication>, DriverError> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("lnk"))
    {
        return Ok(None);
    }
    let shortcut = match std::fs::canonicalize(path) {
        Ok(shortcut) if shortcut.starts_with(root) => shortcut,
        Ok(_) | Err(_) => return Ok(None),
    };
    match std::fs::symlink_metadata(&shortcut) {
        Ok(metadata) if metadata.is_file() && !is_reparse_point(&metadata) => {}
        Ok(_) | Err(_) => return Ok(None),
    }
    let Ok(target) = resolve_shortcut_target(&shortcut) else {
        return Ok(None);
    };
    let Ok(target) = std::fs::canonicalize(target) else {
        return Ok(None);
    };
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_file() && !is_reparse_point(&metadata) => {}
        Ok(_) | Err(_) => return Ok(None),
    }
    let identity = normalize_executable_identity(&target);
    if identity.starts_with(r"\\")
        || target
            .extension()
            .and_then(|extension| extension.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("exe"))
    {
        return Ok(None);
    }
    let shortcut_key = file_identity(&shortcut)?;
    let target_key = file_identity(&target)?;
    let cache_key = format!("{identity}:{target_key}");
    let (publisher, signature_status) = cache.inspect(cache_key, &identity);
    let name = shortcut
        .file_stem()
        .and_then(|name| name.to_str())
        .map_or_else(|| "Installed application".to_owned(), str::to_owned);
    Ok(Some(DriverLaunchableApplication {
        key: format!("shortcut:{shortcut_key}:target:{target_key}"),
        launch_target: shortcut.to_string_lossy().into_owned(),
        identity: identity.clone(),
        name,
        application_id: identity.clone(),
        provenance: ApplicationProvenance::Windows {
            executable_path: identity,
            publisher,
            signature_status,
        },
    }))
}

fn start_menu_roots() -> Result<Vec<PathBuf>, DriverError> {
    let mut roots = Vec::new();
    for folder in [&FOLDERID_Programs, &FOLDERID_CommonPrograms] {
        // SAFETY: Shell allocates the returned null-terminated path with the
        // COM allocator; it is copied and freed before the next iteration.
        let raw = unsafe { SHGetKnownFolderPath(folder, KNOWN_FOLDER_FLAG(0), None) }
            .map_err(|_| discovery_failure("cannot resolve a Windows Start Menu directory"))?;
        let value = unsafe { raw.to_string() };
        // SAFETY: `raw` came from SHGetKnownFolderPath and is freed exactly once.
        unsafe { CoTaskMemFree(Some(raw.0.cast::<c_void>())) };
        let value =
            value.map_err(|_| discovery_failure("Windows Start Menu path is not valid UTF-16"))?;
        let Ok(root) = std::fs::canonicalize(value) else {
            continue;
        };
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    if roots.is_empty() {
        return Err(discovery_failure(
            "no readable Windows Start Menu registration roots are available",
        ));
    }
    Ok(roots)
}

fn resolve_shortcut_target(shortcut: &Path) -> windows::core::Result<PathBuf> {
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }?;
    let persist: IPersistFile = link.cast()?;
    let shortcut = wide_path(shortcut);
    // SAFETY: Both COM objects belong to the current actor apartment and all
    // buffers remain live for the calls. No Resolve call is made, preventing
    // tracking, search, prompts, or network repair of a stale shortcut.
    unsafe { persist.Load(PCWSTR(shortcut.as_ptr()), STGM_READ) }?;
    let mut target = vec![0_u16; MAX_PATH_UNITS];
    let mut find_data = WIN32_FIND_DATAW::default();
    unsafe {
        link.GetPath(
            &mut target,
            &raw mut find_data,
            u32::try_from(SLGP_RAWPATH.0).unwrap_or_default(),
        )?;
    }
    let length = target.iter().position(|unit| *unit == 0).unwrap_or(0);
    if length == 0 {
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x8000_4005_u32.cast_signed()),
            "shortcut target is empty",
        ));
    }
    Ok(PathBuf::from(String::from_utf16_lossy(&target[..length])))
}

fn file_identity(path: &Path) -> Result<String, DriverError> {
    let wide = wide_path(path);
    // SAFETY: The normalized UTF-16 path remains live for CreateFileW. The
    // handle grants read-attributes only and is closed on every exit path.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|_| discovery_failure("Windows registration identity is unavailable"))?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: The live handle is read-only and the output structure is fully
    // initialized. CloseHandle is attempted regardless of query outcome.
    let query = unsafe { GetFileInformationByHandle(handle, &raw mut information) };
    let _ = unsafe { CloseHandle(handle) };
    query.map_err(|_| discovery_failure("Windows registration identity query failed"))?;
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    let last_write = (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(information.ftLastWriteTime.dwLowDateTime);
    let size = (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow);
    Ok(format!(
        "{:08x}:{index:016x}:{last_write:016x}:{size:016x}",
        information.dwVolumeSerialNumber
    ))
}

fn is_reparse_point(metadata: &Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn discovery_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message)
        .retryable("discover_launchable_applications")
}

fn actor_stopped<T>(_error: T) -> DriverError {
    DriverError::new(
        DriverErrorKind::Platform,
        "Windows launch actor stopped unexpectedly",
    )
}
