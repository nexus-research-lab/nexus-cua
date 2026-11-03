//! Bounded `AppKit` hit testing on the process main thread.

use std::future::Future;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use nexus_cua_protocol::ScreenPoint;
use nexus_cua_runtime::{DriverError, DriverErrorKind};
use objc2::{MainThreadMarker, rc::autoreleasepool};
use objc2_app_kit::NSWindow;
use objc2_foundation::NSPoint;
use tokio::sync::mpsc::{Sender, channel};

const CAPACITY: usize = 16;
const DEADLINE: Duration = Duration::from_millis(250);
static SENDER: Mutex<Option<Sender<HitTest>>> = Mutex::new(None);

struct HitTest {
    point: ScreenPoint,
    below: isize,
    deadline: Instant,
    reply: mpsc::SyncSender<isize>,
}

// The input actor retains serialization and dispatch ownership. Only copied
// coordinates and window numbers cross this queue; no AppKit object escapes.
pub(super) fn window_at(point: ScreenPoint, below: isize) -> Result<isize, DriverError> {
    let sender = SENDER
        .lock()
        .ok()
        .and_then(|sender| sender.clone())
        .ok_or_else(unavailable)?;
    let (reply, receiver) = mpsc::sync_channel(1);
    sender
        .try_send(HitTest {
            point,
            below,
            deadline: Instant::now() + DEADLINE,
            reply,
        })
        .map_err(|_| unavailable())?;
    receiver.recv_timeout(DEADLINE).map_err(|_| unavailable())
}

pub(crate) async fn run<F: Future>(future: F) -> F::Output {
    let Some(main_thread) = MainThreadMarker::new() else {
        return future.await;
    };
    let receiver = {
        let mut slot = SENDER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(|sender| !sender.is_closed()) {
            None
        } else {
            let (sender, receiver) = channel(CAPACITY);
            *slot = Some(sender);
            Some(receiver)
        }
    };
    let Some(mut receiver) = receiver else {
        return future.await;
    };
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            request = receiver.recv() => {
                let Some(request) = request else { return future.await; };
                if Instant::now() >= request.deadline { continue; }
                let number = autoreleasepool(|_| {
                    let height = core_graphics::display::CGDisplay::main().bounds().size.height;
                    NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(
                        NSPoint::new(request.point.x, height - request.point.y), request.below, main_thread,
                    )
                });
                let _ = request.reply.try_send(number);
            }
        }
    }
}

fn unavailable() -> DriverError {
    DriverError::new(
        DriverErrorKind::Busy,
        "native main-thread hit testing is unavailable",
    )
    .retryable("retry_with_backoff")
    .mutation_not_dispatched()
}
