//! Serialized macOS plain-text clipboard actor backed by `NSPasteboard`.

use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;

use nexus_cua_protocol::{ActionEffectStatus, ActionEvidenceKind, SensitiveText};
use nexus_cua_runtime::{
    DriverClipboardSnapshot, DriverClipboardWriteOutput, DriverError, DriverErrorKind,
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

const COMMAND_CAPACITY: usize = 8;

#[derive(Clone)]
pub(super) struct ClipboardActor {
    sender: SyncSender<ClipboardCommand>,
}

impl ClipboardActor {
    pub(super) fn spawn() -> Result<Self, DriverError> {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        let (ready, ready_receiver) = sync_channel(1);
        thread::Builder::new()
            .name("nexus-cua-macos-clipboard".to_owned())
            .spawn(move || {
                let state = ClipboardState::new();
                let _ = ready.send(());
                state.run(&receiver);
            })
            .map_err(|_| clipboard_failure("failed to create macOS clipboard actor"))?;
        ready_receiver
            .recv()
            .map_err(|_| clipboard_failure("macOS clipboard actor stopped during startup"))?;
        Ok(Self { sender })
    }

    pub(super) async fn read(
        &self,
        max_text_bytes: usize,
    ) -> Result<DriverClipboardSnapshot, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.try_send(
            ClipboardCommand::Read {
                max_text_bytes,
                reply,
            },
            false,
        )?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn write(
        &self,
        expected_change_marker: String,
        text: SensitiveText,
        max_text_bytes: usize,
    ) -> Result<DriverClipboardWriteOutput, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.try_send(
            ClipboardCommand::Write {
                expected_change_marker,
                text,
                max_text_bytes,
                reply,
            },
            true,
        )?;
        receiver.await.map_err(actor_stopped)?
    }

    fn try_send(&self, command: ClipboardCommand, mutation: bool) -> Result<(), DriverError> {
        self.sender.try_send(command).map_err(|error| {
            let driver_error = match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "macOS clipboard actor is busy")
                        .retryable("retry_with_backoff")
                }
                TrySendError::Disconnected(_) => actor_stopped(()),
            };
            if mutation {
                driver_error.mutation_not_dispatched()
            } else {
                driver_error
            }
        })
    }
}

enum ClipboardCommand {
    Read {
        max_text_bytes: usize,
        reply: oneshot::Sender<Result<DriverClipboardSnapshot, DriverError>>,
    },
    Write {
        expected_change_marker: String,
        text: SensitiveText,
        max_text_bytes: usize,
        reply: oneshot::Sender<Result<DriverClipboardWriteOutput, DriverError>>,
    },
}

struct ClipboardState {
    pasteboard: objc2::rc::Retained<NSPasteboard>,
}

impl ClipboardState {
    fn new() -> Self {
        Self {
            pasteboard: NSPasteboard::generalPasteboard(),
        }
    }

    fn run(self, receiver: &Receiver<ClipboardCommand>) {
        while let Ok(command) = receiver.recv() {
            match command {
                ClipboardCommand::Read {
                    max_text_bytes,
                    reply,
                } => {
                    let _ = reply.send(self.read(max_text_bytes));
                }
                ClipboardCommand::Write {
                    expected_change_marker,
                    text,
                    max_text_bytes,
                    reply,
                } => {
                    let _ = reply.send(self.write(&expected_change_marker, &text, max_text_bytes));
                }
            }
        }
    }

    fn read(&self, max_text_bytes: usize) -> Result<DriverClipboardSnapshot, DriverError> {
        let change_marker = marker(self.pasteboard.changeCount());
        let string_type = unsafe { NSPasteboardTypeString };
        let text = self
            .pasteboard
            .stringForType(string_type)
            .map(|value| Zeroizing::new(value.to_string()));
        if text
            .as_ref()
            .is_some_and(|value| value.len() > max_text_bytes)
        {
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "macOS clipboard text exceeds the configured byte bound",
            ));
        }
        Ok(DriverClipboardSnapshot {
            change_marker,
            text: text.map(|value| SensitiveText::new(value.as_str())),
        })
    }

    fn write(
        &self,
        expected_change_marker: &str,
        text: &SensitiveText,
        max_text_bytes: usize,
    ) -> Result<DriverClipboardWriteOutput, DriverError> {
        if text.expose().len() > max_text_bytes || text.expose().contains('\0') {
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "macOS clipboard text violates the configured bound",
            )
            .mutation_not_dispatched());
        }
        if marker(self.pasteboard.changeCount()) != expected_change_marker {
            return Err(DriverError::new(
                DriverErrorKind::StaleClipboard,
                "macOS clipboard changed after it was read",
            )
            .retryable("read_clipboard_again")
            .mutation_not_dispatched());
        }

        self.pasteboard.clearContents();
        let value = NSString::from_str(text.expose());
        let string_type = unsafe { NSPasteboardTypeString };
        if !self.pasteboard.setString_forType(&value, string_type) {
            return Err(
                clipboard_failure("macOS clipboard rejected text after replacement began")
                    .mutation_indeterminate(),
            );
        }
        let confirmed = self
            .pasteboard
            .stringForType(string_type)
            .is_some_and(|value| value.to_string() == text.expose());
        if !confirmed {
            return Err(clipboard_failure(
                "macOS clipboard read-back did not confirm the replacement",
            )
            .mutation_indeterminate());
        }
        Ok(DriverClipboardWriteOutput {
            change_marker: marker(self.pasteboard.changeCount()),
            effect_status: ActionEffectStatus::Confirmed,
            evidence_kind: ActionEvidenceKind::NativeReadback,
        })
    }
}

fn marker(change_count: isize) -> String {
    format!("macos:{change_count}")
}

fn clipboard_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message)
}

fn actor_stopped<T>(_: T) -> DriverError {
    clipboard_failure("macOS clipboard actor stopped unexpectedly")
}
