//! Serialized foreground input actor backed by `CGEvent`.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::Duration;

use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField, KeyCode,
    ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use nexus_cua_protocol::{PointerButton, ScreenPoint, SensitiveText};
use nexus_cua_runtime::{DriverError, DriverErrorKind};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

const COMMAND_CAPACITY: usize = 32;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone)]
pub(super) struct InputActor {
    sender: SyncSender<InputCommand>,
}

pub(super) enum InputAction {
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
    pub(super) fn spawn() -> Result<Self, DriverError> {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        let (ready, ready_receiver) = sync_channel(1);
        thread::Builder::new()
            .name("nexus-cua-macos-input".to_owned())
            .spawn(move || match InputState::new() {
                Ok(state) => {
                    let _ = ready.send(Ok(()));
                    state.run(&receiver);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|_| input_failure("failed to create macOS input actor"))?;
        ready_receiver
            .recv()
            .map_err(|_| input_failure("macOS input actor stopped during startup"))??;
        Ok(Self { sender })
    }

    pub(super) async fn perform(&self, pid: i32, action: InputAction) -> Result<(), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(InputCommand { pid, action, reply })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "macOS input actor is busy")
                        .retryable("retry_with_backoff")
                        .mutation_not_dispatched()
                }
                TrySendError::Disconnected(_) => actor_stopped(()).mutation_not_dispatched(),
            })?;
        receiver.await.map_err(actor_stopped)?
    }
}

struct InputCommand {
    pid: i32,
    action: InputAction,
    reply: oneshot::Sender<Result<(), DriverError>>,
}

struct InputState {
    source: CGEventSource,
}

impl InputState {
    fn new() -> Result<Self, DriverError> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|()| input_failure("failed to create HID CGEvent source"))?;
        Ok(Self { source })
    }

    fn run(self, receiver: &Receiver<InputCommand>) {
        while let Ok(command) = receiver.recv() {
            let result = self.perform(command.pid, command.action);
            let _ = command.reply.send(result);
        }
    }

    fn perform(&self, pid: i32, action: InputAction) -> Result<(), DriverError> {
        match action {
            InputAction::Click {
                point,
                button,
                count,
                background,
            } => {
                if background {
                    self.click_background(pid, point, button, count)
                } else {
                    self.click(point, button, count)
                }
            }
            InputAction::Press {
                point,
                button,
                duration_ms,
            } => self.press(point, button, duration_ms),
            InputAction::Move { point, duration_ms } => self.move_pointer(point, duration_ms),
            InputAction::TypeText(text) => self.type_text(pid, text.expose()),
            InputAction::PressKeys(keys) => self.press_keys(pid, &keys),
            InputAction::Scroll {
                point,
                delta_x,
                delta_y,
            } => self.scroll(point, delta_x, delta_y),
            InputAction::Drag {
                from,
                to,
                duration_ms,
            } => self.drag(from, to, duration_ms),
        }
    }

    fn click(
        &self,
        point: ScreenPoint,
        button: PointerButton,
        count: u8,
    ) -> Result<(), DriverError> {
        let (down_type, up_type, native_button) = mouse_types(button);
        let point = cg_point(point);
        let movement = self.mouse_event(CGEventType::MouseMoved, point, native_button)?;
        let mut events = Vec::with_capacity(usize::from(count));
        for click in 1..=count {
            let down = self.mouse_event(down_type, point, native_button)?;
            down.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i64::from(click));
            let up = self.mouse_event(up_type, point, native_button)?;
            up.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i64::from(click));
            events.push((down, up));
        }
        movement.post(CGEventTapLocation::HID);
        thread::sleep(FRAME_INTERVAL);
        for (down, up) in events {
            down.post(CGEventTapLocation::HID);
            thread::sleep(Duration::from_millis(10));
            up.post(CGEventTapLocation::HID);
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn click_background(
        &self,
        pid: i32,
        point: ScreenPoint,
        button: PointerButton,
        count: u8,
    ) -> Result<(), DriverError> {
        let (down_type, up_type, native_button) = mouse_types(button);
        let point = cg_point(point);
        for click in 1..=count {
            let down = self.mouse_event(down_type, point, native_button)?;
            down.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i64::from(click));
            let up = self.mouse_event(up_type, point, native_button)?;
            up.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, i64::from(click));
            down.post_to_pid(pid);
            up.post_to_pid(pid);
        }
        Ok(())
    }

    fn press(
        &self,
        point: ScreenPoint,
        button: PointerButton,
        duration_ms: u32,
    ) -> Result<(), DriverError> {
        let (down_type, up_type, native_button) = mouse_types(button);
        let point = cg_point(point);
        // Construct both events before posting the down event so allocation
        // failure cannot leave a synthetic button held.
        let down = self.mouse_event(down_type, point, native_button)?;
        let up = self.mouse_event(up_type, point, native_button)?;
        down.post(CGEventTapLocation::HID);
        thread::sleep(Duration::from_millis(u64::from(duration_ms)));
        up.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn move_pointer(&self, target: ScreenPoint, duration_ms: u32) -> Result<(), DriverError> {
        let current = CGEvent::new(self.source.clone())
            .map_err(|()| input_failure("failed to read current pointer location"))?
            .location();
        self.interpolate_mouse(
            current,
            cg_point(target),
            duration_ms,
            CGEventType::MouseMoved,
            CGMouseButton::Left,
        )
    }

    fn drag(
        &self,
        from: ScreenPoint,
        to: ScreenPoint,
        duration_ms: u32,
    ) -> Result<(), DriverError> {
        let from = cg_point(from);
        let to = cg_point(to);
        self.mouse_event(CGEventType::MouseMoved, from, CGMouseButton::Left)?
            .post(CGEventTapLocation::HID);
        self.mouse_event(CGEventType::LeftMouseDown, from, CGMouseButton::Left)?
            .post(CGEventTapLocation::HID);
        let drag_result = self.interpolate_mouse(
            from,
            to,
            duration_ms,
            CGEventType::LeftMouseDragged,
            CGMouseButton::Left,
        );
        let up_result = self
            .mouse_event(CGEventType::LeftMouseUp, to, CGMouseButton::Left)
            .map(|event| event.post(CGEventTapLocation::HID));
        drag_result.and(up_result)
    }

    fn interpolate_mouse(
        &self,
        from: CGPoint,
        to: CGPoint,
        duration_ms: u32,
        event_type: CGEventType,
        button: CGMouseButton,
    ) -> Result<(), DriverError> {
        let steps = (duration_ms / 16).clamp(1, 625);
        for step in 1..=steps {
            let progress = f64::from(step) / f64::from(steps);
            let point = CGPoint::new(
                from.x + (to.x - from.x) * progress,
                from.y + (to.y - from.y) * progress,
            );
            self.mouse_event(event_type, point, button)?
                .post(CGEventTapLocation::HID);
            if step < steps {
                thread::sleep(FRAME_INTERVAL);
            }
        }
        Ok(())
    }

    fn type_text(&self, pid: i32, text: &str) -> Result<(), DriverError> {
        let utf16 = Zeroizing::new(text.encode_utf16().collect::<Vec<_>>());
        for chunk in crate::input_text::chunks(&utf16) {
            let down = CGEvent::new_keyboard_event(self.source.clone(), 0, true)
                .map_err(|()| input_failure("failed to create Unicode key-down event"))?;
            down.set_string_from_utf16_unchecked(chunk);
            let up = CGEvent::new_keyboard_event(self.source.clone(), 0, false)
                .map_err(|()| input_failure("failed to create Unicode key-up event"))?;
            up.set_string_from_utf16_unchecked(chunk);
            // The driver confirms this application is active before dispatch.
            // PID routing prevents a concurrent focus change from leaking
            // keyboard content into an unrelated foreground application.
            down.post_to_pid(pid);
            up.post_to_pid(pid);
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    fn press_keys(&self, pid: i32, keys: &[String]) -> Result<(), DriverError> {
        if keys.len() == 1 && keys[0].len() == 1 && keys[0].is_ascii() {
            return self.type_text(pid, &keys[0]);
        }
        let mut flags = CGEventFlags::empty();
        let mut modifiers = Vec::new();
        let mut primary = Vec::new();
        for key in keys {
            let modifier = match nexus_cua_protocol::canonical_key(key) {
                "mod" | "meta" | "command" => {
                    Some((KeyCode::COMMAND, CGEventFlags::CGEventFlagCommand))
                }
                "control" => Some((KeyCode::CONTROL, CGEventFlags::CGEventFlagControl)),
                "alt" | "option" => Some((KeyCode::OPTION, CGEventFlags::CGEventFlagAlternate)),
                "shift" => Some((KeyCode::SHIFT, CGEventFlags::CGEventFlagShift)),
                _ => None,
            };
            if let Some((keycode, flag)) = modifier {
                flags |= flag;
                if !modifiers.iter().any(|(candidate, _)| *candidate == keycode) {
                    modifiers.push((keycode, flag));
                }
            } else if key.len() == 1 && key.is_ascii() {
                let (code, shift) =
                    super::keyboard_layout::character_key(u16::from(key.as_bytes()[0]))
                        .ok_or_else(|| unsupported_key(key))?;
                if shift {
                    flags |= CGEventFlags::CGEventFlagShift;
                }
                primary.push(code);
            } else {
                primary.push(
                    key_code(nexus_cua_protocol::canonical_key(key))
                        .ok_or_else(|| unsupported_key(key))?,
                );
            }
        }
        if primary.is_empty() {
            return Err(unsupported_key("modifier-only chord"));
        }

        let mut events = Vec::with_capacity(modifiers.len() * 2 + primary.len() * 2);
        let mut active_flags = CGEventFlags::empty();
        for (keycode, flag) in &modifiers {
            active_flags |= *flag;
            events.push(self.keyboard_event(*keycode, true, active_flags)?);
        }
        for keycode in primary {
            events.push(self.keyboard_event(keycode, true, flags)?);
            events.push(self.keyboard_event(keycode, false, flags)?);
        }
        for (keycode, flag) in modifiers.into_iter().rev() {
            active_flags.remove(flag);
            events.push(self.keyboard_event(keycode, false, active_flags)?);
        }
        for event in events {
            event.post_to_pid(pid);
        }
        Ok(())
    }

    fn keyboard_event(
        &self,
        keycode: u16,
        key_down: bool,
        flags: CGEventFlags,
    ) -> Result<CGEvent, DriverError> {
        let event = CGEvent::new_keyboard_event(self.source.clone(), keycode, key_down)
            .map_err(|()| input_failure("failed to create keyboard event"))?;
        event.set_flags(flags);
        Ok(event)
    }

    fn scroll(&self, point: ScreenPoint, delta_x: f64, delta_y: f64) -> Result<(), DriverError> {
        let event = CGEvent::new_scroll_event(
            self.source.clone(),
            ScrollEventUnit::PIXEL,
            2,
            bounded_i32(-delta_y),
            bounded_i32(-delta_x),
            0,
        )
        .map_err(|()| input_failure("failed to create scroll event"))?;
        event.set_location(cg_point(point));
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn mouse_event(
        &self,
        event_type: CGEventType,
        point: CGPoint,
        button: CGMouseButton,
    ) -> Result<CGEvent, DriverError> {
        CGEvent::new_mouse_event(self.source.clone(), event_type, point, button)
            .map_err(|()| input_failure("failed to create pointer event"))
    }
}

fn cg_point(point: ScreenPoint) -> CGPoint {
    CGPoint::new(point.x, point.y)
}

fn mouse_types(button: PointerButton) -> (CGEventType, CGEventType, CGMouseButton) {
    match button {
        PointerButton::Left => (
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGMouseButton::Left,
        ),
        PointerButton::Middle => (
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGMouseButton::Center,
        ),
        PointerButton::Right => (
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGMouseButton::Right,
        ),
    }
}

fn key_code(key: &str) -> Option<u16> {
    Some(match key {
        "enter" | "return" => KeyCode::RETURN,
        "tab" => KeyCode::TAB,
        "space" => KeyCode::SPACE,
        "backspace" => KeyCode::DELETE,
        "delete" => KeyCode::FORWARD_DELETE,
        "escape" => KeyCode::ESCAPE,
        "left" => KeyCode::LEFT_ARROW,
        "right" => KeyCode::RIGHT_ARROW,
        "up" => KeyCode::UP_ARROW,
        "down" => KeyCode::DOWN_ARROW,
        "home" => KeyCode::HOME,
        "end" => KeyCode::END,
        "page_up" => KeyCode::PAGE_UP,
        "page_down" => KeyCode::PAGE_DOWN,
        "f1" => KeyCode::F1,
        "f2" => KeyCode::F2,
        "f3" => KeyCode::F3,
        "f4" => KeyCode::F4,
        "f5" => KeyCode::F5,
        "f6" => KeyCode::F6,
        "f7" => KeyCode::F7,
        "f8" => KeyCode::F8,
        "f9" => KeyCode::F9,
        "f10" => KeyCode::F10,
        "f11" => KeyCode::F11,
        "f12" => KeyCode::F12,
        _ => return None,
    })
}

// The clamp proves that the rounded native scroll delta fits in i32.
#[allow(clippy::cast_possible_truncation)]
fn bounded_i32(value: f64) -> i32 {
    value
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
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
    DriverError::new(DriverErrorKind::Platform, "macOS input actor stopped")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_and_backspace_have_opposite_directions() {
        assert_eq!(key_code("backspace"), Some(KeyCode::DELETE));
        assert_eq!(key_code("delete"), Some(KeyCode::FORWARD_DELETE));
        assert_ne!(key_code("delete"), key_code("backspace"));
    }
}
