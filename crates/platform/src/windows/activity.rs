//! Passive, content-free input monitoring on a dedicated Windows message pump.
//!
//! Raw Input is the physical-input source (unlike low-level hooks it cannot be
//! silently removed for a slow callback). Hooks additionally observe foreign
//! injected input. No characters, coordinates or event history are retained.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use nexus_cua_runtime::{DriverActionGuard, DriverError, DriverErrorKind};
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, GetUserObjectInformationW,
    OpenInputDesktop, UOI_NAME,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::Input::{
    GetRawInputData, GetRegisteredRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
    RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HHOOK,
    HWND_MESSAGE, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_REMOVE, PeekMessageW,
    RegisterClassW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP, WNDCLASSW,
};
use windows::core::w;

const CAPACITY: usize = 256;
const TICK: Duration = Duration::from_millis(8);
const DEADLINE: Duration = Duration::from_millis(500);

// An identification marker, never authority. Process-specific to avoid ignoring
// input from another runtime instance. Raw physical input never uses this tag.
pub(super) fn event_marker() -> usize {
    (0x4e58_4355_u32 ^ std::process::id().rotate_left(16)) as usize
}

#[derive(Clone)]
pub(super) struct ActivityActor {
    sender: mpsc::SyncSender<Registration>,
}

struct Registration {
    guard: DriverActionGuard,
    reply: mpsc::SyncSender<Result<(), DriverError>>,
}

#[derive(Default)]
struct State {
    guards: Vec<DriverActionGuard>,
    physical: PressedInputs,
    injected: PressedInputs,
    heartbeat: Option<Instant>,
}

#[derive(Default)]
struct PressedInputs([u64; 4]);

impl PressedInputs {
    fn update(&mut self, key: usize, down: bool) {
        // Raw keyboard modifier codes are generic; normalize initial L/R state.
        let key = match key {
            0xa0 | 0xa1 => 0x10,
            0xa2 | 0xa3 => 0x11,
            0xa4 | 0xa5 => 0x12,
            value => value,
        };
        if key < 256 {
            let bit = 1 << (key % 64);
            if down {
                self.0[key / 64] |= bit;
            } else {
                self.0[key / 64] &= !bit;
            }
        }
    }

    fn held(&self) -> bool {
        self.0.iter().any(|bits| *bits != 0)
    }
}

impl State {
    fn record(&self) {
        let held = self.physical.held() || self.injected.held();
        for guard in &self.guards {
            guard.record_user_input_state(held);
        }
    }

    fn fail(&self) {
        for guard in &self.guards {
            guard.activity_monitor_failed();
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<Arc<Mutex<State>>>> = const { RefCell::new(None) };
}

fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|slot| {
        if let Some(state) = slot.borrow().as_ref() {
            f(&mut state.lock().expect("content-free input state"));
        }
    });
}

impl ActivityActor {
    pub(super) fn spawn() -> Result<Self, DriverError> {
        let (sender, receiver) = mpsc::sync_channel(32);
        std::thread::Builder::new()
            .name("nexus-cua-windows-activity".to_owned())
            .spawn(move || run(&receiver))
            .map_err(|_| unavailable())?;
        Ok(Self { sender })
    }

    pub(super) fn register(&self, guard: &DriverActionGuard) -> Result<(), DriverError> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(Registration {
                guard: guard.clone(),
                reply,
            })
            .map_err(|_| unavailable())?;
        receiver.recv_timeout(DEADLINE).unwrap_or_else(|_| {
            guard.activity_monitor_failed();
            Err(unavailable())
        })
    }
}

fn run(receiver: &mpsc::Receiver<Registration>) {
    while let Ok(first) = receiver.recv() {
        if let Err(error) = first.guard.check() {
            let _ = first.reply.send(Err(error));
            continue;
        }
        let state = Arc::new(Mutex::new(State::default()));
        STATE.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&state)));
        let Ok(monitor) = Monitor::new() else {
            let _ = first.reply.send(Err(unavailable()));
            STATE.with(|slot| *slot.borrow_mut() = None);
            continue;
        };
        // A bounded watchdog makes a stopped/starved pump fail closed even
        // while a native input actor is holding a button or blocked in UIA.
        let weak = Arc::downgrade(&state);
        let watchdog = std::thread::Builder::new()
            .name("nexus-cua-input-health".to_owned())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(25));
                    let Some(state) = weak.upgrade() else { break };
                    let state = state.lock().expect("content-free input state");
                    let Some(heartbeat) = state.heartbeat else {
                        break;
                    };
                    if heartbeat.elapsed() >= DEADLINE {
                        state.fail();
                    }
                }
            });
        if watchdog.is_err() {
            let _ = first.reply.send(Err(unavailable()));
        } else {
            with_state(|state| {
                state.heartbeat = Some(Instant::now());
                for key in 1..256 {
                    if unsafe { GetAsyncKeyState(key) } < 0 {
                        state
                            .physical
                            .update(usize::try_from(key).unwrap_or(0), true);
                    }
                }
                admit(state, first);
            });
            monitor.pump(receiver);
        }
        with_state(|state| {
            state.fail();
            state.heartbeat = None;
        });
        drop(monitor);
        STATE.with(|slot| *slot.borrow_mut() = None);
        if let Ok(watchdog) = watchdog {
            let _ = watchdog.join();
        }
    }
}

fn admit(state: &mut State, registration: Registration) {
    state.guards.retain(|guard| guard.check().is_ok());
    let result = registration.guard.check().and_then(|()| {
        if state.guards.len() >= CAPACITY {
            return Err(unavailable());
        }
        if state.physical.held() || state.injected.held() {
            registration.guard.record_user_input_state(true);
        }
        state.guards.push(registration.guard);
        Ok(())
    });
    let _ = registration.reply.send(result);
}

struct Monitor {
    window: HWND,
    keyboard: HHOOK,
    mouse: HHOOK,
}

impl Monitor {
    fn new() -> Result<Self, DriverError> {
        if !input_desktop_available() {
            return Err(unavailable());
        }
        // SAFETY: Window, hooks and Raw Input registration are owned and freed
        // by this pump thread; callbacks retain no native objects.
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None).map_err(|_| unavailable())?.0);
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: w!("NexusCuaInputMonitor"),
                ..Default::default()
            };
            let _ = RegisterClassW(&raw const class); // The class may exist from a prior session.
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class.lpszClassName,
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance),
                None,
            )
            .map_err(|_| unavailable())?;
            let mut monitor = Self {
                window,
                keyboard: HHOOK::default(),
                mouse: HHOOK::default(),
            };
            RegisterRawInputDevices(
                &devices(window, false),
                u32::try_from(size_of::<RAWINPUTDEVICE>()).expect("fixed Win32 structure"),
            )
            .map_err(|_| unavailable())?;
            monitor.keyboard =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(instance), 0)
                    .map_err(|_| unavailable())?;
            monitor.mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(instance), 0)
                .map_err(|_| unavailable())?;
            Ok(monitor)
        }
    }

    fn registered(&self) -> bool {
        let mut devices = [RAWINPUTDEVICE::default(); 4];
        let mut count = 4;
        let read = unsafe {
            GetRegisteredRawInputDevices(
                Some(devices.as_mut_ptr()),
                &raw mut count,
                u32::try_from(size_of::<RAWINPUTDEVICE>()).expect("fixed Win32 structure"),
            )
        };
        read != u32::MAX
            && [2, 6].iter().all(|usage| {
                devices.iter().any(|device| {
                    device.usUsagePage == 1
                        && device.usUsage == *usage
                        && device.hwndTarget == self.window
                        && device.dwFlags.contains(RIDEV_INPUTSINK)
                })
            })
    }

    fn pump(&self, receiver: &mpsc::Receiver<Registration>) {
        loop {
            let mut message = MSG::default();
            // Bound each drain so a busy device cannot starve revocation/health.
            for _ in 0..256 {
                if !unsafe { PeekMessageW(&raw mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                    break;
                }
                unsafe {
                    DispatchMessageW(&raw const message);
                }
            }
            if !input_desktop_available() || !self.registered() {
                with_state(|state| state.fail());
                return;
            }
            let mut active = false;
            with_state(|state| {
                state.heartbeat = Some(Instant::now());
                state.guards.retain(|guard| guard.check().is_ok());
                active = !state.guards.is_empty();
            });
            if !active {
                return;
            }
            match receiver.recv_timeout(TICK) {
                Ok(registration) => with_state(|state| admit(state, registration)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        unsafe {
            if !self.keyboard.0.is_null() {
                let _ = UnhookWindowsHookEx(self.keyboard);
            }
            if !self.mouse.0.is_null() {
                let _ = UnhookWindowsHookEx(self.mouse);
            }
            let _ = RegisterRawInputDevices(
                &devices(HWND::default(), true),
                u32::try_from(size_of::<RAWINPUTDEVICE>()).expect("fixed Win32 structure"),
            );
            let _ = DestroyWindow(self.window);
        }
    }
}

fn devices(window: HWND, remove: bool) -> [RAWINPUTDEVICE; 2] {
    [2, 6].map(|usage| RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: usage,
        dwFlags: if remove {
            RIDEV_REMOVE
        } else {
            RIDEV_INPUTSINK
        },
        hwndTarget: window,
    })
}

pub(super) fn input_desktop_available() -> bool {
    unsafe {
        let Ok(desktop) =
            OpenInputDesktop(DESKTOP_CONTROL_FLAGS::default(), false, DESKTOP_READOBJECTS)
        else {
            return false;
        };
        let mut name = [0_u16; 64];
        let result = GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            u32::try_from(size_of_val(&name)).expect("fixed desktop name buffer"),
            None,
        );
        let _ = CloseDesktop(desktop);
        result.is_ok() && name[..8] == [68, 101, 102, 97, 117, 108, 116, 0]
    }
}

// Windows can also deliver SendInput through WM_INPUT with a null device.
// Its extra-info field is 32 bits even on x64; use the same 32-bit marker in
// both channels. Never filter a physical device merely by a matching tag.
fn own_raw_input(no_device: bool, marker: u32) -> bool {
    no_device && marker as usize == event_marker()
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    word: WPARAM,
    parameter: LPARAM,
) -> LRESULT {
    if message == WM_INPUT {
        let mut input = RAWINPUT::default();
        let mut size = u32::try_from(size_of::<RAWINPUT>()).expect("fixed Win32 structure");
        let read = unsafe {
            GetRawInputData(
                HRAWINPUT(parameter.0 as *mut _),
                RID_INPUT,
                Some((&raw mut input).cast()),
                &raw mut size,
                u32::try_from(size_of::<RAWINPUTHEADER>()).expect("fixed Win32 structure"),
            )
        };
        with_state(|state| {
            if read == u32::MAX {
                state.fail();
                return;
            }
            if input.header.dwType == RIM_TYPEKEYBOARD.0 {
                let key = unsafe { input.data.keyboard };
                if own_raw_input(input.header.hDevice.0.is_null(), key.ExtraInformation) {
                    return;
                }
                state
                    .physical
                    .update(usize::from(key.VKey), key.Flags & 1 == 0);
            } else if input.header.dwType == RIM_TYPEMOUSE.0 {
                let mouse = unsafe { input.data.mouse };
                if own_raw_input(input.header.hDevice.0.is_null(), mouse.ulExtraInformation) {
                    return;
                }
                let flags = unsafe { mouse.Anonymous.Anonymous.usButtonFlags };
                for (index, key) in [1, 2, 4, 5, 6].into_iter().enumerate() {
                    if flags & (1 << (index * 2)) != 0 {
                        state.physical.update(key, true);
                    }
                    if flags & (2 << (index * 2)) != 0 {
                        state.physical.update(key, false);
                    }
                }
            }
            state.record();
        });
    }
    unsafe { DefWindowProcW(window, message, word, parameter) }
}

unsafe extern "system" fn keyboard_proc(code: i32, word: WPARAM, parameter: LPARAM) -> LRESULT {
    if code >= 0 {
        let event = unsafe { &*(parameter.0 as *const KBDLLHOOKSTRUCT) };
        if !event.flags.contains(LLKHF_INJECTED) || event.dwExtraInfo != event_marker() {
            with_state(|state| {
                let inputs = if event.flags.contains(LLKHF_INJECTED) {
                    &mut state.injected
                } else {
                    &mut state.physical
                };
                inputs.update(
                    event.vkCode as usize,
                    ![WM_KEYUP as usize, WM_SYSKEYUP as usize].contains(&word.0),
                );
                state.record();
            });
        }
    }
    unsafe { CallNextHookEx(None, code, word, parameter) }
}

unsafe extern "system" fn mouse_proc(code: i32, word: WPARAM, parameter: LPARAM) -> LRESULT {
    if code >= 0 {
        let event = unsafe { &*(parameter.0 as *const MSLLHOOKSTRUCT) };
        if event.flags & 1 == 0 || event.dwExtraInfo != event_marker() {
            with_state(|state| {
                let key = match u32::try_from(word.0).unwrap_or(0) {
                    WM_LBUTTONDOWN => Some((1, true)),
                    WM_LBUTTONUP => Some((1, false)),
                    WM_RBUTTONDOWN => Some((2, true)),
                    WM_RBUTTONUP => Some((2, false)),
                    WM_MBUTTONDOWN => Some((4, true)),
                    WM_MBUTTONUP => Some((4, false)),
                    WM_XBUTTONDOWN | WM_XBUTTONUP => Some((
                        if event.mouseData >> 16 == 1 { 5 } else { 6 },
                        word.0 == WM_XBUTTONDOWN as usize,
                    )),
                    _ => None,
                };
                if let Some((key, down)) = key {
                    let inputs = if event.flags & 1 != 0 {
                        &mut state.injected
                    } else {
                        &mut state.physical
                    };
                    inputs.update(key, down);
                }
                state.record();
            });
        }
    }
    unsafe { CallNextHookEx(None, code, word, parameter) }
}

fn unavailable() -> DriverError {
    DriverError::new(
        DriverErrorKind::PermissionRequired,
        "Windows user activity monitoring requires an available interactive input desktop",
    )
    .retryable("unlock_desktop_and_request_new_session")
}

#[cfg(test)]
mod tests {
    use super::{PressedInputs, event_marker, own_raw_input};

    #[test]
    fn raw_input_ignores_only_this_process_tagged_synthetic_packets() {
        let marker = u32::try_from(event_marker()).unwrap();
        assert!(own_raw_input(true, marker));
        assert!(!own_raw_input(false, marker));
        assert!(!own_raw_input(true, marker ^ 1));
        assert!(!own_raw_input(false, 0));
    }

    #[test]
    fn initial_modifiers_and_mouse_buttons_clear_only_on_matching_releases() {
        let mut inputs = PressedInputs::default();
        inputs.update(0xa2, true); // Initially held left control.
        inputs.update(1, true);
        inputs.update(0x11, false); // Raw Input reports generic control.
        assert!(inputs.held());
        inputs.update(2, false);
        assert!(inputs.held());
        inputs.update(1, false);
        assert!(!inputs.held());
        inputs.update(256, true); // Invalid native code cannot grow storage.
        assert!(!inputs.held());
    }
}
