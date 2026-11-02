//! Content-free WGC interop diagnostic; run in the interactive console session.
#[cfg(not(target_os = "windows"))]
fn main() {}

#[cfg(target_os = "windows")]
fn main() -> windows::core::Result<()> {
    use windows::Graphics::Capture::{GraphicsCaptureItem, GraphicsCaptureSession};
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
    use windows::Win32::UI::HiDpi::{
        AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    use windows::core::{BOOL, factory};

    unsafe extern "system" fn monitor(
        handle: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        _data: LPARAM,
    ) -> BOOL {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = u32::try_from(std::mem::size_of::<MONITORINFOEXW>()).unwrap();
        // SAFETY: The synchronous callback receives a current monitor and writes
        // only into the initialized fixed-size metadata structure.
        let valid =
            unsafe { GetMonitorInfoW(handle, std::ptr::from_mut(&mut info).cast::<MONITORINFO>()) };
        let end = info
            .szDevice
            .iter()
            .position(|v| *v == 0)
            .unwrap_or(info.szDevice.len());
        println!(
            "monitor={} valid={}",
            String::from_utf16_lossy(&info.szDevice[..end]),
            valid.as_bool()
        );
        let result = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .and_then(|interop| unsafe { interop.CreateForMonitor::<GraphicsCaptureItem>(handle) });
        match result {
            Ok(item) => println!("monitor_create=ok size={:?}", item.Size()),
            Err(error) => println!("monitor_create_hresult={:#010x}", error.code().0),
        }
        true.into()
    }

    // SAFETY: This standalone process configures DPI before any window creation;
    // all WinRT objects remain on this initialized thread. No pixels are captured.
    unsafe {
        let dpi = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        println!(
            "set_dpi={dpi:?} pmv2={}",
            AreDpiAwarenessContextsEqual(
                GetThreadDpiAwarenessContext(),
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            )
            .as_bool()
        );
        RoInitialize(RO_INIT_MULTITHREADED)?;
        let result = (|| -> windows::core::Result<()> {
            println!("wgc_supported={}", GraphicsCaptureSession::IsSupported()?);
            let interop: IGraphicsCaptureItemInterop = factory::<GraphicsCaptureItem, _>()?;
            match interop.CreateForWindow::<GraphicsCaptureItem>(GetForegroundWindow()) {
                Ok(item) => println!("window_create=ok size={:?}", item.Size()),
                Err(error) => println!("window_create_hresult={:#010x}", error.code().0),
            }
            let _ = EnumDisplayMonitors(None, None, Some(monitor), LPARAM(0));
            Ok(())
        })();
        RoUninitialize();
        result
    }
}
