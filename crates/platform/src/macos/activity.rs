//! Content-free, passive user-takeover monitoring on its own CF run loop.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPortRef;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField,
};
use nexus_cua_runtime::{DriverActionGuard, DriverError, DriverErrorKind};
use objc2_core_graphics::CGPreflightListenEventAccess;

const CAPACITY: usize = 256;
const TICK: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_millis(500);
// Identification is paired with the source PID; this is not an authority token.
const EVENT_MARKER: i64 = 0x4e58_4355_4149_4e50;

// kCGEventSourceStateHIDSystemState: hardware state only, excluding posted events.
const HID_SYSTEM_STATE: i32 = 1;

unsafe extern "C" {
    fn CGEventTapIsEnabled(tap: CFMachPortRef) -> bool;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
    fn CGEventSourceFlagsState(state: i32) -> u64;
}

pub(super) fn mark(event: &CGEvent) {
    event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, EVENT_MARKER);
}

fn is_own_event(pid: i64, marker: i64) -> bool {
    pid == i64::from(std::process::id()) && marker == EVENT_MARKER
}

#[derive(Clone)]
pub(super) struct ActivityActor {
    sender: SyncSender<Registration>,
}

struct Registration {
    guard: DriverActionGuard,
    reply: SyncSender<Result<(), DriverError>>,
}

impl ActivityActor {
    pub(super) fn spawn() -> Result<Self, DriverError> {
        let (sender, receiver) = mpsc::sync_channel(32);
        std::thread::Builder::new()
            .name("nexus-cua-macos-activity".to_owned())
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

#[derive(Default)]
struct Guards(Vec<DriverActionGuard>, PressedInputs);

// Fixed-size physical key/button state, never characters or event history.
#[derive(Default)]
struct PressedInputs {
    keys: [u64; 4],
    buttons: u32,
    modifiers: bool,
}

fn holds_modifier(flags: CGEventFlags) -> bool {
    flags.intersects(
        CGEventFlags::CGEventFlagShift
            | CGEventFlags::CGEventFlagControl
            | CGEventFlags::CGEventFlagAlternate
            | CGEventFlags::CGEventFlagCommand
            | CGEventFlags::CGEventFlagSecondaryFn,
    )
}

impl PressedInputs {
    fn update(&mut self, kind: CGEventType, code: i64, flags: CGEventFlags) {
        self.modifiers = holds_modifier(flags);
        match kind {
            CGEventType::KeyDown | CGEventType::KeyUp => {
                if let Ok(code) = usize::try_from(code)
                    && code < 256
                {
                    let mask = 1_u64 << (code % 64);
                    if matches!(kind, CGEventType::KeyDown) {
                        self.keys[code / 64] |= mask;
                    } else {
                        self.keys[code / 64] &= !mask;
                    }
                }
            }
            CGEventType::LeftMouseDown
            | CGEventType::RightMouseDown
            | CGEventType::OtherMouseDown => {
                if let Ok(code) = u32::try_from(code)
                    && code < 32
                {
                    self.buttons |= 1_u32 << code;
                }
            }
            CGEventType::LeftMouseUp | CGEventType::RightMouseUp | CGEventType::OtherMouseUp => {
                if let Ok(code) = u32::try_from(code)
                    && code < 32
                {
                    self.buttons &= !(1_u32 << code);
                }
            }
            _ => {}
        }
    }

    fn held(&self) -> bool {
        self.modifiers || self.buttons != 0 || self.keys.iter().any(|keys| *keys != 0)
    }

    // A tap can miss a release, for example while Secure Event Input hides
    // keyboard events. Hardware state may only clear tracked input, never add it.
    fn reconcile_releases(
        &mut self,
        key_down: impl Fn(u16) -> bool,
        button_down: impl Fn(u32) -> bool,
        flags: CGEventFlags,
    ) {
        if !holds_modifier(flags) {
            self.modifiers = false;
        }
        for (word, keys) in (0_u16..).zip(self.keys.iter_mut()) {
            for bit in 0..64 {
                if *keys & (1 << bit) != 0 && !key_down(word * 64 + bit) {
                    *keys &= !(1 << bit);
                }
            }
        }
        for button in 0..32 {
            if self.buttons & (1 << button) != 0 && !button_down(button) {
                self.buttons &= !(1 << button);
            }
        }
    }
}

fn reconcile_hardware_releases(guards: &mut Guards) {
    if !guards.1.held() {
        return;
    }
    // SAFETY: These CoreGraphics state reads take plain values and retain nothing.
    guards.1.reconcile_releases(
        |key| unsafe { CGEventSourceKeyState(HID_SYSTEM_STATE, key) },
        |button| unsafe { CGEventSourceButtonState(HID_SYSTEM_STATE, button) },
        CGEventFlags::from_bits_truncate(unsafe { CGEventSourceFlagsState(HID_SYSTEM_STATE) }),
    );
    if !guards.1.held() {
        for guard in &guards.0 {
            guard.record_input_released();
        }
    }
}

impl Drop for Guards {
    fn drop(&mut self) {
        for guard in &self.0 {
            guard.activity_monitor_failed();
        }
    }
}

fn run(receiver: &Receiver<Registration>) {
    // No native monitor, input capture, or polling while no protected session exists.
    while let Ok(first) = receiver.recv() {
        if let Err(error) = first.guard.check() {
            let _ = first.reply.send(Err(error));
            continue;
        }
        if !CGPreflightListenEventAccess() {
            let _ = first.reply.send(Err(DriverError::new(
                DriverErrorKind::PermissionRequired,
                "macOS Input Monitoring permission is required for user takeover protection",
            )
            .retryable("grant_input_monitoring_permission")));
            continue;
        }
        let guards = Arc::new(Mutex::new(Guards::default()));
        let healthy = Arc::new(AtomicBool::new(true));
        let tap = make_tap(Arc::clone(&guards), Arc::clone(&healthy));
        let Ok(tap) = tap else {
            let _ = first.reply.send(Err(unavailable()));
            continue;
        };
        let Ok(source) = tap.mach_port().create_runloop_source(0) else {
            let _ = first.reply.send(Err(unavailable()));
            continue;
        };
        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&source, unsafe { kCFRunLoopDefaultMode });
        tap.enable();
        admit(first, &guards);
        let connected = monitor(receiver, &guards, &healthy, &tap);
        run_loop.remove_source(&source, unsafe { kCFRunLoopDefaultMode });
        drop(tap);
        if !connected {
            break;
        }
    }
}

fn admit(registration: Registration, guards: &Mutex<Guards>) {
    let mut guards = guards.lock().expect("activity registry is actor-owned");
    guards.0.retain(|guard| guard.check().is_ok());
    let result = registration.guard.check().and_then(|()| {
        if guards.0.len() >= CAPACITY {
            return Err(DriverError::new(
                DriverErrorKind::Busy,
                "user activity session budget is exhausted",
            ));
        }
        if guards.1.held() {
            registration.guard.record_user_input_state(true);
        }
        guards.0.push(registration.guard);
        Ok(())
    });
    let _ = registration.reply.send(result);
}

fn monitor(
    receiver: &Receiver<Registration>,
    guards: &Mutex<Guards>,
    healthy: &AtomicBool,
    tap: &CGEventTap<'_>,
) -> bool {
    loop {
        // The tap remains owned by this run-loop actor. Lost permission or
        // silent native disablement must not leave foreground input unprotected.
        if !unsafe { CGEventTapIsEnabled(tap.mach_port().as_concrete_TypeRef()) } {
            healthy.store(false, Ordering::Release);
            let guards = guards.lock().expect("activity registry is actor-owned");
            for guard in &guards.0 {
                guard.activity_monitor_failed();
            }
        }
        for _ in 0..32 {
            match receiver.try_recv() {
                Ok(registration) => {
                    if healthy.load(Ordering::Acquire) {
                        admit(registration, guards);
                    } else {
                        let _ = registration.reply.send(Err(unavailable()));
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return false,
            }
        }
        {
            let mut guards = guards.lock().expect("activity registry is actor-owned");
            guards.0.retain(|guard| guard.check().is_ok());
            if guards.0.is_empty() || !healthy.load(Ordering::Acquire) {
                return true;
            }
            reconcile_hardware_releases(&mut guards);
        }
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, TICK, false);
    }
}

fn make_tap(
    guards: Arc<Mutex<Guards>>,
    healthy: Arc<AtomicBool>,
) -> Result<CGEventTap<'static>, ()> {
    CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        vec![
            CGEventType::MouseMoved,
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGEventType::LeftMouseDragged,
            CGEventType::RightMouseDragged,
            CGEventType::OtherMouseDragged,
            CGEventType::ScrollWheel,
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ],
        move |_, kind, event| {
            let lost = matches!(
                kind,
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
            );
            // Disabled notifications may not contain a usable event; never read their fields.
            if !lost
                && is_own_event(
                    event.get_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID),
                    event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA),
                )
            {
                return CallbackResult::Keep;
            }
            if lost {
                healthy.store(false, Ordering::Release);
            }
            let mut guards = guards.lock().expect("activity registry is actor-owned");
            if !lost {
                let code = match kind {
                    CGEventType::KeyDown | CGEventType::KeyUp => {
                        event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE)
                    }
                    CGEventType::RightMouseDown | CGEventType::RightMouseUp => 1,
                    CGEventType::OtherMouseDown | CGEventType::OtherMouseUp => {
                        event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER)
                    }
                    _ => 0, // Left button, or an event without a key/button code.
                };
                guards.1.update(kind, code, event.get_flags());
            }
            for guard in &guards.0 {
                if lost {
                    guard.activity_monitor_failed();
                } else {
                    guard.record_user_input_state(guards.1.held());
                }
            }
            CallbackResult::Keep
        },
    )
}

fn unavailable() -> DriverError {
    DriverError::new(
        DriverErrorKind::Platform,
        "user activity monitoring is unavailable; automatic input is disabled",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_keys_buttons_and_modifiers_do_not_become_idle_without_release() {
        let mut state = PressedInputs::default();
        let none = CGEventFlags::empty();
        state.update(CGEventType::KeyDown, 12, none);
        state.update(CGEventType::KeyDown, 12, none); // autorepeat is one held key
        state.update(CGEventType::LeftMouseDown, 0, none);
        state.update(CGEventType::KeyUp, 12, none);
        assert!(state.held());
        state.update(CGEventType::LeftMouseUp, 0, none);
        assert!(!state.held());
        state.update(
            CGEventType::FlagsChanged,
            0,
            CGEventFlags::CGEventFlagCommand,
        );
        assert!(state.held());
        state.update(
            CGEventType::FlagsChanged,
            0,
            CGEventFlags::CGEventFlagAlphaShift,
        );
        assert!(!state.held()); // Caps Lock is a toggle, not a held modifier.
    }

    #[test]
    fn hardware_state_clears_missed_releases_but_never_adds_input() {
        let mut state = PressedInputs::default();
        let command = CGEventFlags::CGEventFlagCommand;
        state.update(CGEventType::KeyDown, 12, command);
        state.update(CGEventType::KeyDown, 200, command);
        state.update(CGEventType::RightMouseDown, 1, command);
        state.reconcile_releases(|key| key == 200, |_| true, command);
        assert_eq!(state.keys, [0, 0, 0, 1 << (200 - 192)]);
        assert_eq!(state.buttons, 1 << 1);
        assert!(state.modifiers);
        state.reconcile_releases(|_| false, |_| false, CGEventFlags::empty());
        assert!(!state.held());
        state.reconcile_releases(|_| true, |_| true, command);
        assert!(!state.held());
    }

    #[test]
    fn only_marked_events_from_this_process_are_ignored() {
        let pid = i64::from(std::process::id());
        assert!(is_own_event(pid, EVENT_MARKER));
        assert!(!is_own_event(pid, 0));
        assert!(!is_own_event(0, EVENT_MARKER));
        assert!(!is_own_event(pid + 1, EVENT_MARKER));
    }
}
