//! Bounded `AppKit` hit testing on the process main thread.

use std::future::Future;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use nexus_cua_protocol::ScreenPoint;
use nexus_cua_runtime::{DriverActionGuard, DriverError, DriverErrorKind};
use objc2::{MainThreadMarker, rc::autoreleasepool};
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication, NSWindow,
    NSWorkspace,
};
use objc2_foundation::NSPoint;
use tokio::sync::mpsc::{Sender, channel};

const CAPACITY: usize = 16;
const DEADLINE: Duration = Duration::from_millis(250);
static SENDER: Mutex<Option<Sender<MainRequest>>> = Mutex::new(None);

enum MainRequest {
    Activate {
        pid: i32,
        generation: String,
        guard: Option<DriverActionGuard>,
        deadline: Instant,
        reply: tokio::sync::oneshot::Sender<Result<(), DriverError>>,
    },
    HitTest(HitTest),
    Applications {
        deadline: Instant,
        reply: mpsc::SyncSender<Result<Vec<(i32, bool)>, DriverError>>,
    },
}

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
        .try_send(MainRequest::HitTest(HitTest {
            point,
            below,
            deadline: Instant::now() + DEADLINE,
            reply,
        }))
        .map_err(|_| unavailable())?;
    receiver.recv_timeout(DEADLINE).map_err(|_| unavailable())
}

pub(super) fn running_applications() -> Result<Vec<(i32, bool)>, DriverError> {
    let sender = SENDER
        .lock()
        .ok()
        .and_then(|sender| sender.clone())
        .ok_or_else(unavailable)?;
    let (reply, receiver) = mpsc::sync_channel(1);
    sender
        .try_send(MainRequest::Applications {
            deadline: Instant::now() + DEADLINE,
            reply,
        })
        .map_err(|_| unavailable())?;
    receiver.recv_timeout(DEADLINE).map_err(|_| unavailable())?
}

pub(super) async fn activate_application(
    pid: i32,
    generation: String,
    guard: Option<DriverActionGuard>,
) -> Result<(), DriverError> {
    let sender = SENDER
        .lock()
        .ok()
        .and_then(|sender| sender.clone())
        .ok_or_else(unavailable)?;
    let (reply, receiver) = tokio::sync::oneshot::channel();
    sender
        .try_send(MainRequest::Activate {
            pid,
            generation,
            guard,
            deadline: Instant::now() + DEADLINE,
            reply,
        })
        .map_err(|_| unavailable())?;
    tokio::time::timeout(DEADLINE, receiver)
        .await
        .map_err(|_| {
            DriverError::new(
                DriverErrorKind::TargetUnresponsive,
                "application activation completion is unknown",
            )
            .mutation_indeterminate()
        })?
        .map_err(|_| unavailable())?
}

fn activate(
    pid: i32,
    generation: &str,
    guard: Option<&DriverActionGuard>,
) -> Result<(), DriverError> {
    if super::capture::application_generation(pid).as_deref() != Some(generation) {
        return Err(DriverError::new(
            DriverErrorKind::TargetUnavailable,
            "application generation changed before activation",
        )
        .mutation_not_dispatched());
    }
    let application = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .ok_or_else(unavailable)?;
    if let Some(guard) = guard {
        guard.begin_dispatch()?;
    }
    application.unhide();
    if let Some(guard) = guard {
        guard.check()?;
    }
    if !application.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows) {
        return Err(DriverError::new(
            DriverErrorKind::Platform,
            "application activation was not confirmed",
        )
        .mutation_indeterminate());
    }
    Ok(())
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
                autoreleasepool(|_| match request {
                    MainRequest::Activate { pid, generation, guard, deadline, reply } => {
                        let result = if Instant::now() >= deadline { Err(unavailable()) } else { activate(pid, &generation, guard.as_ref()) };
                        let _ = reply.send(result);
                    }
                    MainRequest::HitTest(request) => {
                        if Instant::now() >= request.deadline { return; }
                        let height = core_graphics::display::CGDisplay::main().bounds().size.height;
                        let number = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(
                            NSPoint::new(request.point.x, height - request.point.y), request.below, main_thread,
                        );
                        let _ = request.reply.try_send(number);
                    }
                    MainRequest::Applications { deadline, reply } => {
                        if Instant::now() >= deadline { return; }
                        // Drain pending native notifications only when discovery is requested.
                        // No timer, idle polling, or native object crosses the main actor.
                        for _ in 0..8 {
                            if Instant::now() >= deadline { break; }
                            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, Duration::ZERO, true);
                        }
                        let running = NSWorkspace::sharedWorkspace().runningApplications();
                        let result = if running.len() > 4096 { Err(unavailable()) } else {
                            Ok(running.iter().filter(|app| !app.isTerminated() && app.activationPolicy() == NSApplicationActivationPolicy::Regular)
                                .map(|app| (app.processIdentifier(), app.isActive())).collect())
                        };
                        let _ = reply.try_send(result);
                    }
                });
            }
        }
    }
}

fn unavailable() -> DriverError {
    DriverError::new(
        DriverErrorKind::Busy,
        "native main-thread service is unavailable",
    )
    .retryable("retry_with_backoff")
    .mutation_not_dispatched()
}
