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
use nexus_cua_runtime::{DriverActionGuard, DriverError, DriverErrorKind};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

const COMMAND_CAPACITY: usize = 32;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone)]
pub(super) struct InputActor {
    sender: SyncSender<InputCommand>,
    guard: Option<DriverActionGuard>,
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
        Ok(Self {
            sender,
            guard: None,
        })
    }

    pub(super) fn guarded(&self, guard: DriverActionGuard) -> Self {
        Self {
            sender: self.sender.clone(),
            guard: Some(guard),
        }
    }

    pub(super) async fn perform(&self, pid: i32, action: InputAction) -> Result<(), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(InputCommand {
                pid,
                action,
                reply,
                guard: self.guard.clone(),
            })
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

fn pointer_targets(action: &InputAction) -> [Option<ScreenPoint>; 2] {
    match action {
        InputAction::Click {
            point,
            background: false,
            ..
        }
        | InputAction::Press { point, .. }
        | InputAction::Scroll { point, .. } => [Some(*point), None],
        InputAction::Drag { from, to, .. } => [Some(*from), Some(*to)],
        _ => [None, None],
    }
}

struct InputCommand {
    guard: Option<DriverActionGuard>,
    pid: i32,
    action: InputAction,
    reply: oneshot::Sender<Result<(), DriverError>>,
}

struct InputState {
    guard: Option<DriverActionGuard>,
    source: CGEventSource,
}

impl InputState {
    fn new() -> Result<Self, DriverError> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|()| input_failure("failed to create HID CGEvent source"))?;
        Ok(Self {
            source,
            guard: None,
        })
    }

    fn run(mut self, receiver: &Receiver<InputCommand>) {
        while let Ok(command) = receiver.recv() {
            self.guard = command.guard;
            let result = self.perform(command.pid, command.action);
            self.guard = None;
            let _ = command.reply.send(result);
        }
    }

    fn perform(&self, pid: i32, action: InputAction) -> Result<(), DriverError> {
        self.check_authority()?;
        for point in pointer_targets(&action).into_iter().flatten() {
            super::capture::validate_pointer_owner(pid, point)?;
        }
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
        self.post(&movement, None)?;
        thread::sleep(FRAME_INTERVAL);
        for (down, up) in events {
            self.post(&down, None)?;
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
            self.post(&down, Some(pid))?;
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
        self.post(&down, None)?;
        let result = self.wait_while_authorized(Duration::from_millis(u64::from(duration_ms)));
        if result.is_err()
            && let Ok(current) = CGEvent::new(self.source.clone())
        {
            up.set_location(current.location());
        }
        up.post(CGEventTapLocation::HID);
        result
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
        let movement = self.mouse_event(CGEventType::MouseMoved, from, CGMouseButton::Left)?;
        let down = self.mouse_event(CGEventType::LeftMouseDown, from, CGMouseButton::Left)?;
        let up = self.mouse_event(CGEventType::LeftMouseUp, from, CGMouseButton::Left)?;
        self.post(&movement, None)?;
        self.post(&down, None)?;
        let result = self.interpolate_mouse(
            from,
            to,
            duration_ms,
            CGEventType::LeftMouseDragged,
            CGMouseButton::Left,
        );
        let release_at = if result.is_ok() {
            to
        } else {
            CGEvent::new(self.source.clone()).map_or(from, |event| event.location())
        };
        up.set_location(release_at);
        up.post(CGEventTapLocation::HID);
        result
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
            self.post(&self.mouse_event(event_type, point, button)?, None)?;
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
            self.post(&down, Some(pid))?;
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
        self.check_authority()?;
        if let Some(guard) = &self.guard {
            guard.begin_dispatch()?;
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
        self.post(
            &self.mouse_event(
                CGEventType::MouseMoved,
                cg_point(point),
                CGMouseButton::Left,
            )?,
            None,
        )?;
        thread::sleep(FRAME_INTERVAL);
        self.post(&event, None)?;
        Ok(())
    }

    fn check_authority(&self) -> Result<(), DriverError> {
        self.guard.as_ref().map_or(Ok(()), DriverActionGuard::check)
    }

    fn post(&self, event: &CGEvent, pid: Option<i32>) -> Result<(), DriverError> {
        if let Some(guard) = &self.guard {
            guard.begin_dispatch()?;
        }
        match pid {
            Some(pid) => event.post_to_pid(pid),
            None => event.post(CGEventTapLocation::HID),
        }
        Ok(())
    }

    fn wait_while_authorized(&self, duration: Duration) -> Result<(), DriverError> {
        let deadline = std::time::Instant::now() + duration;
        while std::time::Instant::now() < deadline {
            self.check_authority()?;
            thread::sleep(
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(FRAME_INTERVAL),
            );
        }
        self.check_authority()
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
