//! Serialized `CoreGraphics` discovery and `ScreenCaptureKit` exact-window capture actor.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::mem::{MaybeUninit, size_of};
use std::ptr;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use block2::RcBlock;
use core_foundation::array::CFArray;
use core_foundation::base::{CFRetain, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryGetValueIfPresent};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::base::kCGImageAlphaPremultipliedLast;
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGPoint, CGRect as LegacyCGRect, CGSize};
use core_graphics::image::CGImage as LegacyCGImage;
use core_graphics::window::{
    self, kCGNullWindowID, kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionAll, kCGWindowName, kCGWindowNumber,
    kCGWindowOwnerName, kCGWindowOwnerPID,
};
use foreign_types::ForeignType;
use nexus_cua_protocol::ScreenRect;
use nexus_cua_runtime::{DriverError, DriverErrorKind, RgbaImage};
use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::NSRunningApplication;
use objc2_core_graphics::CGImage;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration,
};
use tokio::sync::oneshot;

const COMMAND_CAPACITY: usize = 32;
const RECYCLE_CAPACITY: usize = 2;
const APPLICATION_CACHE_CAPACITY: usize = 256;
const NATIVE_CALLBACK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub(super) struct NativeWindow {
    pub(super) key: String,
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
        let (recycle_sender, recycle_receiver) = sync_channel(RECYCLE_CAPACITY);
        thread::Builder::new()
            .name("nexus-cua-macos-capture".to_owned())
            .spawn(move || actor_loop(&receiver, &recycle_receiver, &recycle_sender))
            .expect("create CoreGraphics capture actor thread");
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
        expected_bounds: ScreenRect,
    ) -> Result<(RgbaImage, ScreenRect), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(CaptureCommand::Capture {
            target_key,
            expected_bounds,
            reply,
        })?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn resolve_window(
        &self,
        target_key: String,
    ) -> Result<NativeWindow, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(CaptureCommand::ResolveWindow { target_key, reply })?;
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
        expected_bounds: ScreenRect,
        reply: oneshot::Sender<Result<(RgbaImage, ScreenRect), DriverError>>,
    },
    ResolveWindow {
        target_key: String,
        reply: oneshot::Sender<Result<NativeWindow, DriverError>>,
    },
}

fn actor_loop(
    receiver: &Receiver<CaptureCommand>,
    recycle_receiver: &Receiver<Vec<u8>>,
    recycle_sender: &SyncSender<Vec<u8>>,
) {
    let mut recycled = VecDeque::with_capacity(RECYCLE_CAPACITY);
    let mut applications = ApplicationCache::default();
    let mut capture_filter = None;
    while let Ok(command) = receiver.recv() {
        while let Ok(pixels) = recycle_receiver.try_recv() {
            if recycled.len() < RECYCLE_CAPACITY {
                recycled.push_back(pixels);
            }
        }
        autoreleasepool(|_| match command {
            CaptureCommand::ListWindows { reply } => {
                let _ = reply.send(load_windows(&mut applications));
            }
            CaptureCommand::Capture {
                target_key,
                expected_bounds,
                reply,
            } => {
                let _ = reply.send(capture_target(
                    &target_key,
                    expected_bounds,
                    &mut capture_filter,
                    recycled.pop_front(),
                    recycle_sender.clone(),
                ));
            }
            CaptureCommand::ResolveWindow { target_key, reply } => {
                let _ = reply.send(resolve_target(&target_key, &mut applications));
            }
        });
    }
}

fn load_windows(applications: &mut ApplicationCache) -> Result<Vec<NativeWindow>, DriverError> {
    let dictionaries = window::copy_window_info(
        kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )
    .ok_or_else(|| native_failure("CoreGraphics window discovery failed"))?;
    let mut output = Vec::new();
    for pointer in &dictionaries {
        let pointer = *pointer as CFTypeRef;
        if pointer.is_null() {
            continue;
        }
        // SAFETY: The window-info array retains every dictionary for the
        // iteration; the wrapper takes its own retain before downcasting.
        let value = unsafe { CFType::wrap_under_get_rule(pointer) };
        let Some(dictionary) = value.downcast::<CFDictionary>() else {
            continue;
        };
        if let Some(window) = native_window(&dictionary, applications) {
            output.push(window);
        }
    }
    Ok(output)
}

fn resolve_target(
    target_key: &str,
    applications: &mut ApplicationCache,
) -> Result<NativeWindow, DriverError> {
    let window_id = target_window_id(target_key)?;
    let window_ids = CFArray::from_copyable(&[window_id]);
    let dictionaries =
        window::create_description_from_array(window_ids).ok_or_else(target_unavailable)?;
    for dictionary in &dictionaries {
        let dictionary = dictionary.to_untyped();
        if let Some(window) = native_window(&dictionary, applications)
            && window.key == target_key
        {
            return Ok(window);
        }
    }
    Err(target_unavailable())
}

fn native_window(
    dictionary: &CFDictionary,
    applications: &mut ApplicationCache,
) -> Option<NativeWindow> {
    if dictionary_i64(dictionary, unsafe { kCGWindowLayer }) != Some(0) {
        return None;
    }
    let pid = dictionary_i64(dictionary, unsafe { kCGWindowOwnerPID })
        .and_then(|value| i32::try_from(value).ok())?;
    let id = dictionary_i64(dictionary, unsafe { kCGWindowNumber })
        .and_then(|value| u32::try_from(value).ok())?;
    let frame = dictionary_rect(dictionary, unsafe { kCGWindowBounds })?;
    if frame.size.width <= 1.0 || frame.size.height <= 1.0 {
        return None;
    }
    let owner_name = dictionary_string(dictionary, unsafe { kCGWindowOwnerName })
        .unwrap_or_else(|| format!("process {pid}"));
    let application = applications.resolve(pid, owner_name)?;
    let foreground = application
        .running
        .as_ref()
        .is_some_and(|application| application.isActive());
    Some(NativeWindow {
        key: format!("sc:{}:{id}", application.generation),
        pid,
        application_key: application.generation.clone(),
        application_id: application.application_id.clone(),
        bundle_id: application.bundle_id.clone(),
        executable_path: application.executable_path.clone(),
        application_name: application.name.clone(),
        title: dictionary_string(dictionary, unsafe { kCGWindowName }).unwrap_or_default(),
        screen_bounds: ScreenRect {
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        },
        foreground,
        visible: dictionary_bool(dictionary, unsafe { kCGWindowIsOnscreen }).unwrap_or(false),
    })
}

fn capture_target(
    target_key: &str,
    expected_bounds: ScreenRect,
    cached_filter: &mut Option<CaptureFilter>,
    recycled: Option<Vec<u8>>,
    recycle_sender: SyncSender<Vec<u8>>,
) -> Result<(RgbaImage, ScreenRect), DriverError> {
    let window_id = target_window_id(target_key)?;
    if cached_filter
        .as_ref()
        .is_none_or(|cached| cached.target_key != target_key)
    {
        *cached_filter = Some(load_capture_filter(target_key, window_id)?);
    }
    let result = capture_with_filter(
        &cached_filter
            .as_ref()
            .expect("capture filter initialized above")
            .filter,
        expected_bounds,
        recycled,
        recycle_sender,
    );
    if result.is_err() {
        *cached_filter = None;
    }
    result.map(|image| (image, expected_bounds))
}

fn load_capture_filter(target_key: &str, window_id: u32) -> Result<CaptureFilter, DriverError> {
    let (sender, receiver) = sync_channel(1);
    let completion = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            if !error.is_null() || content.is_null() {
                let _ = sender.send(Err(native_failure(
                    "ScreenCaptureKit target discovery failed",
                )));
                return;
            }
            // SAFETY: ScreenCaptureKit retains `content` for the callback. The
            // retained filter owns its exact desktop-independent SCWindow.
            let result = unsafe {
                (&*content)
                    .windows()
                    .iter()
                    .find(|window| window.windowID() == window_id)
                    .map(|window| {
                        SCContentFilter::initWithDesktopIndependentWindow(
                            SCContentFilter::alloc(),
                            &window,
                        )
                    })
                    .ok_or_else(target_unavailable)
            };
            let _ = sender.send(result);
        },
    );
    // SAFETY: ScreenCaptureKit copies the Objective-C completion block and
    // invokes it on its capture queue.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true,
            false,
            &completion,
        );
    }
    let filter = receiver
        .recv_timeout(NATIVE_CALLBACK_TIMEOUT)
        .map_err(|_| native_timeout("ScreenCaptureKit target discovery timed out"))??;
    Ok(CaptureFilter {
        target_key: target_key.to_owned(),
        filter,
    })
}

fn capture_with_filter(
    filter: &SCContentFilter,
    expected_bounds: ScreenRect,
    recycled: Option<Vec<u8>>,
    recycle_sender: SyncSender<Vec<u8>>,
) -> Result<RgbaImage, DriverError> {
    // SAFETY: The filter and configuration remain retained until the
    // completion fires. Pixel copying happens entirely on the native capture
    // queue before the Rust-owned image crosses back to the actor.
    unsafe {
        let scale = f64::from(filter.pointPixelScale()).max(1.0);
        let width = capture_dimension(expected_bounds.width * scale)?;
        let height = capture_dimension(expected_bounds.height * scale)?;
        let configuration = SCStreamConfiguration::new();
        configuration.setWidth(width);
        configuration.setHeight(height);
        configuration.setShowsCursor(false);
        configuration.setIgnoreShadowsSingleWindow(true);
        let (sender, receiver) = sync_channel(1);
        let recycled = Arc::new(Mutex::new(recycled));
        let completion_recycled = Arc::clone(&recycled);
        let completion = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let result = if !error.is_null() || image.is_null() {
                Err(native_failure("ScreenCaptureKit image capture failed"))
            } else {
                let pixels = completion_recycled
                    .lock()
                    .ok()
                    .and_then(|mut pixels| pixels.take());
                // ScreenCaptureKit owns the callback image. Balance this
                // retain with `LegacyCGImage` drop after copying.
                let retained = CFRetain(image.cast()).cast_mut().cast();
                let image = LegacyCGImage::from_ptr(retained);
                copy_image(&image, pixels, recycle_sender.clone())
            };
            let _ = sender.send(result);
        });
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            &configuration,
            Some(&completion),
        );
        receiver
            .recv_timeout(NATIVE_CALLBACK_TIMEOUT)
            .map_err(|_| native_timeout("ScreenCaptureKit image capture timed out"))?
    }
}

fn target_window_id(target_key: &str) -> Result<u32, DriverError> {
    target_key
        .rsplit(':')
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(target_unavailable)
}

#[derive(Clone)]
struct ApplicationDescriptor {
    generation: String,
    application_id: String,
    bundle_id: Option<String>,
    executable_path: Option<String>,
    name: String,
    running: Option<Retained<NSRunningApplication>>,
}

struct CaptureFilter {
    target_key: String,
    filter: Retained<SCContentFilter>,
}

#[derive(Default)]
struct ApplicationCache {
    entries: HashMap<String, ApplicationDescriptor>,
    order: VecDeque<String>,
}

impl ApplicationCache {
    fn resolve(&mut self, pid: i32, fallback_name: String) -> Option<&ApplicationDescriptor> {
        let generation = application_generation(pid)?;
        if !self.entries.contains_key(&generation) {
            while self.entries.len() >= APPLICATION_CACHE_CAPACITY {
                if let Some(stale) = self.order.pop_front() {
                    self.entries.remove(&stale);
                } else {
                    break;
                }
            }
            self.order.push_back(generation.clone());
            self.entries.insert(
                generation.clone(),
                application_descriptor(pid, fallback_name, generation.clone()),
            );
        }
        self.entries.get(&generation)
    }
}

fn application_descriptor(
    pid: i32,
    fallback_name: String,
    generation: String,
) -> ApplicationDescriptor {
    let running = NSRunningApplication::runningApplicationWithProcessIdentifier(pid);
    let bundle_id = running
        .as_ref()
        .and_then(|application| application.bundleIdentifier())
        .map(|value| value.to_string())
        .filter(|value| !value.is_empty());
    let executable_path = running
        .as_ref()
        .and_then(|application| application.executableURL())
        .and_then(|url| url.path())
        .map(|path| path.to_string())
        .filter(|path| !path.is_empty());
    let name = running
        .as_ref()
        .and_then(|application| application.localizedName())
        .map(|value| value.to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_name);
    let application_id = stable_application_id(
        bundle_id.as_deref().unwrap_or_default(),
        executable_path.as_deref(),
        &generation,
    );
    ApplicationDescriptor {
        generation,
        application_id,
        bundle_id,
        executable_path,
        name,
        running,
    }
}

fn dictionary_value(dictionary: &CFDictionary, key: CFStringRef) -> Option<CFType> {
    let mut value: *const c_void = ptr::null();
    // SAFETY: Both dictionary and static CoreGraphics key are valid CF
    // objects. The dictionary retains the returned value for this lookup.
    let found = unsafe {
        CFDictionaryGetValueIfPresent(dictionary.as_concrete_TypeRef(), key.cast(), &raw mut value)
    };
    (found != 0 && !value.is_null()).then(|| unsafe { CFType::wrap_under_get_rule(value.cast()) })
}

fn dictionary_i64(dictionary: &CFDictionary, key: CFStringRef) -> Option<i64> {
    dictionary_value(dictionary, key)?
        .downcast::<CFNumber>()?
        .to_i64()
}

fn dictionary_bool(dictionary: &CFDictionary, key: CFStringRef) -> Option<bool> {
    dictionary_value(dictionary, key)?
        .downcast::<CFBoolean>()
        .map(bool::from)
}

fn dictionary_string(dictionary: &CFDictionary, key: CFStringRef) -> Option<String> {
    dictionary_value(dictionary, key)?
        .downcast::<CFString>()
        .map(|value| value.to_string())
}

fn dictionary_rect(dictionary: &CFDictionary, key: CFStringRef) -> Option<LegacyCGRect> {
    let value = dictionary_value(dictionary, key)?;
    let bounds = value.downcast::<CFDictionary>()?;
    LegacyCGRect::from_dict_representation(&bounds)
}

fn copy_image(
    image: &LegacyCGImage,
    recycled: Option<Vec<u8>>,
    recycle_sender: SyncSender<Vec<u8>>,
) -> Result<RgbaImage, DriverError> {
    let width = image.width();
    let height = image.height();
    let output_width = u32::try_from(width).map_err(|_| image_too_large())?;
    let output_height = u32::try_from(height).map_err(|_| image_too_large())?;
    let pixel_bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(image_too_large)?;
    let mut pixels = recycled.unwrap_or_default();
    pixels.resize(pixel_bytes, 0);
    let context = CGContext::create_bitmap_context(
        Some(pixels.as_mut_ptr().cast()),
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
        image,
    );
    drop(context);
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255) / alpha).min(255) as u8;
            }
        }
    }
    Ok(RgbaImage::with_recycler(
        output_width,
        output_height,
        pixels,
        move |pixels| {
            let _ = recycle_sender.try_send(pixels);
        },
    ))
}

fn application_generation(pid: i32) -> Option<String> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let info_size = i32::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points to a writable `proc_bsdinfo` buffer of the exact
    // size passed to libproc. A full-size return initializes the structure.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            info_size,
        )
    };
    if written != info_size {
        return None;
    }
    // SAFETY: The full-size libproc return above initialized every field.
    let info = unsafe { info.assume_init() };
    Some(format!(
        "pid:{pid}:start:{:016x}:{:08x}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    ))
}

fn capture_dimension(value: f64) -> Result<usize, DriverError> {
    if !value.is_finite() || value <= 0.0 || value > f64::from(u32::MAX) {
        return Err(image_too_large());
    }
    // The finite, positive, u32-bounded check above makes this native API
    // boundary conversion exact for every supported screenshot dimension.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value.ceil() as usize)
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
