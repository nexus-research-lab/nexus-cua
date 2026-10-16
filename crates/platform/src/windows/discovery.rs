//! Bounded `Win32` top-level window discovery actor.

use std::ffi::c_void;
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;

use nexus_cua_protocol::ScreenRect;
use nexus_cua_runtime::{DriverError, DriverErrorKind};
use tokio::sync::oneshot;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GWL_EXSTYLE, GetAncestor, GetForegroundWindow, GetWindowLongW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, MONITORINFOF_PRIMARY, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

const COMMAND_CAPACITY: usize = 32;
const MAX_PATH_UNITS: usize = 32_768;

#[derive(Clone, Debug)]
pub(super) struct NativeWindow {
    pub(super) key: String,
    pub(super) hwnd: isize,
    pub(super) pid: u32,
    pub(super) process_generation: u64,
    pub(super) application_key: String,
    pub(super) application_id: String,
    pub(super) application_name: String,
    pub(super) title: String,
    pub(super) screen_bounds: ScreenRect,
    pub(super) foreground: bool,
    pub(super) minimized: bool,
    pub(super) visible: bool,
}

#[derive(Clone, Debug)]
pub(super) struct NativeDisplay {
    pub(super) key: String,
    pub(super) screen_bounds: ScreenRect,
    pub(super) primary: bool,
}

#[derive(Clone)]
pub(super) struct DiscoveryActor {
    sender: SyncSender<DiscoveryCommand>,
}

impl DiscoveryActor {
    pub(super) fn spawn() -> Self {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        thread::Builder::new()
            .name("nexus-cua-windows-discovery".to_owned())
            .spawn(move || actor_loop(&receiver))
            .expect("create Win32 discovery actor thread");
        Self { sender }
    }

    pub(super) async fn windows(&self) -> Result<Vec<NativeWindow>, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(DiscoveryCommand::ListWindows { reply })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "Windows discovery actor is busy")
                        .retryable("retry_with_backoff")
                }
                TrySendError::Disconnected(_) => actor_stopped(()),
            })?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn displays(&self) -> Result<Vec<NativeDisplay>, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(DiscoveryCommand::ListDisplays { reply })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "Windows discovery actor is busy")
                        .retryable("retry_with_backoff")
                }
                TrySendError::Disconnected(_) => actor_stopped(()),
            })?;
        receiver.await.map_err(actor_stopped)?
    }
}

enum DiscoveryCommand {
    ListWindows {
        reply: oneshot::Sender<Result<Vec<NativeWindow>, DriverError>>,
    },
    ListDisplays {
        reply: oneshot::Sender<Result<Vec<NativeDisplay>, DriverError>>,
    },
}

fn actor_loop(receiver: &Receiver<DiscoveryCommand>) {
    while let Ok(command) = receiver.recv() {
        match command {
            DiscoveryCommand::ListWindows { reply } => {
                let _ = reply.send(list_windows());
            }
            DiscoveryCommand::ListDisplays { reply } => {
                let _ = reply.send(list_displays());
            }
        }
    }
}

fn list_displays() -> Result<Vec<NativeDisplay>, DriverError> {
    let mut handles = Vec::<isize>::new();
    // SAFETY: EnumDisplayMonitors invokes the callback synchronously while the
    // uniquely borrowed Vec remains live.
    let completed = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(ptr_to_lparam(&mut handles)),
        )
    };
    if !completed.as_bool() {
        return Err(native_failure("EnumDisplayMonitors failed"));
    }
    handles
        .into_iter()
        .map(describe_display)
        .collect::<Result<Vec<_>, _>>()
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _bounds: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: `lparam` originates from `list_displays` and the callback is
    // synchronous, so the Vec remains uniquely borrowed.
    let address = usize::from_ne_bytes(lparam.0.to_ne_bytes());
    let displays = unsafe { &mut *std::ptr::with_exposed_provenance_mut::<Vec<isize>>(address) };
    displays.push(isize::from_ne_bytes(monitor.0.addr().to_ne_bytes()));
    true.into()
}

fn describe_display(raw_monitor: isize) -> Result<NativeDisplay, DriverError> {
    let monitor = HMONITOR(raw_monitor as *mut c_void);
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(u32::MAX),
        ..Default::default()
    };
    // SAFETY: GetMonitorInfoW copies metadata for the enumerated monitor into
    // the initialized fixed-size structure.
    if !unsafe { GetMonitorInfoW(monitor, &raw mut info) }.as_bool() {
        return Err(native_failure("GetMonitorInfoW failed"));
    }
    let bounds = info.rcMonitor;
    Ok(NativeDisplay {
        key: format!("hmonitor:{:016x}", monitor.0.addr()),
        screen_bounds: ScreenRect {
            x: f64::from(bounds.left),
            y: f64::from(bounds.top),
            width: f64::from(bounds.right.saturating_sub(bounds.left)),
            height: f64::from(bounds.bottom.saturating_sub(bounds.top)),
        },
        primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

fn list_windows() -> Result<Vec<NativeWindow>, DriverError> {
    let mut handles = Vec::<isize>::new();
    // SAFETY: The callback only copies HWND values into the live Vec passed by
    // pointer, and EnumWindows invokes it synchronously.
    unsafe {
        EnumWindows(Some(collect_window), LPARAM(ptr_to_lparam(&mut handles)))
            .map_err(|_| native_failure("EnumWindows failed"))?;
    }
    let foreground = unsafe { GetForegroundWindow() };
    Ok(handles
        .into_iter()
        .filter_map(|raw| describe_window(hwnd(raw), foreground).ok().flatten())
        .collect())
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` originates from `list_windows` and EnumWindows is
    // synchronous, so the Vec remains uniquely borrowed for this callback.
    let address = usize::from_ne_bytes(lparam.0.to_ne_bytes());
    let windows = unsafe { &mut *std::ptr::with_exposed_provenance_mut::<Vec<isize>>(address) };
    windows.push(raw_hwnd(hwnd));
    true.into()
}

fn describe_window(hwnd: HWND, foreground: HWND) -> Result<Option<NativeWindow>, DriverError> {
    // SAFETY: All calls consume a copied HWND and write only initialized local
    // buffers. A disappearing window is treated as a skipped enumeration row.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool()
            || GetAncestor(hwnd, GA_ROOT) != hwnd
            || (u32::from_ne_bytes(GetWindowLongW(hwnd, GWL_EXSTYLE).to_ne_bytes())
                & WS_EX_TOOLWINDOW.0)
                != 0
        {
            return Ok(None);
        }
        let mut cloaked = 0_u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast::<c_void>(),
            u32::try_from(size_of::<u32>()).unwrap_or(u32::MAX),
        )
        .is_ok()
            && cloaked != 0
        {
            return Ok(None);
        }
        let title = window_title(hwnd);
        let bounds = window_bounds(hwnd)?;
        if bounds.width <= 1.0 || bounds.height <= 1.0 {
            return Ok(None);
        }
        let mut pid = 0_u32;
        GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
        if pid == 0 {
            return Ok(None);
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|_| native_failure("cannot inspect window process"))?;
        let identity = process_identity(process, pid);
        let _ = CloseHandle(process);
        let (application_id, application_name, generation) = identity?;
        let application_key = format!("pid:{pid}:start:{generation:016x}");
        Ok(Some(NativeWindow {
            key: format!("hwnd:{:016x}:{application_key}", hwnd.0.addr()),
            hwnd: raw_hwnd(hwnd),
            pid,
            process_generation: generation,
            application_key,
            application_id,
            application_name,
            title,
            screen_bounds: bounds,
            foreground: hwnd == foreground,
            minimized: IsIconic(hwnd).as_bool(),
            visible: true,
        }))
    }
}

fn process_identity(
    process: windows::Win32::Foundation::HANDLE,
    pid: u32,
) -> Result<(String, String, u64), DriverError> {
    let mut buffer = vec![0_u16; MAX_PATH_UNITS];
    let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: The process handle is live; every output buffer is initialized
    // and correctly sized for the duration of these calls.
    unsafe {
        QueryFullProcessImageNameW(
            process,
            windows::Win32::System::Threading::PROCESS_NAME_FORMAT::default(),
            PWSTR(buffer.as_mut_ptr()),
            &raw mut length,
        )
        .map_err(|_| native_failure("cannot read process image path"))?;
        GetProcessTimes(
            process,
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
        .map_err(|_| native_failure("cannot read process generation"))?;
    }
    buffer.truncate(usize::try_from(length).unwrap_or(0));
    let path = String::from_utf16_lossy(&buffer);
    let application_name = Path::new(&path)
        .file_stem()
        .and_then(|name| name.to_str())
        .map_or_else(|| format!("Process {pid}"), str::to_owned);
    let generation = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    Ok((
        normalize_executable_identity(Path::new(&path)),
        application_name,
        generation,
    ))
}

pub(super) fn normalize_executable_identity(path: &Path) -> String {
    let path = path.to_string_lossy().replace('/', "\\");
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}").to_lowercase()
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_lowercase()
    }
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: The mutable UTF-16 buffer is sized from the current title length;
    // races only truncate the user-visible title and cannot overflow the slice.
    unsafe {
        let length = usize::try_from(GetWindowTextLengthW(hwnd).max(0)).unwrap_or(0);
        let mut buffer = vec![0_u16; length.saturating_add(1)];
        let copied = usize::try_from(GetWindowTextW(hwnd, &mut buffer).max(0)).unwrap_or(0);
        String::from_utf16_lossy(&buffer[..copied.min(buffer.len())])
    }
}

fn window_bounds(hwnd: HWND) -> Result<ScreenRect, DriverError> {
    let mut rectangle = RECT::default();
    // SAFETY: DWM/GetWindowRect write a RECT into the initialized local.
    unsafe {
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rectangle).cast::<c_void>(),
            u32::try_from(size_of::<RECT>()).unwrap_or(u32::MAX),
        )
        .is_err()
        {
            GetWindowRect(hwnd, &raw mut rectangle)
                .map_err(|_| native_failure("cannot read window bounds"))?;
        }
    }
    Ok(ScreenRect {
        x: f64::from(rectangle.left),
        y: f64::from(rectangle.top),
        width: f64::from(rectangle.right.saturating_sub(rectangle.left)),
        height: f64::from(rectangle.bottom.saturating_sub(rectangle.top)),
    })
}

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn raw_hwnd(hwnd: HWND) -> isize {
    isize::from_ne_bytes(hwnd.0.addr().to_ne_bytes())
}

fn ptr_to_lparam<T>(value: &mut T) -> isize {
    let address = std::ptr::from_mut(value).addr();
    isize::from_ne_bytes(address.to_ne_bytes())
}

fn native_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).retryable("list_windows")
}

fn actor_stopped<T>(_error: T) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, "Windows discovery actor stopped")
}

#[cfg(test)]
mod tests {
    use super::normalize_executable_identity;
    use std::path::Path;

    #[test]
    fn executable_identity_normalizes_verbatim_drive_paths() {
        assert_eq!(
            normalize_executable_identity(Path::new(r"\\?\C:\Program Files\Fixture\APP.EXE")),
            r"c:\program files\fixture\app.exe"
        );
    }

    #[test]
    fn executable_identity_preserves_unc_authority() {
        assert_eq!(
            normalize_executable_identity(Path::new(r"\\?\UNC\server\share\APP.EXE")),
            r"\\server\share\app.exe"
        );
    }
}
