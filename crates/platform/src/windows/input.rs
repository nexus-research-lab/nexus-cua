//! Serialized Windows foreground input actor backed by SendInput.

use std::ffi::c_void;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::Duration;

use nexus_cua_protocol::{PointerButton, ScreenPoint, SensitiveText};
use nexus_cua_runtime::{DriverError, DriverErrorKind};
use tokio::sync::oneshot;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
    MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_F10, VK_F11, VK_F12, VK_HOME,
    VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetSystemMetrics, IsIconic, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE, SetForegroundWindow,
    ShowWindow,
};

const COMMAND_CAPACITY: usize = 32;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const WHEEL_DELTA: f64 = 120.0;

#[derive(Clone)]
pub(super) struct InputActor {
    sender: SyncSender<InputCommand>,
}

pub(super) enum InputAction {
    Activate,
    Click {
        point: ScreenPoint,
        button: PointerButton,
        count: u8,
    },
    Move {
        point: ScreenPoint,
        duration_ms: u32,
    },
    TypeText(SensitiveText),
    PressKeys(Vec<String>),
    Scroll {
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
            .spawn(move || actor_loop(receiver))
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
            .try_send(InputCommand {
                hwnd,
                action,
                reply,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "Windows input actor is busy")
                        .retryable("retry_with_backoff")
                }
                TrySendError::Disconnected(_) => actor_stopped(()),
            })?;
        receiver.await.map_err(actor_stopped)?
    }
}

struct InputCommand {
    hwnd: isize,
    action: InputAction,
    reply: oneshot::Sender<Result<(), DriverError>>,
}

fn actor_loop(receiver: Receiver<InputCommand>) {
    while let Ok(command) = receiver.recv() {
        let result = perform(command.hwnd, command.action);
        let _ = command.reply.send(result);
    }
}

fn perform(raw_hwnd: isize, action: InputAction) -> Result<(), DriverError> {
    let hwnd = hwnd(raw_hwnd);
    match action {
        InputAction::Activate => activate(hwnd),
        InputAction::Click {
            point,
            button,
            count,
        } => click(point, button, count),
        InputAction::Move { point, duration_ms } => move_pointer(point, duration_ms),
        InputAction::TypeText(text) => type_text(text.expose()),
        InputAction::PressKeys(keys) => press_keys(&keys),
        InputAction::Scroll { delta_x, delta_y } => scroll(delta_x, delta_y),
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
            ShowWindow(hwnd, SW_RESTORE);
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

fn click(point: ScreenPoint, button: PointerButton, count: u8) -> Result<(), DriverError> {
    let (down, up) = mouse_button_flags(button);
    let mut inputs = Vec::with_capacity(usize::from(count) * 2 + 1);
    inputs.push(absolute_mouse(point, MOUSEEVENTF_MOVE)?);
    for _ in 0..count {
        inputs.push(absolute_mouse(point, down)?);
        inputs.push(absolute_mouse(point, up)?);
    }
    send(&inputs)
}

fn move_pointer(target: ScreenPoint, duration_ms: u32) -> Result<(), DriverError> {
    let mut current = POINT::default();
    // SAFETY: GetCursorPos writes the initialized local POINT.
    unsafe {
        GetCursorPos(&mut current).map_err(|_| input_failure("cannot read pointer position"))?;
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

fn type_text(text: &str) -> Result<(), DriverError> {
    let mut inputs = Vec::with_capacity(text.encode_utf16().count() * 2);
    for unit in text.encode_utf16() {
        inputs.push(keyboard_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
        inputs.push(keyboard_input(
            VIRTUAL_KEY(0),
            unit,
            KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
        ));
    }
    send(&inputs)
}

fn press_keys(keys: &[String]) -> Result<(), DriverError> {
    let mut modifiers = Vec::new();
    let mut primary = Vec::new();
    for key in keys {
        match key.as_str() {
            "meta" | "command" => modifiers.push(VK_LWIN),
            "control" => modifiers.push(VK_CONTROL),
            "alt" | "option" => modifiers.push(VK_MENU),
            "shift" => modifiers.push(VK_SHIFT),
            _ => primary.push(key_code(key).ok_or_else(|| unsupported_key(key))?),
        }
    }
    if primary.is_empty() {
        return Err(unsupported_key("modifier-only chord"));
    }
    let mut inputs = Vec::with_capacity(modifiers.len() * 2 + primary.len() * 2);
    for modifier in &modifiers {
        inputs.push(keyboard_input(*modifier, 0, Default::default()));
    }
    for key in primary {
        inputs.push(keyboard_input(key, 0, Default::default()));
        inputs.push(keyboard_input(key, 0, KEYEVENTF_KEYUP));
    }
    for modifier in modifiers.into_iter().rev() {
        inputs.push(keyboard_input(modifier, 0, KEYEVENTF_KEYUP));
    }
    send(&inputs)
}

fn scroll(delta_x: f64, delta_y: f64) -> Result<(), DriverError> {
    let mut inputs = Vec::with_capacity(2);
    if delta_y != 0.0 {
        inputs.push(relative_mouse(
            MOUSEEVENTF_WHEEL,
            wheel_delta(delta_y) as u32,
        ));
    }
    if delta_x != 0.0 {
        inputs.push(relative_mouse(
            MOUSEEVENTF_HWHEEL,
            wheel_delta(delta_x) as u32,
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

fn keyboard_input(
    virtual_key: VIRTUAL_KEY,
    scan: u16,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
) -> INPUT {
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
    } else {
        Err(DriverError::new(
            DriverErrorKind::Platform,
            "SendInput was rejected or blocked by Windows UIPI",
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

fn normalize_absolute(value: f64, origin: i32, extent: i32) -> i32 {
    (((value - f64::from(origin)) * 65_535.0 / f64::from(extent - 1)).round()).clamp(0.0, 65_535.0)
        as i32
}

fn wheel_delta(value: f64) -> i32 {
    (value * WHEEL_DELTA)
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
    if key.len() == 1 {
        let byte = key.as_bytes()[0];
        if byte.is_ascii_lowercase() {
            return Some(VIRTUAL_KEY(u16::from(byte.to_ascii_uppercase())));
        }
        if byte.is_ascii_digit() {
            return Some(VIRTUAL_KEY(u16::from(byte)));
        }
    }
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
