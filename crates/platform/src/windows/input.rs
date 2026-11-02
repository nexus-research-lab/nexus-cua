//! Serialized Windows foreground input actor backed by `SendInput`.

use std::ffi::c_void;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::Duration;

use nexus_cua_protocol::{
    ForegroundRestorationStatus, PointerButton, ScreenPoint, ScreenRect, SensitiveText,
};
use nexus_cua_runtime::{DriverError, DriverErrorKind};
use tokio::sync::oneshot;
use windows::Win32::Foundation::{
    CloseHandle, FILETIME, HWND, LPARAM, POINT, RECT, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    WPARAM,
};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    WaitForSingleObject,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyW,
    SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1,
    VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_F10, VK_F11, VK_F12, VK_HOME,
    VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindow, PostMessageW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SW_RESTORE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SWP_NOOWNERZORDER, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow, WM_CLOSE,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

const COMMAND_CAPACITY: usize = 32;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const WHEEL_DELTA: f64 = 120.0;
const TERMINATION_CONFIRM_TIMEOUT_MS: u32 = 2_000;
const FOREGROUND_RESTORE_TIMEOUT: Duration = Duration::from_millis(250);
const FOREGROUND_RESTORE_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone)]
pub(super) struct InputActor {
    sender: SyncSender<InputCommand>,
}

pub(super) enum InputAction {
    Activate,
    SetWindowBounds(ScreenRect),
    SetWindowMinimized(bool),
    TerminateApplication {
        pid: u32,
        process_generation: u64,
        windows: Vec<isize>,
    },
    Click {
        point: ScreenPoint,
        button: PointerButton,
        count: u8,
        background: bool,
    },
    Press {
        point: ScreenPoint,
        button: PointerButton,
        duration_ms: u32,
    },
    Move {
        point: ScreenPoint,
        duration_ms: u32,
    },
    TypeText(SensitiveText),
    PressKeys(Vec<String>),
    Scroll {
        point: ScreenPoint,
        delta_x: f64,
        delta_y: f64,
    },
    Drag {
        from: ScreenPoint,
        to: ScreenPoint,
        duration_ms: u32,
    },
}

impl InputActor {
    pub(super) fn spawn() -> Self {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        thread::Builder::new()
            .name("nexus-cua-windows-input".to_owned())
            .spawn(move || actor_loop(&receiver))
            .expect("create SendInput actor thread");
        Self { sender }
    }

    pub(super) async fn perform(
        &self,
        hwnd: isize,
        action: InputAction,
    ) -> Result<(), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(InputCommand::Perform {
                hwnd,
                action,
                reply,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "Windows input actor is busy")
                        .retryable("retry_with_backoff")
                        .mutation_not_dispatched()
                }
                TrySendError::Disconnected(_) => actor_stopped(()).mutation_not_dispatched(),
            })?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn perform_foreground(
        &self,
        hwnd: isize,
        pid: u32,
        process_generation: u64,
        action: InputAction,
        restore_foreground: bool,
        previous: Option<ForegroundOwner>,
    ) -> Result<ForegroundRestorationStatus, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(InputCommand::Foreground {
                hwnd,
                pid,
                process_generation,
                action,
                restore_foreground,
                previous,
                reply,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "Windows input actor is busy")
                        .retryable("retry_with_backoff")
                        .mutation_not_dispatched()
                }
                TrySendError::Disconnected(_) => actor_stopped(()).mutation_not_dispatched(),
            })?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn foreground_owner(&self) -> Result<Option<ForegroundOwner>, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(InputCommand::ForegroundOwner { reply })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "Windows input actor is busy")
                        .retryable("retry_with_backoff")
                        .mutation_not_dispatched()
                }
                TrySendError::Disconnected(_) => actor_stopped(()).mutation_not_dispatched(),
            })?;
        receiver.await.map_err(actor_stopped)?
    }
}

enum InputCommand {
    Perform {
        hwnd: isize,
        action: InputAction,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
    Foreground {
        hwnd: isize,
        pid: u32,
        process_generation: u64,
        action: InputAction,
        restore_foreground: bool,
        previous: Option<ForegroundOwner>,
        reply: oneshot::Sender<Result<ForegroundRestorationStatus, DriverError>>,
    },
    ForegroundOwner {
        reply: oneshot::Sender<Result<Option<ForegroundOwner>, DriverError>>,
    },
}

fn actor_loop(receiver: &Receiver<InputCommand>) {
    while let Ok(command) = receiver.recv() {
        match command {
            InputCommand::Perform {
                hwnd,
                action,
                reply,
            } => {
                let _ = reply.send(perform(hwnd, action));
            }
            InputCommand::Foreground {
                hwnd,
                pid,
                process_generation,
                action,
                restore_foreground,
                previous,
                reply,
            } => {
                let _ = reply.send(perform_foreground(
                    hwnd,
                    pid,
                    process_generation,
                    action,
                    restore_foreground,
                    previous,
                ));
            }
            InputCommand::ForegroundOwner { reply } => {
                let _ = reply.send(Ok(foreground_owner()));
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ForegroundOwner {
    hwnd: isize,
    pid: u32,
    process_generation: u64,
}

fn perform_foreground(
    raw_hwnd: isize,
    pid: u32,
    expected_generation: u64,
    action: InputAction,
    restore_foreground: bool,
    previous: Option<ForegroundOwner>,
) -> Result<ForegroundRestorationStatus, DriverError> {
    let target = hwnd(raw_hwnd);
    if window_process_generation(target) != Some((pid, expected_generation)) {
        return Err(DriverError::new(
            DriverErrorKind::TargetUnavailable,
            "target application process generation changed",
        )
        .mutation_not_dispatched());
    }
    if unsafe { GetForegroundWindow() } != target {
        activate(target)?;
    }
    let action_result = perform(raw_hwnd, action);
    let restoration = if !restore_foreground {
        ForegroundRestorationStatus::NotRequested
    } else if previous.is_some_and(|owner| owner.hwnd == raw_hwnd) {
        ForegroundRestorationStatus::NotNeeded
    } else {
        previous.map_or(ForegroundRestorationStatus::TargetUnavailable, |owner| {
            restore_foreground_owner(owner, pid, expected_generation)
        })
    };
    action_result.map(|()| restoration)
}

fn foreground_owner() -> Option<ForegroundOwner> {
    let foreground = unsafe { GetForegroundWindow() };
    let (pid, process_generation) = window_process_generation(foreground)?;
    Some(ForegroundOwner {
        hwnd: raw_hwnd(foreground),
        pid,
        process_generation,
    })
}

fn restore_foreground_owner(
    owner: ForegroundOwner,
    target_pid: u32,
    target_generation: u64,
) -> ForegroundRestorationStatus {
    let current = unsafe { GetForegroundWindow() };
    if window_process_generation(current) != Some((target_pid, target_generation)) {
        return ForegroundRestorationStatus::SkippedOwnerChanged;
    }
    let previous = hwnd(owner.hwnd);
    if !unsafe { IsWindow(Some(previous)) }.as_bool()
        || window_process_generation(previous) != Some((owner.pid, owner.process_generation))
    {
        return ForegroundRestorationStatus::TargetUnavailable;
    }
    if !unsafe { SetForegroundWindow(previous) }.as_bool() {
        return ForegroundRestorationStatus::Failed;
    }
    let deadline = std::time::Instant::now() + FOREGROUND_RESTORE_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if unsafe { GetForegroundWindow() } == previous {
            return ForegroundRestorationStatus::Restored;
        }
        thread::sleep(FOREGROUND_RESTORE_INTERVAL);
    }
    ForegroundRestorationStatus::Failed
}

fn window_process_generation(window: HWND) -> Option<(u32, u64)> {
    if window.0.is_null() {
        return None;
    }
    let mut pid = 0_u32;
    unsafe { GetWindowThreadProcessId(window, Some(&raw mut pid)) };
    (pid != 0)
        .then(|| process_generation(pid))
        .flatten()
        .map(|generation| (pid, generation))
}

fn process_generation(pid: u32) -> Option<u64> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let result = unsafe {
        GetProcessTimes(
            process,
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    result
        .ok()
        .map(|()| (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

fn perform(raw_hwnd: isize, action: InputAction) -> Result<(), DriverError> {
    let hwnd = hwnd(raw_hwnd);
    match action {
        InputAction::Activate => activate(hwnd),
        InputAction::SetWindowBounds(bounds) => set_window_bounds(hwnd, bounds),
        InputAction::SetWindowMinimized(minimized) => set_window_minimized(hwnd, minimized),
        InputAction::TerminateApplication {
            pid,
            process_generation,
            windows,
        } => terminate_application(pid, process_generation, &windows),
        InputAction::Click {
            point,
            button,
            count,
            background,
        } => {
            if background {
                click_background(hwnd, point, button, count)
            } else {
                click(point, button, count)
            }
        }
        InputAction::Press {
            point,
            button,
            duration_ms,
        } => press(point, button, duration_ms),
        InputAction::Move { point, duration_ms } => move_pointer(point, duration_ms),
        InputAction::TypeText(text) => type_text(hwnd, text.expose()),
        InputAction::PressKeys(keys) => press_keys(hwnd, &keys),
        InputAction::Scroll {
            point,
            delta_x,
            delta_y,
        } => scroll(hwnd, point, delta_x, delta_y),
        InputAction::Drag {
            from,
            to,
            duration_ms,
        } => drag(from, to, duration_ms),
    }
}

fn activate(hwnd: HWND) -> Result<(), DriverError> {
    // SAFETY: The HWND was freshly resolved from the process-lifetime key. The
    // caller verifies that Windows actually granted foreground ownership.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() || GetForegroundWindow() != hwnd {
            return Err(DriverError::new(
                DriverErrorKind::ForegroundRequired,
                "Windows refused foreground activation",
            ));
        }
    }
    Ok(())
}

fn set_window_bounds(hwnd: HWND, requested: ScreenRect) -> Result<(), DriverError> {
    let outer = outer_window_rect(hwnd)?;
    let extended = extended_window_rect(hwnd).unwrap_or(outer);
    let left_inset = extended.left.saturating_sub(outer.left);
    let top_inset = extended.top.saturating_sub(outer.top);
    let right_inset = outer.right.saturating_sub(extended.right);
    let bottom_inset = outer.bottom.saturating_sub(extended.bottom);
    let x = checked_i32(requested.x.round(), "window x coordinate")?.saturating_sub(left_inset);
    let y = checked_i32(requested.y.round(), "window y coordinate")?.saturating_sub(top_inset);
    let width = checked_i32(requested.width.round(), "window width")?
        .saturating_add(left_inset)
        .saturating_add(right_inset);
    let height = checked_i32(requested.height.round(), "window height")?
        .saturating_add(top_inset)
        .saturating_add(bottom_inset);
    // SAFETY: The HWND was freshly resolved and all dimensions were range
    // checked. NOACTIVATE/NOZORDER preserve foreground and stacking authority.
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
        )
        .map_err(|_| window_mutation_failure("Windows rejected the requested window bounds"))?;
    }
    let actual = extended_window_rect(hwnd).unwrap_or(outer_window_rect(hwnd)?);
    let actual = rect_to_screen_rect(actual);
    if !window_bounds_match(actual, requested) {
        return Err(window_mutation_failure(
            "Windows adjusted or rejected the requested window bounds",
        ));
    }
    Ok(())
}

fn set_window_minimized(hwnd: HWND, minimized: bool) -> Result<(), DriverError> {
    // SAFETY: The freshly resolved HWND is only shown or minimized. These
    // commands do not grant keyboard or pointer authority to the target.
    unsafe {
        let _ = ShowWindow(
            hwnd,
            if minimized {
                SW_SHOWMINNOACTIVE
            } else {
                SW_SHOWNOACTIVATE
            },
        );
        if IsIconic(hwnd).as_bool() != minimized {
            return Err(window_mutation_failure(
                "Windows did not apply the requested minimized state",
            ));
        }
    }
    Ok(())
}

fn terminate_application(
    pid: u32,
    expected_generation: u64,
    windows: &[isize],
) -> Result<(), DriverError> {
    if windows.is_empty() {
        return Err(DriverError::new(
            DriverErrorKind::TargetUnavailable,
            "target application has no current top-level windows",
        )
        .mutation_not_dispatched());
    }
    // SAFETY: The PID was resolved from the exact process-generation key. The
    // process handle is opened and its creation time is revalidated before any
    // close message is posted, preventing PID-reuse termination.
    unsafe {
        let process = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
        .map_err(|_| {
            DriverError::new(
                DriverErrorKind::TargetUnavailable,
                "target application generation is unavailable",
            )
            .mutation_not_dispatched()
        })?;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let generation = GetProcessTimes(
            process,
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
        .map(|()| (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime));
        let Ok(generation) = generation else {
            let _ = CloseHandle(process);
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "cannot revalidate target application generation",
            )
            .mutation_not_dispatched());
        };
        if generation != expected_generation {
            let _ = CloseHandle(process);
            return Err(DriverError::new(
                DriverErrorKind::TargetUnavailable,
                "target application process generation changed",
            )
            .mutation_not_dispatched());
        }

        let mut posted = 0_usize;
        for raw in windows {
            if PostMessageW(Some(hwnd(*raw)), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() {
                posted += 1;
            }
        }
        if posted == 0 {
            let _ = CloseHandle(process);
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "Windows rejected cooperative application termination",
            )
            .mutation_not_dispatched());
        }
        let wait = WaitForSingleObject(process, TERMINATION_CONFIRM_TIMEOUT_MS);
        let _ = CloseHandle(process);
        if wait == WAIT_OBJECT_0 {
            return Ok(());
        }
        let message = if wait == WAIT_TIMEOUT {
            "Windows accepted termination but the application did not exit in time"
        } else if wait == WAIT_FAILED {
            "Windows could not confirm application exit after termination"
        } else {
            "Windows returned an unknown application termination state"
        };
        Err(DriverError::new(DriverErrorKind::Platform, message)
            .retryable("verify_application_exit")
            .mutation_indeterminate())
    }
}

fn outer_window_rect(hwnd: HWND) -> Result<RECT, DriverError> {
    let mut rectangle = RECT::default();
    // SAFETY: GetWindowRect writes the initialized local RECT.
    unsafe {
        GetWindowRect(hwnd, &raw mut rectangle)
            .map_err(|_| window_mutation_failure("cannot read Windows window bounds"))?;
    }
    Ok(rectangle)
}

fn extended_window_rect(hwnd: HWND) -> Option<RECT> {
    let mut rectangle = RECT::default();
    // SAFETY: DwmGetWindowAttribute writes exactly one initialized RECT.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rectangle).cast::<c_void>(),
            u32::try_from(std::mem::size_of::<RECT>()).ok()?,
        )
        .ok()
        .map(|()| rectangle)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn checked_i32(value: f64, name: &str) -> Result<i32, DriverError> {
    if !value.is_finite() || value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
        return Err(DriverError::new(
            DriverErrorKind::Platform,
            format!("{name} is outside the native range"),
        )
        .mutation_not_dispatched());
    }
    Ok(value as i32)
}

fn rect_to_screen_rect(rectangle: RECT) -> ScreenRect {
    ScreenRect {
        x: f64::from(rectangle.left),
        y: f64::from(rectangle.top),
        width: f64::from(rectangle.right.saturating_sub(rectangle.left)),
        height: f64::from(rectangle.bottom.saturating_sub(rectangle.top)),
    }
}

fn window_bounds_match(actual: ScreenRect, requested: ScreenRect) -> bool {
    const TOLERANCE: f64 = 2.0;
    (actual.x - requested.x).abs() <= TOLERANCE
        && (actual.y - requested.y).abs() <= TOLERANCE
        && (actual.width - requested.width).abs() <= TOLERANCE
        && (actual.height - requested.height).abs() <= TOLERANCE
}

fn window_mutation_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).mutation_indeterminate()
}

fn click(point: ScreenPoint, button: PointerButton, count: u8) -> Result<(), DriverError> {
    let (down, up) = mouse_button_flags(button);
    let movement = absolute_mouse(point, MOUSEEVENTF_MOVE)?;
    let down = absolute_mouse(point, down)?;
    let up = absolute_mouse(point, up)?;
    send(&[movement])?;
    thread::sleep(FRAME_INTERVAL);
    for _ in 0..count {
        let pressed = send(&[down]);
        thread::sleep(Duration::from_millis(10));
        // Always attempt release, including an uncertain partial down sequence.
        let released = send(&[up]);
        pressed
            .and(released)
            .map_err(DriverError::mutation_indeterminate)?;
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn click_background(
    hwnd: HWND,
    point: ScreenPoint,
    button: PointerButton,
    count: u8,
) -> Result<(), DriverError> {
    let mut client = POINT {
        x: checked_i32(point.x.round(), "background click x coordinate")?,
        y: checked_i32(point.y.round(), "background click y coordinate")?,
    };
    // SAFETY: ScreenToClient writes the initialized local point for the freshly
    // resolved HWND and does not activate or move the system pointer.
    unsafe {
        ScreenToClient(hwnd, &raw mut client)
            .ok()
            .map_err(|_| background_unavailable("cannot map click into target client space"))?;
    }
    let x = i16::try_from(client.x)
        .map_err(|_| background_unavailable("background click x exceeds message range"))?;
    let y = i16::try_from(client.y)
        .map_err(|_| background_unavailable("background click y exceeds message range"))?;
    let packed = u32::from(u16::from_ne_bytes(x.to_ne_bytes()))
        | (u32::from(u16::from_ne_bytes(y.to_ne_bytes())) << 16);
    let lparam = LPARAM(isize::try_from(packed).unwrap_or(isize::MAX));
    let (down, up, button_state) = background_button_messages(button);
    for _ in 0..count {
        // SAFETY: PostMessageW only queues bounded pointer messages to the
        // freshly resolved target HWND. It does not touch the global input
        // queue or foreground window.
        unsafe {
            PostMessageW(Some(hwnd), down, WPARAM(button_state), lparam).map_err(|_| {
                background_unavailable("target rejected background pointer-down delivery")
            })?;
            PostMessageW(Some(hwnd), up, WPARAM(0), lparam).map_err(|_| {
                DriverError::new(
                    DriverErrorKind::Platform,
                    "target rejected background pointer-up delivery",
                )
                .mutation_indeterminate()
            })?;
        }
    }
    Ok(())
}

fn background_button_messages(button: PointerButton) -> (u32, u32, usize) {
    match button {
        PointerButton::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, 1),
        PointerButton::Middle => (WM_MBUTTONDOWN, WM_MBUTTONUP, 16),
        PointerButton::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, 2),
    }
}

fn background_unavailable(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::ForegroundRequired, message)
        .retryable("retry_with_foreground_delivery")
        .mutation_not_dispatched()
}

fn press(point: ScreenPoint, button: PointerButton, duration_ms: u32) -> Result<(), DriverError> {
    let (down, up) = mouse_button_flags(button);
    send(&[
        absolute_mouse(point, MOUSEEVENTF_MOVE)?,
        absolute_mouse(point, down)?,
    ])?;
    thread::sleep(Duration::from_millis(u64::from(duration_ms)));
    // Once the down sequence succeeds, always attempt the matching release
    // before returning from this serialized actor action.
    send(&[absolute_mouse(point, up)?])
}

fn move_pointer(target: ScreenPoint, duration_ms: u32) -> Result<(), DriverError> {
    let mut current = POINT::default();
    // SAFETY: GetCursorPos writes the initialized local POINT.
    unsafe {
        GetCursorPos(&raw mut current)
            .map_err(|_| input_failure("cannot read pointer position"))?;
    }
    interpolate(
        ScreenPoint {
            x: f64::from(current.x),
            y: f64::from(current.y),
        },
        target,
        duration_ms,
        MOUSEEVENTF_MOVE,
    )
}

fn drag(from: ScreenPoint, to: ScreenPoint, duration_ms: u32) -> Result<(), DriverError> {
    send(&[
        absolute_mouse(from, MOUSEEVENTF_MOVE)?,
        absolute_mouse(from, MOUSEEVENTF_LEFTDOWN)?,
    ])?;
    let drag_result = interpolate(from, to, duration_ms, MOUSEEVENTF_MOVE);
    let up_result = send(&[absolute_mouse(to, MOUSEEVENTF_LEFTUP)?]);
    drag_result.and(up_result)
}

fn interpolate(
    from: ScreenPoint,
    to: ScreenPoint,
    duration_ms: u32,
    flags: MOUSE_EVENT_FLAGS,
) -> Result<(), DriverError> {
    let steps = (duration_ms / 16).clamp(1, 625);
    for step in 1..=steps {
        let progress = f64::from(step) / f64::from(steps);
        send(&[absolute_mouse(
            ScreenPoint {
                x: from.x + (to.x - from.x) * progress,
                y: from.y + (to.y - from.y) * progress,
            },
            flags,
        )?])?;
        if step < steps {
            thread::sleep(FRAME_INTERVAL);
        }
    }
    Ok(())
}

fn type_text(target: HWND, text: &str) -> Result<(), DriverError> {
    let units = zeroize::Zeroizing::new(text.encode_utf16().collect::<Vec<_>>());
    let mut dispatched = false;
    for chunk in crate::input_text::chunks(&units) {
        if unsafe { GetForegroundWindow() } != target {
            let error = DriverError::new(
                DriverErrorKind::ForegroundRequired,
                "foreground owner changed during text input",
            );
            return Err(if dispatched {
                error.mutation_indeterminate()
            } else {
                error.mutation_not_dispatched()
            });
        }
        let mut inputs = Vec::with_capacity(chunk.len() * 2);
        for unit in chunk {
            inputs.push(keyboard_input(VIRTUAL_KEY(0), *unit, KEYEVENTF_UNICODE));
            inputs.push(keyboard_input(
                VIRTUAL_KEY(0),
                *unit,
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
            ));
        }
        let result = send(&inputs);
        inputs.fill(INPUT::default());
        result.map_err(|error| {
            if dispatched {
                error.mutation_indeterminate()
            } else {
                error
            }
        })?;
        dispatched = true;
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn press_keys(target: HWND, keys: &[String]) -> Result<(), DriverError> {
    if keys.len() == 1 && keys[0].len() == 1 && keys[0].is_ascii() {
        return type_text(target, &keys[0]);
    }
    // Query the target thread's layout, not the headless input actor's layout.
    let thread_id = unsafe { GetWindowThreadProcessId(target, None) };
    let layout =
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout(thread_id) };
    let mut modifiers = Vec::new();
    let mut primary = Vec::new();
    for key in keys {
        match nexus_cua_protocol::canonical_key(key) {
            "meta" | "win" => modifiers.push(VK_LWIN),
            "mod" | "control" => modifiers.push(VK_CONTROL),
            "alt" | "option" => modifiers.push(VK_MENU),
            "shift" => modifiers.push(VK_SHIFT),
            _ if key.len() == 1 && key.is_ascii() => {
                let mapped = unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanExW(
                        u16::from(key.as_bytes()[0]),
                        layout,
                    )
                };
                let mapped = u16::try_from(mapped).map_err(|_| unsupported_key(key))?;
                let required = mapped >> 8;
                if required & !7 != 0 {
                    return Err(unsupported_key(key));
                }
                for (mask, modifier) in [(1, VK_SHIFT), (2, VK_CONTROL), (4, VK_MENU)] {
                    if required & mask != 0 {
                        modifiers.push(modifier);
                    }
                }
                primary.push(VIRTUAL_KEY(mapped & 0xff));
            }
            _ => primary.push(
                key_code(nexus_cua_protocol::canonical_key(key))
                    .ok_or_else(|| unsupported_key(key))?,
            ),
        }
    }
    if primary.is_empty() {
        return Err(unsupported_key("modifier-only chord"));
    }
    let mut unique = Vec::new();
    modifiers.retain(|key| {
        if unique.contains(key) {
            false
        } else {
            unique.push(*key);
            true
        }
    });
    let mut inputs = Vec::with_capacity(modifiers.len() * 2 + primary.len() * 2);
    for modifier in &modifiers {
        inputs.push(keyboard_input(*modifier, 0, KEYBD_EVENT_FLAGS::default()));
    }
    for key in primary {
        inputs.push(keyboard_input(key, 0, KEYBD_EVENT_FLAGS::default()));
        inputs.push(keyboard_input(key, 0, KEYEVENTF_KEYUP));
    }
    for modifier in modifiers.into_iter().rev() {
        inputs.push(keyboard_input(modifier, 0, KEYEVENTF_KEYUP));
    }
    send(&inputs)
}

fn scroll(target: HWND, point: ScreenPoint, delta_x: f64, delta_y: f64) -> Result<(), DriverError> {
    let native_point = POINT {
        x: checked_i32(point.x.round(), "scroll x")?,
        y: checked_i32(point.y.round(), "scroll y")?,
    };
    // SAFETY: Read-only hit test immediately before serialized pointer dispatch.
    let hit = unsafe { windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(native_point) };
    let root = unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetAncestor(
            hit,
            windows::Win32::UI::WindowsAndMessaging::GA_ROOT,
        )
    };
    if root != target {
        return Err(DriverError::new(
            DriverErrorKind::TargetUnavailable,
            "scroll point is covered by another window",
        )
        .mutation_not_dispatched());
    }
    let mut inputs = Vec::with_capacity(3);
    inputs.push(absolute_mouse(point, MOUSEEVENTF_MOVE)?);
    if delta_y != 0.0 {
        inputs.push(relative_mouse(
            MOUSEEVENTF_WHEEL,
            u32::from_ne_bytes(wheel_delta(-delta_y, wheel_units(false)?).to_ne_bytes()),
        ));
    }
    if delta_x != 0.0 {
        inputs.push(relative_mouse(
            MOUSEEVENTF_HWHEEL,
            u32::from_ne_bytes(wheel_delta(delta_x, wheel_units(true)?).to_ne_bytes()),
        ));
    }
    send(&inputs)
}

fn absolute_mouse(point: ScreenPoint, flags: MOUSE_EVENT_FLAGS) -> Result<INPUT, DriverError> {
    let desktop = virtual_desktop()?;
    let dx = normalize_absolute(point.x, desktop.0, desktop.2);
    let dy = normalize_absolute(point.y, desktop.1, desktop.3);
    Ok(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    })
}

fn relative_mouse(flags: MOUSE_EVENT_FLAGS, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                mouseData: data,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn keyboard_input(virtual_key: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    // Unicode packets carry a UTF-16 unit, not a hardware scan code.
    let (scan, flags) = if flags.contains(KEYEVENTF_UNICODE) {
        (scan, flags)
    } else {
        let extended = matches!(
            virtual_key,
            VK_LEFT
                | VK_RIGHT
                | VK_UP
                | VK_DOWN
                | VK_HOME
                | VK_END
                | VK_PRIOR
                | VK_NEXT
                | VK_DELETE
                | VK_LWIN
        );
        // SAFETY: Read-only translation of a validated virtual key.
        let mapped = unsafe { MapVirtualKeyW(u32::from(virtual_key.0), MAPVK_VK_TO_VSC) };
        (
            u16::try_from(mapped & 0xff).unwrap_or(0),
            if extended {
                flags | KEYEVENTF_EXTENDEDKEY
            } else {
                flags
            },
        )
    };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<(), DriverError> {
    if inputs.is_empty() {
        return Ok(());
    }
    // SAFETY: SendInput reads the fully initialized contiguous INPUT slice.
    let sent = unsafe {
        SendInput(
            inputs,
            i32::try_from(size_of::<INPUT>()).unwrap_or(i32::MAX),
        )
    };
    if sent == u32::try_from(inputs.len()).unwrap_or(u32::MAX) {
        Ok(())
    } else if sent == 0 {
        Err(DriverError::new(
            DriverErrorKind::PermissionRequired,
            "Windows UIPI or the secure desktop blocked foreground input",
        )
        .retryable("run_host_at_matching_integrity"))
    } else {
        Err(DriverError::new(
            DriverErrorKind::Platform,
            "Windows accepted only part of the foreground input sequence",
        ))
    }
}

fn virtual_desktop() -> Result<(i32, i32, i32, i32), DriverError> {
    // SAFETY: GetSystemMetrics is read-only for these virtual desktop values.
    let metrics = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if metrics.2 <= 1 || metrics.3 <= 1 {
        Err(input_failure("virtual desktop geometry is unavailable"))
    } else {
        Ok(metrics)
    }
}

// The clamp proves that the rounded Win32 absolute coordinate fits in i32.
#[allow(clippy::cast_possible_truncation)]
fn normalize_absolute(value: f64, origin: i32, extent: i32) -> i32 {
    (((value - f64::from(origin)) * 65_535.0 / f64::from(extent - 1)).round()).clamp(0.0, 65_535.0)
        as i32
}

// Windows wheel messages express detents rather than pixels. Use the configured
// line/character count and a documented 16-logical-pixel unit; applications may
// quantize or override wheel behavior, so dispatch never proves scroll distance.
fn wheel_units(horizontal: bool) -> Result<u32, DriverError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETWHEELSCROLLCHARS, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        SystemParametersInfoW,
    };
    let mut units = 0_u32;
    // SAFETY: these read-only selectors write one initialized UINT.
    unsafe {
        SystemParametersInfoW(
            if horizontal {
                SPI_GETWHEELSCROLLCHARS
            } else {
                SPI_GETWHEELSCROLLLINES
            },
            0,
            Some((&raw mut units).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
        )
    }
    .map_err(|_| input_failure("cannot read Windows wheel settings").mutation_not_dispatched())?;
    if units == 0 || units == u32::MAX {
        return Err(DriverError::new(
            DriverErrorKind::Unsupported,
            "pixel scrolling requires line-based Windows wheel settings",
        )
        .mutation_not_dispatched());
    }
    Ok(units)
}

// The clamp proves that the rounded native wheel delta fits in i32.
#[allow(clippy::cast_possible_truncation)]
fn wheel_delta(value: f64, units: u32) -> i32 {
    (value * WHEEL_DELTA / (f64::from(units) * 16.0))
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

fn mouse_button_flags(button: PointerButton) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
    match button {
        PointerButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        PointerButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        PointerButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
    }
}

fn key_code(key: &str) -> Option<VIRTUAL_KEY> {
    Some(match key {
        "enter" | "return" => VK_RETURN,
        "tab" => VK_TAB,
        "space" => VK_SPACE,
        "backspace" => VK_BACK,
        "delete" => VK_DELETE,
        "escape" => VK_ESCAPE,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "home" => VK_HOME,
        "end" => VK_END,
        "page_up" => VK_PRIOR,
        "page_down" => VK_NEXT,
        "f1" => VK_F1,
        "f2" => VK_F2,
        "f3" => VK_F3,
        "f4" => VK_F4,
        "f5" => VK_F5,
        "f6" => VK_F6,
        "f7" => VK_F7,
        "f8" => VK_F8,
        "f9" => VK_F9,
        "f10" => VK_F10,
        "f11" => VK_F11,
        "f12" => VK_F12,
        _ => return None,
    })
}

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn raw_hwnd(hwnd: HWND) -> isize {
    isize::from_ne_bytes(hwnd.0.addr().to_ne_bytes())
}

fn unsupported_key(key: &str) -> DriverError {
    DriverError::new(
        DriverErrorKind::Unsupported,
        format!("unsupported normalized key: {key}"),
    )
}

fn input_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message)
}

fn actor_stopped<T>(_error: T) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, "Windows input actor stopped")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_keys_are_extended_and_unicode_packets_are_unchanged() {
        for key in [
            VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_HOME, VK_END, VK_PRIOR, VK_NEXT, VK_DELETE,
        ] {
            for flags in [KEYBD_EVENT_FLAGS::default(), KEYEVENTF_KEYUP] {
                let input = keyboard_input(key, 0, flags);
                // SAFETY: keyboard_input initializes the keyboard union member.
                let key_input = unsafe { input.Anonymous.ki };
                assert!(key_input.dwFlags.contains(KEYEVENTF_EXTENDEDKEY));
                assert_ne!(key_input.wScan, 0);
                assert_eq!(
                    key_input.dwFlags.contains(KEYEVENTF_KEYUP),
                    flags.contains(KEYEVENTF_KEYUP)
                );
            }
        }
        let unicode = keyboard_input(VIRTUAL_KEY(0), 0x4e2d, KEYEVENTF_UNICODE);
        let key_input = unsafe { unicode.Anonymous.ki };
        assert_eq!(key_input.wScan, 0x4e2d);
        assert_eq!(key_input.dwFlags, KEYEVENTF_UNICODE);
        let backspace = keyboard_input(VK_BACK, 0, KEYBD_EVENT_FLAGS::default());
        assert!(
            !unsafe { backspace.Anonymous.ki }
                .dwFlags
                .contains(KEYEVENTF_EXTENDEDKEY)
        );
    }
}
