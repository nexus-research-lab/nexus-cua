//! One bounded `ScreenCaptureKit` stream; all native samples stay on its capture queue.

use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFStringRef;
use dispatch2::{DispatchQueue, DispatchRetained};
use nexus_cua_runtime::{DriverError, DriverErrorKind, RgbaImage};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCStream, SCStreamConfiguration, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType,
};

const FRAME_WAIT: Duration = Duration::from_secs(1);
const CALLBACK_WAIT: Duration = Duration::from_secs(3);
const MAX_PIXELS: usize = 16_777_216;

#[derive(Default)]
struct LatestFrame {
    image: Option<RgbaImage>,
    callback_at: Option<Instant>,
    unavailable: bool,
}

#[derive(Default)]
struct FrameSlot {
    latest: Mutex<LatestFrame>,
    changed: Condvar,
}

define_class!(
    // NSObject has no additional subclassing requirements. Native sample buffers
    // are borrowed only on the serial handler queue; ivars contain synchronized Rust data.
    #[unsafe(super = NSObject)]
    #[ivars = Arc<FrameSlot>]
    struct FrameOutput;

    unsafe impl NSObjectProtocol for FrameOutput {}
    unsafe impl SCStreamOutput for FrameOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen { return; }
            let status = frame_status(sample);
            let image = if status == Some(0) { copy_sample(sample) } else { None };
            let Ok(mut latest) = self.ivars().latest.lock() else { return; };
            match status {
                Some(0) => {
                    latest.unavailable = image.is_none();
                    latest.image = image;
                    latest.callback_at = Some(Instant::now());
                }
                Some(1) => { latest.callback_at = Some(Instant::now()); }
                Some(2 | 3 | 5) => {
                    latest.image = None;
                    latest.unavailable = true;
                    latest.callback_at = Some(Instant::now());
                }
                _ => {}
            }
            self.ivars().changed.notify_all();
        }
    }
);

fn frame_status(sample: &CMSampleBuffer) -> Option<i64> {
    let attachments = unsafe { sample.sample_attachments_array(false) }?;
    let array = unsafe {
        CFArray::<*const std::ffi::c_void>::wrap_under_get_rule(
            std::ptr::from_ref(&*attachments).cast(),
        )
    };
    let pointer = *array.get(0)?;
    let value = unsafe { CFType::wrap_under_get_rule(pointer as CFTypeRef) };
    let dictionary = value.downcast::<CFDictionary>()?;
    super::capture::dictionary_i64(&dictionary, unsafe {
        std::ptr::from_ref(SCStreamFrameInfoStatus).cast() as CFStringRef
    })
}

fn copy_sample(sample: &CMSampleBuffer) -> Option<RgbaImage> {
    let buffer = unsafe { sample.image_buffer() }?;
    if CVPixelBufferGetPixelFormatType(&buffer) != u32::from_be_bytes(*b"BGRA") {
        return None;
    }
    let width = CVPixelBufferGetWidth(&buffer);
    let height = CVPixelBufferGetHeight(&buffer);
    let bytes = width.checked_mul(height)?.checked_mul(4)?;
    if width == 0 || height == 0 || bytes > MAX_PIXELS * 4 {
        return None;
    }
    let flags = CVPixelBufferLockFlags::ReadOnly;
    if unsafe { CVPixelBufferLockBaseAddress(&buffer, flags) } != 0 {
        return None;
    }
    let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
    let pitch = CVPixelBufferGetBytesPerRow(&buffer);
    let result = if base.is_null() || pitch < width * 4 {
        None
    } else {
        let mut pixels = vec![0_u8; bytes];
        for row in 0..height {
            // The locked CVPixelBuffer owns at least RowBytes per row. Copy only
            // validated BGRA bytes; no native object leaves the callback queue.
            let source = unsafe { std::slice::from_raw_parts(base.add(row * pitch), width * 4) };
            for (bgra, rgba) in source.as_chunks::<4>().0.iter().zip(
                pixels[row * width * 4..(row + 1) * width * 4]
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut(),
            ) {
                rgba.copy_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
                let alpha = u16::from(rgba[3]);
                if alpha != 0 && alpha != 255 {
                    for channel in &mut rgba[..3] {
                        *channel = ((u16::from(*channel) * 255) / alpha).min(255) as u8;
                    }
                }
            }
        }
        Some(RgbaImage::new(
            u32::try_from(width).ok()?,
            u32::try_from(height).ok()?,
            pixels,
        ))
    };
    let _ = unsafe { CVPixelBufferUnlockBaseAddress(&buffer, flags) };
    result
}

pub(super) struct CaptureStream {
    stream: Retained<SCStream>,
    _output: Retained<FrameOutput>,
    _queue: DispatchRetained<DispatchQueue>,
    slot: Arc<FrameSlot>,
}

impl CaptureStream {
    pub(super) fn start(
        filter: &SCContentFilter,
        width: usize,
        height: usize,
    ) -> Result<Self, DriverError> {
        if width
            .checked_mul(height)
            .is_none_or(|pixels| pixels > MAX_PIXELS)
        {
            return Err(unavailable("stream frame exceeds the pixel budget"));
        }
        let slot = Arc::new(FrameSlot::default());
        let allocated = FrameOutput::alloc().set_ivars(Arc::clone(&slot));
        let output: Retained<FrameOutput> = unsafe { msg_send![super(allocated), init] };
        let queue = DispatchQueue::new("nexus-cua-capture-output", None);
        let stream = unsafe {
            let configuration = SCStreamConfiguration::new();
            configuration.setWidth(width);
            configuration.setHeight(height);
            configuration.setPixelFormat(u32::from_be_bytes(*b"BGRA"));
            configuration.setShowsCursor(false);
            configuration.setIgnoreShadowsSingleWindow(true);
            configuration.setQueueDepth(3);
            configuration.setMinimumFrameInterval(CMTime::new(1, 10));
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                filter,
                &configuration,
                None,
            )
        };
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|_| unavailable("could not attach the capture output"))?;
        let (sender, receiver) = sync_channel(1);
        let completion = RcBlock::new(move |error: *mut objc2_foundation::NSError| {
            let _ = sender.send(error.is_null());
        });
        unsafe {
            stream.startCaptureWithCompletionHandler(Some(&completion));
        }
        let capture = Self {
            stream,
            _output: output,
            _queue: queue,
            slot,
        };
        if receiver.recv_timeout(CALLBACK_WAIT) != Ok(true) {
            return Err(unavailable("capture stream did not start"));
        }
        Ok(capture)
    }

    pub(super) fn snapshot(&self) -> Result<RgbaImage, DriverError> {
        let requested = Instant::now();
        let deadline = requested + FRAME_WAIT;
        let mut latest = self
            .slot
            .latest
            .lock()
            .map_err(|_| unavailable("capture frame state is unavailable"))?;
        loop {
            if latest.callback_at.is_some_and(|time| time >= requested) {
                if latest.unavailable {
                    return Err(unavailable("capture stream target is unavailable"));
                }
                if let Some(image) = &latest.image {
                    return Ok(RgbaImage::new(
                        image.width,
                        image.height,
                        image.pixels.clone(),
                    ));
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(unavailable("capture stream did not deliver a fresh frame"));
            }
            latest = self
                .slot
                .changed
                .wait_timeout(latest, remaining)
                .map_err(|_| unavailable("capture frame wait failed"))?
                .0;
        }
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        let (sender, receiver) = sync_channel(1);
        let completion = RcBlock::new(move |_error: *mut objc2_foundation::NSError| {
            let _ = sender.send(());
        });
        unsafe {
            self.stream
                .stopCaptureWithCompletionHandler(Some(&completion));
        }
        let _ = receiver.recv_timeout(Duration::from_secs(1));
        if let Ok(mut latest) = self.slot.latest.lock() {
            latest.image = None;
        }
    }
}

fn unavailable(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::TargetUnavailable, message).retryable("observe_window")
}
