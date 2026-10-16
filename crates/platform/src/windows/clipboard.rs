//! Serialized Windows plain-text clipboard actor backed by Win32 clipboard APIs.

use std::slice;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;

use nexus_cua_protocol::{ActionEffectStatus, ActionEvidenceKind, SensitiveText};
use nexus_cua_runtime::{
    DriverClipboardSnapshot, DriverClipboardWriteOutput, DriverError, DriverErrorKind,
};
use tokio::sync::oneshot;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WS_OVERLAPPED,
};
use windows::core::w;
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
            .name("nexus-cua-windows-clipboard".to_owned())
            .spawn(move || match ClipboardState::new() {
                Ok(state) => {
                    let _ = ready.send(Ok(()));
                    state.run(&receiver);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|_| clipboard_failure("failed to create Windows clipboard actor"))?;
        ready_receiver
            .recv()
            .map_err(|_| clipboard_failure("Windows clipboard actor stopped during startup"))??;
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
                    DriverError::new(DriverErrorKind::Busy, "Windows clipboard actor is busy")
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
    owner: HWND,
}

impl ClipboardState {
    fn new() -> Result<Self, DriverError> {
        // SAFETY: `STATIC` is a system window class, all optional handles are
        // null, and the message-only window remains owned by this actor thread.
        let owner = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("nexus-cua-clipboard"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        }
        .map_err(|_| clipboard_failure("failed to create Windows clipboard owner window"))?;
        Ok(Self { owner })
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
        let _clipboard = OpenClipboardGuard::new(self.owner)?;
        let text = read_open_clipboard(max_text_bytes)?;
        Ok(DriverClipboardSnapshot {
            change_marker: current_marker(),
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
            return Err(
                clipboard_failure("Windows clipboard text violates the configured bound")
                    .mutation_not_dispatched(),
            );
        }
        let _clipboard =
            OpenClipboardGuard::new(self.owner).map_err(DriverError::mutation_not_dispatched)?;
        if current_marker() != expected_change_marker {
            return Err(DriverError::new(
                DriverErrorKind::StaleClipboard,
                "Windows clipboard changed after it was read",
            )
            .retryable("read_clipboard_again")
            .mutation_not_dispatched());
        }

        let utf16 = Zeroizing::new(
            text.expose()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<_>>(),
        );
        let byte_length = utf16.len().checked_mul(size_of::<u16>()).ok_or_else(|| {
            clipboard_failure("Windows clipboard allocation size overflowed")
                .mutation_not_dispatched()
        })?;
        let mut memory = GlobalMemory::allocate(byte_length)?;
        memory.copy_utf16(&utf16)?;

        // SAFETY: The clipboard is open with this actor-owned window. After
        // `EmptyClipboard`, any failure is reported as indeterminate.
        unsafe { EmptyClipboard() }.map_err(|_| {
            clipboard_failure("Windows clipboard could not begin replacement")
                .mutation_indeterminate()
        })?;
        let handle = memory.handle();
        unsafe { SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(handle.0))) }.map_err(
            |_| {
                clipboard_failure("Windows clipboard rejected text after replacement began")
                    .mutation_indeterminate()
            },
        )?;
        memory.release();

        let confirmed = read_open_clipboard(max_text_bytes)
            .map_err(DriverError::mutation_indeterminate)?
            .is_some_and(|value| value.as_str() == text.expose());
        if !confirmed {
            return Err(clipboard_failure(
                "Windows clipboard read-back did not confirm the replacement",
            )
            .mutation_indeterminate());
        }
        Ok(DriverClipboardWriteOutput {
            change_marker: current_marker(),
            effect_status: ActionEffectStatus::Confirmed,
            evidence_kind: ActionEvidenceKind::NativeReadback,
        })
    }
}

impl Drop for ClipboardState {
    fn drop(&mut self) {
        // SAFETY: The window was created and is destroyed on this actor thread.
        let _ = unsafe { DestroyWindow(self.owner) };
    }
}

struct OpenClipboardGuard;

impl OpenClipboardGuard {
    fn new(owner: HWND) -> Result<Self, DriverError> {
        // SAFETY: `owner` is the actor-owned message-only window.
        unsafe { OpenClipboard(Some(owner)) }.map_err(|_| {
            DriverError::new(DriverErrorKind::Busy, "Windows clipboard is in use")
                .retryable("retry_with_backoff")
        })?;
        Ok(Self)
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: This guard exists only after `OpenClipboard` succeeds.
        let _ = unsafe { CloseClipboard() };
    }
}

struct GlobalMemory {
    handle: Option<HGLOBAL>,
}

impl GlobalMemory {
    fn allocate(byte_length: usize) -> Result<Self, DriverError> {
        // SAFETY: The allocation size is checked and remains actor-local until
        // ownership transfers to the clipboard.
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_length) }.map_err(|_| {
            clipboard_failure("Windows clipboard allocation failed").mutation_not_dispatched()
        })?;
        Ok(Self {
            handle: Some(handle),
        })
    }

    fn handle(&self) -> HGLOBAL {
        self.handle.expect("global-memory handle is present")
    }

    fn copy_utf16(&self, utf16: &[u16]) -> Result<(), DriverError> {
        let handle = self.handle();
        // SAFETY: `handle` is a live movable allocation sized for `utf16`.
        let pointer = unsafe { GlobalLock(handle) }.cast::<u16>();
        if pointer.is_null() {
            return Err(
                clipboard_failure("Windows clipboard allocation could not be locked")
                    .mutation_not_dispatched(),
            );
        }
        // SAFETY: Source and destination are non-overlapping and the allocation
        // was created with exactly this byte length.
        unsafe { pointer.copy_from_nonoverlapping(utf16.as_ptr(), utf16.len()) };
        // `GlobalUnlock` returns false both on error and when the lock count
        // reaches zero, so its wrapper result is intentionally ignored.
        let _ = unsafe { GlobalUnlock(handle) };
        Ok(())
    }

    fn release(&mut self) {
        self.handle = None;
    }
}

impl Drop for GlobalMemory {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: Ownership has not transferred to `SetClipboardData`.
            // Win32 returns null on successful free, which the generated
            // wrapper represents as an error; the native call still frees it.
            let _ = unsafe { GlobalFree(Some(handle)) };
        }
    }
}

fn read_open_clipboard(max_text_bytes: usize) -> Result<Option<Zeroizing<String>>, DriverError> {
    let format = u32::from(CF_UNICODETEXT.0);
    // SAFETY: The caller holds the clipboard open.
    if unsafe { IsClipboardFormatAvailable(format) }.is_err() {
        return Ok(None);
    }
    let handle = unsafe { GetClipboardData(format) }
        .map_err(|_| clipboard_failure("Windows clipboard text handle is unavailable"))?;
    let global = HGLOBAL(handle.0);
    // SAFETY: Clipboard data for `CF_UNICODETEXT` is a global-memory handle.
    let allocated_bytes = unsafe { GlobalSize(global) };
    if allocated_bytes < size_of::<u16>() {
        return Err(clipboard_failure(
            "Windows clipboard text allocation is malformed",
        ));
    }
    let pointer = unsafe { GlobalLock(global) }.cast::<u16>();
    if pointer.is_null() {
        return Err(clipboard_failure(
            "Windows clipboard text could not be locked",
        ));
    }
    let available_units = allocated_bytes / size_of::<u16>();
    let inspected_units = available_units.min(max_text_bytes.saturating_add(1));
    // SAFETY: The allocation contains at least `inspected_units` UTF-16 units.
    let units = unsafe { slice::from_raw_parts(pointer, inspected_units) };
    let terminator = units.iter().position(|unit| *unit == 0);
    let result = terminator.map_or_else(
        || {
            Err(clipboard_failure(
                "Windows clipboard text exceeds the configured byte bound or lacks a terminator",
            ))
        },
        |terminator| {
            let copied = Zeroizing::new(units[..terminator].to_vec());
            let text = String::from_utf16(&copied)
                .map_err(|_| clipboard_failure("Windows clipboard text is not valid UTF-16"))?;
            let text = Zeroizing::new(text);
            if text.as_bytes().len() > max_text_bytes {
                return Err(clipboard_failure(
                    "Windows clipboard text exceeds the configured byte bound",
                ));
            }
            Ok(Some(text))
        },
    );
    let _ = unsafe { GlobalUnlock(global) };
    result
}

fn current_marker() -> String {
    // SAFETY: This process-local read has no preconditions.
    format!("windows:{}", unsafe { GetClipboardSequenceNumber() })
}

fn clipboard_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message)
}

fn actor_stopped<T>(_: T) -> DriverError {
    clipboard_failure("Windows clipboard actor stopped unexpectedly")
}
