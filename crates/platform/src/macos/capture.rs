//! `ScreenCaptureKit` discovery and one-shot capture actor.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::Duration;

use block2::RcBlock;
use core_foundation::base::CFRetain;
use core_graphics::base::kCGImageAlphaPremultipliedLast;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGPoint, CGRect as LegacyCGRect, CGSize};
use core_graphics::image::CGImage as LegacyCGImage;
use foreign_types::ForeignType;
use nexus_cua_protocol::ScreenRect;
use nexus_cua_runtime::{DriverError, DriverErrorKind, RgbaImage};
use objc2::AnyThread;
use objc2_app_kit::NSRunningApplication;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration, SCWindow,
};
use tokio::sync::oneshot;

const COMMAND_CAPACITY: usize = 32;
const NATIVE_CALLBACK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub(super) struct NativeWindow {
    pub(super) key: String,
    pub(super) id: u32,
    pub(super) pid: i32,
    pub(super) application_key: String,
    pub(super) application_id: String,
    pub(super) bundle_id: Option<String>,
    pub(super) executable_path: Option<String>,
    pub(super) application_name: String,
    pub(super) title: String,
    pub(super) screen_bounds: ScreenRect,
    pub(super) foreground: bool,
    pub(super) visible: bool,
}

#[derive(Clone)]
pub(super) struct CaptureActor {
    sender: SyncSender<CaptureCommand>,
}

impl CaptureActor {
    pub(super) fn spawn() -> Self {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        thread::Builder::new()
            .name("nexus-cua-macos-capture".to_owned())
            .spawn(move || actor_loop(&receiver))
            .expect("create ScreenCaptureKit actor thread");
        Self { sender }
    }

    pub(super) async fn list_windows(&self) -> Result<Vec<NativeWindow>, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(CaptureCommand::ListWindows { reply })?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn capture(
        &self,
        target_key: String,
    ) -> Result<(RgbaImage, ScreenRect), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(CaptureCommand::Capture { target_key, reply })?;
        receiver.await.map_err(actor_stopped)?
    }

    fn admit(&self, command: CaptureCommand) -> Result<(), DriverError> {
        self.sender.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => {
                DriverError::new(DriverErrorKind::Busy, "macOS capture actor is busy")
                    .retryable("retry_with_backoff")
            }
            TrySendError::Disconnected(_) => actor_stopped(()),
        })
    }
}

enum CaptureCommand {
    ListWindows {
        reply: oneshot::Sender<Result<Vec<NativeWindow>, DriverError>>,
    },
    Capture {
        target_key: String,
        reply: oneshot::Sender<Result<(RgbaImage, ScreenRect), DriverError>>,
    },
}

fn actor_loop(receiver: &Receiver<CaptureCommand>) {
    while let Ok(command) = receiver.recv() {
        match command {
            CaptureCommand::ListWindows { reply } => {
                let _ = reply.send(load_windows());
            }
            CaptureCommand::Capture { target_key, reply } => {
                let _ = reply.send(capture_target(&target_key));
            }
        }
    }
}

fn load_windows() -> Result<Vec<NativeWindow>, DriverError> {
    let (sender, receiver) = sync_channel(1);
    let completion = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = if !error.is_null() || content.is_null() {
                Err(native_failure("ScreenCaptureKit window discovery failed"))
            } else {
                // SAFETY: ScreenCaptureKit owns `content` for the callback duration.
                // `extract_windows` copies only plain Rust values before returning.
                unsafe { Ok(extract_windows(&*content)) }
            };
            let _ = sender.send(result);
        },
    );
    // SAFETY: The copied Objective-C block is retained by ScreenCaptureKit for
    // the asynchronous operation and the closure owns its reply sender.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true,
            false,
            &completion,
        );
    }
    receiver
        .recv_timeout(NATIVE_CALLBACK_TIMEOUT)
        .map_err(|_| native_timeout("ScreenCaptureKit window discovery timed out"))?
}

fn capture_target(target_key: &str) -> Result<(RgbaImage, ScreenRect), DriverError> {
    let target_key = target_key.to_owned();
    let (sender, receiver) = sync_channel(1);
    let completion = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            if !error.is_null() || content.is_null() {
                let _ = sender.send(Err(native_failure(
                    "ScreenCaptureKit target refresh failed",
                )));
                return;
            }
            // SAFETY: Window/filter/configuration ownership stays inside the
            // nested ScreenCaptureKit callbacks. Only copied Rust bytes leave.
            unsafe {
                let window = match find_window(&*content, &target_key) {
                    Ok(window) => window,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                };
                start_capture(&window, sender.clone());
            }
        },
    );
    // SAFETY: See the matching discovery call in `load_windows`.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true,
            false,
            &completion,
        );
    }
    receiver
        .recv_timeout(NATIVE_CALLBACK_TIMEOUT)
        .map_err(|_| native_timeout("ScreenCaptureKit image capture timed out"))?
}

fn start_capture(
    window: &SCWindow,
    sender: SyncSender<Result<(RgbaImage, ScreenRect), DriverError>>,
) {
    // SAFETY: All Objective-C objects remain on this capture actor/callback
    // chain. The callback copies the CGImage before any native owner expires.
    unsafe {
        let frame = window.frame();
        let filter =
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window);
        let scale = f64::from(filter.pointPixelScale()).max(1.0);
        let configuration = SCStreamConfiguration::new();
        let width = match capture_dimension(frame.size.width * scale) {
            Ok(value) => value,
            Err(error) => {
                let _ = sender.send(Err(error));
                return;
            }
        };
        let height = match capture_dimension(frame.size.height * scale) {
            Ok(value) => value,
            Err(error) => {
                let _ = sender.send(Err(error));
                return;
            }
        };
        configuration.setWidth(width);
        configuration.setHeight(height);
        configuration.setShowsCursor(false);
        configuration.setIgnoreShadowsSingleWindow(true);
        let screen_bounds = ScreenRect {
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        };
        let completion = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let result = if !error.is_null() || image.is_null() {
                Err(native_failure("ScreenCaptureKit image capture failed"))
            } else {
                copy_image(image).map(|image| (image, screen_bounds))
            };
            let _ = sender.send(result);
        });
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            &filter,
            &configuration,
            Some(&completion),
        );
    }
}

fn extract_windows(content: &SCShareableContent) -> Vec<NativeWindow> {
    // SAFETY: `content` is retained by the surrounding ScreenCaptureKit
    // callback. We copy all values into Rust-owned descriptors synchronously.
    unsafe {
        let mut output = Vec::new();
        let mut applications = HashMap::new();
        let native_applications = content.applications();
        for application in &native_applications {
            let pid = application.processID();
            let application_key = application_generation(pid);
            let bundle_identifier = application.bundleIdentifier().to_string();
            let executable_path =
                NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                    .and_then(|application| application.executableURL())
                    .and_then(|url| url.path())
                    .map(|path| path.to_string())
                    .filter(|path| !path.is_empty());
            applications.insert(
                pid,
                (
                    stable_application_id(
                        &bundle_identifier,
                        executable_path.as_deref(),
                        &application_key,
                    ),
                    application.applicationName().to_string(),
                    application_key,
                    (!bundle_identifier.is_empty()).then_some(bundle_identifier),
                    executable_path,
                ),
            );
        }
        let native_windows = content.windows();
        for window in &native_windows {
            if window.windowLayer() != 0 {
                continue;
            }
            let Some(application) = window.owningApplication() else {
                continue;
            };
            let pid = application.processID();
            let Some((
                application_id,
                application_name,
                application_key,
                bundle_id,
                executable_path,
            )) = applications.get(&pid)
            else {
                continue;
            };
            let id = window.windowID();
            let frame = window.frame();
            if frame.size.width <= 1.0 || frame.size.height <= 1.0 {
                continue;
            }
            output.push(NativeWindow {
                key: format!("sc:{application_key}:{id}"),
                id,
                pid,
                application_key: application_key.clone(),
                application_id: application_id.clone(),
                bundle_id: bundle_id.clone(),
                executable_path: executable_path.clone(),
                application_name: application_name.clone(),
                title: window
                    .title()
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                screen_bounds: ScreenRect {
                    x: frame.origin.x,
                    y: frame.origin.y,
                    width: frame.size.width,
                    height: frame.size.height,
                },
                foreground: window.isActive(),
                visible: window.isOnScreen(),
            });
        }
        output
    }
}

fn find_window(
    content: &SCShareableContent,
    target_key: &str,
) -> Result<objc2::rc::Retained<SCWindow>, DriverError> {
    let descriptors = extract_windows(content);
    let target_id = descriptors
        .iter()
        .find(|descriptor| descriptor.key == target_key)
        .map(|descriptor| descriptor.id)
        .ok_or_else(target_unavailable)?;
    // SAFETY: The shareable content owns the window list for this synchronous
    // lookup, and NSArray iteration returns retained window references.
    unsafe {
        content
            .windows()
            .iter()
            .find(|window| window.windowID() == target_id)
            .ok_or_else(target_unavailable)
    }
}

fn copy_image(image: *mut CGImage) -> Result<RgbaImage, DriverError> {
    // SAFETY: The ScreenCaptureKit callback supplies a non-null CGImage. We
    // retain it before wrapping, and `LegacyCGImage` balances that retain.
    let image = unsafe {
        let retained = CFRetain(image.cast()).cast_mut().cast();
        LegacyCGImage::from_ptr(retained)
    };
    let width = image.width();
    let height = image.height();
    let output_width = u32::try_from(width).map_err(|_| image_too_large())?;
    let output_height = u32::try_from(height).map_err(|_| image_too_large())?;
    let mut context = CGContext::create_bitmap_context(
        None,
        width,
        height,
        8,
        width.saturating_mul(4),
        &CGColorSpace::create_device_rgb(),
        kCGImageAlphaPremultipliedLast,
    );
    context.translate(0.0, f64::from(output_height));
    context.scale(1.0, -1.0);
    context.draw_image(
        LegacyCGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(f64::from(output_width), f64::from(output_height)),
        ),
        &image,
    );
    let mut pixels = context.data().to_vec();
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255) / alpha).min(255) as u8;
            }
        }
    }
    Ok(RgbaImage {
        width: output_width,
        height: output_height,
        pixels,
    })
}

fn capture_dimension(value: f64) -> Result<usize, DriverError> {
    if !value.is_finite() || value <= 0.0 || value > f64::from(u32::MAX) {
        return Err(image_too_large());
    }
    // The finite, positive, u32-bounded check above makes this native API
    // boundary conversion exact for all supported screenshot dimensions.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value.ceil() as usize)
}

fn application_generation(pid: i32) -> String {
    let launch = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .and_then(|application| application.launchDate())
        .map(|date| date.timeIntervalSinceReferenceDate().to_bits())
        .unwrap_or_default();
    format!("pid:{pid}:launch:{launch:016x}")
}

fn stable_application_id(
    bundle_identifier: &str,
    executable_path: Option<&str>,
    generation: &str,
) -> String {
    if !bundle_identifier.is_empty() {
        return bundle_identifier.to_owned();
    }
    executable_path.map_or_else(|| format!("process:{generation}"), str::to_owned)
}

fn native_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).retryable("retry_native_capture")
}

fn native_timeout(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).retryable("retry_with_backoff")
}

fn target_unavailable() -> DriverError {
    DriverError::new(
        DriverErrorKind::TargetUnavailable,
        "target window generation is unavailable",
    )
    .retryable("list_windows")
}

fn actor_stopped<T>(_error: T) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, "macOS capture actor stopped")
}

fn image_too_large() -> DriverError {
    DriverError::new(
        DriverErrorKind::Platform,
        "captured image dimensions exceed protocol limits",
    )
}
