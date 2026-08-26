//! `Windows.Graphics.Capture` actor with a bounded target-keyed frame-pool LRU.

use std::collections::HashMap;
use std::ffi::c_void;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use nexus_cua_protocol::ScreenRect;
use nexus_cua_runtime::{DriverError, DriverErrorKind, RgbaImage};
use tokio::sync::oneshot;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{Interface, factory};

const COMMAND_CAPACITY: usize = 16;
const MAX_PIPELINES: usize = 4;
const PIPELINE_IDLE: Duration = Duration::from_secs(15);
const FRAME_TIMEOUT: Duration = Duration::from_secs(3);
const FRAME_REFRESH_WAIT: Duration = Duration::from_millis(50);
const FRAME_POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Clone)]
pub(super) struct CaptureActor {
    sender: SyncSender<CaptureCommand>,
    mutation_generation: Arc<AtomicU64>,
}

impl CaptureActor {
    pub(super) fn spawn() -> Result<Self, DriverError> {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        let (ready, ready_receiver) = sync_channel(1);
        let mutation_generation = Arc::new(AtomicU64::new(0));
        let actor_generation = Arc::clone(&mutation_generation);
        thread::Builder::new()
            .name("nexus-cua-windows-capture".to_owned())
            .spawn(move || match CaptureState::new(actor_generation) {
                Ok(state) => {
                    let _ = ready.send(Ok(()));
                    state.run(&receiver);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|_| capture_failure("failed to create Windows capture actor"))?;
        ready_receiver
            .recv()
            .map_err(|_| capture_failure("Windows capture actor stopped during startup"))??;
        Ok(Self {
            sender,
            mutation_generation,
        })
    }

    pub(super) fn invalidate_after_mutation(&self) {
        self.mutation_generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) async fn capture(
        &self,
        target_key: &str,
        hwnd: isize,
        screen_bounds: ScreenRect,
    ) -> Result<(RgbaImage, ScreenRect), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .try_send(CaptureCommand {
                target_key: target_key.to_owned(),
                hwnd,
                screen_bounds,
                reply,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    DriverError::new(DriverErrorKind::Busy, "Windows capture actor is busy")
                        .retryable("retry_with_backoff")
                }
                TrySendError::Disconnected(_) => actor_stopped(()),
            })?;
        receiver.await.map_err(actor_stopped)?
    }
}

struct CaptureCommand {
    target_key: String,
    hwnd: isize,
    screen_bounds: ScreenRect,
    reply: oneshot::Sender<Result<(RgbaImage, ScreenRect), DriverError>>,
}

struct CaptureState {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    pipelines: HashMap<String, CapturePipeline>,
    mutation_generation: Arc<AtomicU64>,
}

struct CapturePipeline {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    width: i32,
    height: i32,
    latest_image: Option<RgbaImage>,
    latest_generation: u64,
    last_used: Instant,
}

impl CaptureState {
    fn new(mutation_generation: Arc<AtomicU64>) -> Result<Self, DriverError> {
        // SAFETY: The dedicated actor balances this WinRT MTA initialization in
        // Drop and keeps all D3D/WinRT interfaces on the same thread.
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED)
                .map_err(|_| capture_failure("failed to initialize Windows Runtime MTA"))?;
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None::<&windows::Win32::Graphics::Dxgi::IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                Some(&raw mut context),
            )
            .map_err(|_| capture_failure("failed to create D3D11 device"))?;
            let device = device.ok_or_else(|| capture_failure("D3D11 device is unavailable"))?;
            let context = context.ok_or_else(|| capture_failure("D3D11 context is unavailable"))?;
            let dxgi: IDXGIDevice = device
                .cast()
                .map_err(|_| capture_failure("D3D11 DXGI interface is unavailable"))?;
            let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)
                .map_err(|_| capture_failure("failed to create WinRT D3D device"))?;
            let winrt_device = inspectable
                .cast()
                .map_err(|_| capture_failure("WinRT D3D interface is unavailable"))?;
            Ok(Self {
                device,
                context,
                winrt_device,
                pipelines: HashMap::new(),
                mutation_generation,
            })
        }
    }

    fn run(mut self, receiver: &Receiver<CaptureCommand>) {
        loop {
            self.prune_idle();
            let command = if self.pipelines.is_empty() {
                match receiver.recv() {
                    Ok(command) => command,
                    Err(_) => break,
                }
            } else {
                let wait = self.next_expiry_wait();
                match receiver.recv_timeout(wait) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            };
            let result = self.capture(&command.target_key, command.hwnd, command.screen_bounds);
            let _ = command.reply.send(result);
        }
    }

    fn capture(
        &mut self,
        target_key: &str,
        raw_hwnd: isize,
        screen_bounds: ScreenRect,
    ) -> Result<(RgbaImage, ScreenRect), DriverError> {
        self.ensure_pipeline(target_key, raw_hwnd)?;
        let pipeline = self
            .pipelines
            .get_mut(target_key)
            .expect("ensured capture pipeline exists");
        let current_size = pipeline.item.Size().map_err(|_| target_unavailable())?;
        if current_size.Width <= 0 || current_size.Height <= 0 {
            return Err(target_unavailable());
        }
        if current_size.Width != pipeline.width || current_size.Height != pipeline.height {
            pipeline
                .pool
                .Recreate(
                    &self.winrt_device,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    2,
                    current_size,
                )
                .map_err(|_| capture_failure("failed to resize WGC frame pool"))?;
            pipeline.width = current_size.Width;
            pipeline.height = current_size.Height;
            pipeline.latest_image = None;
        }
        pipeline.last_used = Instant::now();
        let current_generation = self.mutation_generation.load(Ordering::Acquire);
        let cache_is_current =
            pipeline.latest_image.is_some() && pipeline.latest_generation == current_generation;
        let wait = if cache_is_current {
            FRAME_REFRESH_WAIT
        } else {
            FRAME_TIMEOUT
        };
        let image = if let Some(frame) = newest_frame(&pipeline.pool, wait) {
            let image = copy_frame(&self.device, &self.context, &frame)?;
            let _ = frame.Close();
            pipeline.latest_image = Some(duplicate_image(&image));
            pipeline.latest_generation = current_generation;
            image
        } else if cache_is_current && let Some(image) = &pipeline.latest_image {
            duplicate_image(image)
        } else {
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "Windows Graphics Capture frame timed out",
            )
            .retryable("retry_native_capture"));
        };
        Ok((image, screen_bounds))
    }

    fn ensure_pipeline(&mut self, target_key: &str, raw_hwnd: isize) -> Result<(), DriverError> {
        if self.pipelines.contains_key(target_key) {
            return Ok(());
        }
        if self.pipelines.len() >= MAX_PIPELINES {
            if let Some(oldest) = self
                .pipelines
                .iter()
                .min_by_key(|(_, pipeline)| pipeline.last_used)
                .map(|(target_key, _)| target_key.clone())
            {
                self.pipelines.remove(&oldest);
            }
        }
        let item = capture_item(raw_hwnd)?;
        let size = item.Size().map_err(|_| target_unavailable())?;
        if size.Width <= 0 || size.Height <= 0 {
            return Err(target_unavailable());
        }
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &self.winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(|_| capture_failure("failed to create WGC frame pool"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|_| capture_failure("failed to create WGC session"))?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session
            .StartCapture()
            .map_err(|_| capture_failure("failed to start WGC session"))?;
        self.pipelines.insert(
            target_key.to_owned(),
            CapturePipeline {
                item,
                pool,
                session,
                width: size.Width,
                height: size.Height,
                latest_image: None,
                latest_generation: 0,
                last_used: Instant::now(),
            },
        );
        Ok(())
    }

    fn prune_idle(&mut self) {
        let now = Instant::now();
        self.pipelines
            .retain(|_, pipeline| now.duration_since(pipeline.last_used) < PIPELINE_IDLE);
    }

    fn next_expiry_wait(&self) -> Duration {
        self.pipelines
            .values()
            .map(|pipeline| PIPELINE_IDLE.saturating_sub(pipeline.last_used.elapsed()))
            .min()
            .unwrap_or(PIPELINE_IDLE)
    }
}

impl Drop for CaptureState {
    fn drop(&mut self) {
        self.pipelines.clear();
        // SAFETY: Balances RoInitialize on this actor thread.
        unsafe { RoUninitialize() };
    }
}

impl Drop for CapturePipeline {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

fn capture_item(raw_hwnd: isize) -> Result<GraphicsCaptureItem, DriverError> {
    let interop: IGraphicsCaptureItemInterop = factory::<GraphicsCaptureItem, _>()
        .map_err(|_| capture_failure("WGC activation factory is unavailable"))?;
    // SAFETY: CreateForWindow consumes only the copied HWND and returns a
    // retained WinRT capture item for that exact top-level window.
    unsafe {
        interop
            .CreateForWindow(hwnd(raw_hwnd))
            .map_err(|_| target_unavailable())
    }
}

fn newest_frame(
    pool: &Direct3D11CaptureFramePool,
    wait: Duration,
) -> Option<Direct3D11CaptureFrame> {
    let deadline = Instant::now() + wait;
    loop {
        let mut newest = None;
        while let Ok(frame) = pool.TryGetNextFrame() {
            if let Some(previous) = newest.replace(frame) {
                let _ = previous.Close();
            }
        }
        if let Some(frame) = newest {
            return Some(frame);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(FRAME_POLL_INTERVAL);
    }
}

fn duplicate_image(image: &RgbaImage) -> RgbaImage {
    RgbaImage::new(image.width, image.height, image.pixels.clone())
}

fn copy_frame(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    frame: &Direct3D11CaptureFrame,
) -> Result<RgbaImage, DriverError> {
    let surface = frame
        .Surface()
        .map_err(|_| capture_failure("WGC frame surface is unavailable"))?;
    let access: IDirect3DDxgiInterfaceAccess = surface
        .cast()
        .map_err(|_| capture_failure("WGC DXGI surface interface is unavailable"))?;
    // SAFETY: The WinRT surface owns the returned D3D11 texture interface.
    let texture: ID3D11Texture2D = unsafe {
        access
            .GetInterface()
            .map_err(|_| capture_failure("WGC D3D11 texture is unavailable"))?
    };
    let mut description = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: GetDesc writes the initialized descriptor.
    unsafe { texture.GetDesc(&raw mut description) };
    if description.Width == 0 || description.Height == 0 {
        return Err(capture_failure("WGC frame has invalid dimensions"));
    }
    let staging_description = D3D11_TEXTURE2D_DESC {
        Width: description.Width,
        Height: description.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: description.Format,
        SampleDesc: description.SampleDesc,
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    // SAFETY: D3D reads the descriptor and writes a retained texture pointer.
    unsafe {
        device
            .CreateTexture2D(&raw const staging_description, None, Some(&raw mut staging))
            .map_err(|_| capture_failure("failed to create WGC staging texture"))?;
    }
    let staging = staging.ok_or_else(|| capture_failure("WGC staging texture is unavailable"))?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: Both resources share the D3D device. Map exposes read-only bytes
    // until the matching Unmap below.
    unsafe {
        context.CopyResource(&staging, &texture);
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))
            .map_err(|_| capture_failure("failed to map WGC staging texture"))?;
    }
    let image = copy_bgra_rows(&mapped, description.Width, description.Height);
    // SAFETY: Balances the successful Map before any mapped bytes escape.
    unsafe { context.Unmap(&staging, 0) };
    image
}

fn copy_bgra_rows(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u32,
    height: u32,
) -> Result<RgbaImage, DriverError> {
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| capture_failure("WGC frame width exceeds memory limits"))?;
    let pitch = usize::try_from(mapped.RowPitch).unwrap_or(0);
    if mapped.pData.is_null() || pitch < row_bytes {
        return Err(capture_failure("WGC mapped row pitch is invalid"));
    }
    let pixel_bytes = row_bytes
        .checked_mul(usize::try_from(height).unwrap_or(usize::MAX))
        .ok_or_else(|| capture_failure("WGC frame dimensions exceed memory limits"))?;
    let mut pixels = vec![0_u8; pixel_bytes];
    for row in 0..usize::try_from(height).unwrap_or(0) {
        // SAFETY: Map guarantees RowPitch bytes for each row; row and output
        // ranges are checked against validated dimensions.
        let source = unsafe {
            slice::from_raw_parts((mapped.pData as *const u8).add(row * pitch), row_bytes)
        };
        let output = &mut pixels[row * row_bytes..(row + 1) * row_bytes];
        for (bgra, rgba) in source.chunks_exact(4).zip(output.chunks_exact_mut(4)) {
            rgba.copy_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
        }
    }
    Ok(RgbaImage::new(width, height, pixels))
}

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn capture_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).retryable("retry_native_capture")
}

fn target_unavailable() -> DriverError {
    DriverError::new(
        DriverErrorKind::TargetUnavailable,
        "Windows capture target generation is unavailable",
    )
    .retryable("list_windows")
}

fn actor_stopped<T>(_error: T) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, "Windows capture actor stopped")
}
