//! Bounded Win32 top-level window discovery actor.

use std::ffi::c_void;
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;

use nexus_cua_protocol::ScreenRect;
use nexus_cua_runtime::{DriverError, DriverErrorKind};
use tokio::sync::oneshot;
use windows::Win32::Foundation::{BOOL, CloseHandle, FILETIME, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GA_ROOT, GWL_EXSTYLE, GetAncestor, GetForegroundWindow, GetWindowLongW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::PWSTR;

const COMMAND_CAPACITY: usize = 32;
const MAX_PATH_UNITS: usize = 32_768;

#[derive(Clone, Debug)]
pub(super) struct NativeWindow {
    pub(super) key: String,
    pub(super) hwnd: isize,
    pub(super) pid: u32,
    pub(super) application_key: String,
    pub(super) application_id: String,
    pub(super) application_name: String,
    pub(super) title: String,
    pub(super) screen_bounds: ScreenRect,
    pub(super) foreground: bool,
    pub(super) minimized: bool,
    pub(super) visible: bool,
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
            .spawn(move || actor_loop(receiver))
            .expect("create Win32 discovery actor thread");
        Self { sender }
    }

    pub(super) async fn windows(&self) -> Result<Vec<NativeWindow>, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(DiscoveryCommand::List { reply })
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
    List {
        reply: oneshot::Sender<Result<Vec<NativeWindow>, DriverError>>,
    },
}

fn actor_loop(receiver: Receiver<DiscoveryCommand>) {
    while let Ok(DiscoveryCommand::List { reply }) = receiver.recv() {
        let _ = reply.send(list_windows());
    }
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
    let windows = unsafe { &mut *(lparam.0 as *mut Vec<isize>) };
    windows.push(hwnd.0 as isize);
    true.into()
}

fn describe_window(hwnd: HWND, foreground: HWND) -> Result<Option<NativeWindow>, DriverError> {
    // SAFETY: All calls consume a copied HWND and write only initialized local
    // buffers. A disappearing window is treated as a skipped enumeration row.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool()
            || GetAncestor(hwnd, GA_ROOT) != hwnd
            || (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0) != 0
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
            key: format!("hwnd:{:016x}:{application_key}", hwnd.0 as usize),
            hwnd: hwnd.0 as isize,
            pid,
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
            Default::default(),
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
        .map_err(|_| native_failure("cannot read process image path"))?;
        GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user)
            .map_err(|_| native_failure("cannot read process generation"))?;
    }
    buffer.truncate(usize::try_from(length).unwrap_or(0));
    let path = String::from_utf16_lossy(&buffer);
    let application_name = Path::new(&path)
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Process {pid}"));
    let generation = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    Ok((path.to_lowercase(), application_name, generation))
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: The mutable UTF-16 buffer is sized from the current title length;
    // races only truncate the user-visible title and cannot overflow the slice.
    unsafe {
        let length = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buffer = vec![0_u16; length.saturating_add(1)];
        let copied = GetWindowTextW(hwnd, &mut buffer).max(0) as usize;
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
            GetWindowRect(hwnd, &mut rectangle)
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

fn ptr_to_lparam<T>(value: &mut T) -> isize {
    value as *mut T as isize
}

fn native_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).retryable("list_windows")
}

fn actor_stopped<T>(_error: T) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, "Windows discovery actor stopped")
}
