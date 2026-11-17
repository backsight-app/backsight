//! Windows.Graphics.Capture monitor and window capture into BGRA textures on our device. No hooks, no
//! injection (ADR 0004). Gotchas are in `docs/reference/platform-notes.md`.

use super::d3d::Gpu;
use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};
use windows::{
    Foundation::{Metadata::ApiInformation, TimeSpan, TypedEventHandler},
    Graphics::{
        Capture::{Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{E_FAIL, HWND, POINT},
        Graphics::{
            Direct3D11::{ID3D11Device, ID3D11Texture2D},
            Dxgi::IDXGIDevice,
            Gdi::{HMONITOR, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, MonitorFromWindow},
        },
        System::{
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
            },
        },
    },
    core::{Error, HSTRING, Interface, Result, h},
};

/// WGC timestamps (`SystemRelativeTime`) are QPC time in 100 ns units: run the pacer at this rate.
pub const CLOCK_HZ: u64 = 10_000_000;

/// The pool hands out this many textures; each must go back fast (platform-notes.md).
const BUFFERS: i32 = 2;
const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

/// One captured frame, valid only during the callback: use it at once (`Converter::convert_from`),
/// or clone `handle` to keep `texture` out of the pool a little longer (platform-notes.md).
pub struct Frame<'a> {
    pub texture: &'a ID3D11Texture2D,
    pub size: (u32, u32),
    /// Capture time in [`CLOCK_HZ`] ticks, on the same clock as [`now`].
    pub time: u64,
    /// The WGC frame that owns `texture`; `None` for a texture that isn't from a capture pool.
    pub handle: Option<&'a Direct3D11CaptureFrame>,
}

/// The QPC clock in [`CLOCK_HZ`] ticks, for `Pacer::poll`.
pub fn now() -> u64 {
    let (mut ticks, mut freq) = (0, 0);
    // SAFETY: out-pointers are valid locals; neither call fails on Windows XP or later.
    unsafe {
        let _ = QueryPerformanceCounter(&mut ticks);
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    (ticks as u128 * CLOCK_HZ as u128 / freq as u128) as u64
}

pub fn primary_monitor() -> HMONITOR {
    // SAFETY: no pointers; MONITOR_DEFAULTTOPRIMARY always returns a monitor.
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

/// A connected monitor, for the monitor choice on Backsight's Video page.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Monitor {
    /// Stable across restarts and plugging in other monitors: the display device interface path.
    /// Saved in `video.capture_monitor`.
    pub id: String,
    /// "Monitor 2 (2560 × 1440)", with "main" for the primary one.
    pub label: String,
    pub primary: bool,
    #[serde(skip)]
    pub handle: isize,
}

/// Every connected monitor, left to right as Windows lists them.
pub fn monitors() -> Vec<Monitor> {
    use windows::Win32::{
        Foundation::{LPARAM, RECT},
        Graphics::Gdi::{
            DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, MONITORINFOEXW,
        },
        UI::WindowsAndMessaging::{EDD_GET_DEVICE_INTERFACE_NAME, MONITORINFOF_PRIMARY},
    };
    unsafe extern "system" fn add(monitor: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> windows::core::BOOL {
        // SAFETY: `list` is the Vec passed below, alive for the whole EnumDisplayMonitors call.
        unsafe { (*(list.0 as *mut Vec<HMONITOR>)).push(monitor) };
        true.into()
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(add), LPARAM(&raw mut handles as isize)) };
    let text = |s: &[u16]| String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]);
    let mut out = Vec::new();
    for (n, handle) in handles.into_iter().enumerate() {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        let mut device = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // SAFETY: both structs are valid locals with their sizes set; a stale handle only fails.
        let found = unsafe {
            GetMonitorInfoW(handle, &mut info.monitorInfo).as_bool()
                && EnumDisplayDevicesW(
                    windows::core::PCWSTR(info.szDevice.as_ptr()),
                    0,
                    &mut device,
                    EDD_GET_DEVICE_INTERFACE_NAME,
                )
                .as_bool()
        };
        let r = info.monitorInfo.rcMonitor;
        let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
        // Without an interface path (a remote or virtual display), the GDI name stands in.
        let id = if found { text(&device.DeviceID) } else { text(&info.szDevice) };
        let main = if primary { ", main" } else { "" };
        let label = format!("Monitor {} ({} × {}{main})", n + 1, r.right - r.left, r.bottom - r.top);
        out.push(Monitor { id, label, primary, handle: handle.0 as isize });
    }
    out
}

/// The error code capture reports when its window closes (or its monitor goes away), as opposed
/// to a failure that is worth trying again: `HRESULT_FROM_WIN32(ERROR_INVALID_WINDOW_HANDLE)`.
pub const ITEM_CLOSED: windows::core::HRESULT = windows::core::HRESULT(0x8007_0578_u32 as i32);

/// Whether the window `window` (a raw `HWND`) covers its whole monitor: full screen or borderless.
pub fn covers_monitor(window: isize) -> bool {
    use windows::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO},
        UI::WindowsAndMessaging::GetWindowRect,
    };
    let (mut rect, mut info) =
        (RECT::default(), MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() });
    // SAFETY: only reads window and monitor state into valid locals; a stale handle fails.
    let ok = unsafe {
        let monitor = MonitorFromWindow(HWND(window as _), MONITOR_DEFAULTTONULL);
        GetWindowRect(HWND(window as _), &mut rect).is_ok() && GetMonitorInfoW(monitor, &mut info).as_bool()
    };
    let r = |r: RECT| (r.left, r.top, r.right, r.bottom);
    ok && super::target::covers(r(rect), r(info.rcMonitor))
}

/// Whether the window `window` (a raw `HWND`) is minimized: WGC then sends no frames.
pub fn is_minimized(window: isize) -> bool {
    // SAFETY: only reads window state; a stale handle returns false.
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsIconic(HWND(window as _)).as_bool() }
}

/// The monitor showing most of the window `window` (a raw `HWND`), or the nearest one.
pub fn window_monitor(window: isize) -> HMONITOR {
    use windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST;
    // SAFETY: no pointers; MONITOR_DEFAULTTONEAREST returns a monitor even for a stale handle.
    unsafe { MonitorFromWindow(HWND(window as _), MONITOR_DEFAULTTONEAREST) }
}

/// The monitor showing the focused window (normally the game), or the primary one if none.
pub fn foreground_monitor() -> HMONITOR {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: no pointers; MONITOR_DEFAULTTOPRIMARY always returns a monitor, even for a null window.
    unsafe { MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY) }
}

/// The monitor under the mouse pointer, for the region picker.
pub fn cursor_monitor() -> HMONITOR {
    use windows::Win32::{Graphics::Gdi::MONITOR_DEFAULTTONEAREST, UI::WindowsAndMessaging::GetCursorPos};
    let mut point = POINT::default();
    // SAFETY: `point` is a valid local; on failure it stays (0, 0), which is on the primary monitor.
    unsafe {
        let _ = GetCursorPos(&mut point);
        MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST)
    }
}

/// The monitor's top-left on the virtual desktop in physical pixels (in a per-monitor-v2 process,
/// as the app is). Negative for a monitor left of or above the primary one.
pub fn monitor_origin(monitor: HMONITOR) -> Result<(i32, i32)> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `info` is a valid local with `cbSize` set; a stale handle only makes the call fail.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err(Error::new(E_FAIL, "The monitor was disconnected."));
    }
    Ok((info.rcMonitor.left, info.rcMonitor.top))
}

/// The focused top-level window to screenshot, or `None` when the whole monitor fits better: no
/// focus, the desktop or taskbar, a minimized window, or a Backsight window.
pub fn foreground_window() -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    };
    let mut class = [0u16; 64];
    let mut pid = 0;
    // SAFETY: plain window queries; `class` and `pid` are valid locals and a stale handle only fails them.
    let (window, len, usable) = unsafe {
        let window = GetForegroundWindow();
        let len = GetClassNameW(window, &mut class);
        GetWindowThreadProcessId(window, Some(&mut pid));
        (window, len, IsWindowVisible(window).as_bool() && !IsIconic(window).as_bool())
    };
    let class = String::from_utf16_lossy(&class[..len.max(0) as usize]);
    (usable && len > 0 && !is_shell_class(&class) && pid != std::process::id()).then_some(window)
}

/// Explorer's desktop (`Progman`, `WorkerW`) and taskbars: capturing them as a window is useless.
pub(crate) fn is_shell_class(class: &str) -> bool {
    matches!(class, "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
}

/// Leaves the window `hwnd` (a raw handle) out of every screen capture, ours included, so the
/// region picker's frozen picture never ends up in a replay or recording. Needs Windows 10 2004;
/// older versions return an error and the window is captured as before.
pub fn exclude_from_capture(hwnd: isize) -> windows::core::Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE};
    // SAFETY: only reads the handle; a stale or foreign one makes the call fail, nothing else.
    unsafe { SetWindowDisplayAffinity(HWND(hwnd as _), WDA_EXCLUDEFROMCAPTURE) }
}

/// Copies a captured frame to the CPU as tightly packed, top-down, opaque BGRA.
pub fn read_pixels(gpu: &Gpu, frame: Frame<'_>) -> windows::core::Result<(Vec<u8>, u32, u32)> {
    use windows::Win32::Graphics::Direct3D11::*;
    let (width, height) = frame.size;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        ..Default::default()
    };
    let mut texture = None;
    let _lock = gpu.lock();
    // SAFETY: descriptor and output live through the call; the texture uses this device.
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)) }?;
    let texture = texture.expect("texture created");
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let area = D3D11_BOX { left: 0, top: 0, front: 0, right: width, bottom: height, back: 1 };
    let row = width as usize * 4;
    let mut pixels = vec![0; row * height as usize];
    // SAFETY: same-device BGRA textures; area fits the content, and Map waits for the copy.
    unsafe {
        gpu.context.CopySubresourceRegion(&texture, 0, 0, 0, 0, frame.texture, 0, Some(&area));
        gpu.context.Map(&texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        for (y, out) in pixels.chunks_exact_mut(row).enumerate() {
            std::ptr::copy_nonoverlapping(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                out.as_mut_ptr(),
                row,
            );
        }
        gpu.context.Unmap(&texture, 0);
    }
    // The desktop image is opaque even when its surface's unused alpha bytes are zero.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Ok((pixels, width, height))
}

/// A running capture; dropping it stops capture. A callback already running may still finish
/// after the drop, which is why `on_frame` must own (or share through `Arc`) what it uses.
pub struct Capture {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    frame_token: i64,
    closed_token: i64,
}

/// What the callbacks share. FrameArrived may run on two pool threads at once, and `Closed` on a
/// third, so everything they touch sits behind one lock.
struct State<F> {
    size: SizeInt32,
    format: DirectXPixelFormat,
    on_frame: F,
    stopped: bool,
}

impl<F: FnMut(Result<Frame>)> State<F> {
    /// Reports the first error; capture delivers nothing after it.
    fn fail(&mut self, e: Error) {
        if !std::mem::replace(&mut self.stopped, true) {
            (self.on_frame)(Err(e));
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Capture {
    /// Starts capturing `monitor`. `on_frame` runs on a system thread pool thread for every new
    /// frame (WGC sends one only when the screen changes, so the pacer repeats frames). It gets
    /// at most one `Err`, when capture stops for good: the monitor was disconnected or the frame
    /// pool couldn't follow a resolution change. Calls never overlap. With `min_interval`, Windows
    /// 11 24H2 and later deliver no faster than that, which saves DWM's and our per-frame copies
    /// when a game presents faster than we record; older Windows ignores it.
    pub fn monitor(
        gpu: &Gpu,
        monitor: HMONITOR,
        cursor: bool,
        min_interval: Option<Duration>,
        on_frame: impl FnMut(Result<Frame>) + Send + 'static,
    ) -> Result<Self> {
        Self::start(gpu, monitor_item(monitor)?, FORMAT, cursor, min_interval, on_frame)
    }

    pub(crate) fn hdr_monitor(
        gpu: &Gpu,
        monitor: HMONITOR,
        cursor: bool,
        min_interval: Option<Duration>,
        on_frame: impl FnMut(Result<Frame>) + Send + 'static,
    ) -> Result<Self> {
        Self::start(gpu, monitor_item(monitor)?, DirectXPixelFormat::R16G16B16A16Float, cursor, min_interval, on_frame)
    }

    /// Like [`Capture::monitor`] for one top-level window: its visible frame, title bar included,
    /// in physical pixels, without the windows covering it. No frames come while it is minimized;
    /// the `Err` comes when it closes.
    pub fn window(
        gpu: &Gpu,
        window: HWND,
        cursor: bool,
        min_interval: Option<Duration>,
        on_frame: impl FnMut(Result<Frame>) + Send + 'static,
    ) -> Result<Self> {
        Self::start(gpu, window_item(window)?, FORMAT, cursor, min_interval, on_frame)
    }

    /// [`Capture::window`] in linear scRGB FP16, for a window on a monitor showing HDR (OBS does
    /// the same, platform-notes.md "HDR video"). The pool keeps FP16 if the window moves.
    pub(crate) fn hdr_window(
        gpu: &Gpu,
        window: HWND,
        cursor: bool,
        min_interval: Option<Duration>,
        on_frame: impl FnMut(Result<Frame>) + Send + 'static,
    ) -> Result<Self> {
        Self::start(gpu, window_item(window)?, DirectXPixelFormat::R16G16B16A16Float, cursor, min_interval, on_frame)
    }

    fn start(
        gpu: &Gpu,
        item: GraphicsCaptureItem,
        format: DirectXPixelFormat,
        cursor: bool,
        min_interval: Option<Duration>,
        on_frame: impl FnMut(Result<Frame>) + Send + 'static,
    ) -> Result<Self> {
        let size = item.Size()?;
        // Free-threaded: the default pool needs a DispatcherQueue and silently never fires without one.
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_device(&gpu.device)?, format, BUFFERS, size)?;
        let state = Arc::new(Mutex::new(State { size, format, on_frame, stopped: false }));
        let device = gpu.device.clone(); // the WinRT wrapper isn't Send, so re-wrap on resize
        let s = state.clone();
        let frame_token = pool.FrameArrived(&TypedEventHandler::new(
            move |pool: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
                let mut s = lock(&s);
                if !s.stopped
                    && let Err(e) = deliver(pool.ok()?, &device, &mut s)
                {
                    s.fail(e);
                }
                Ok(())
            },
        ))?;
        let s = state.clone();
        let closed_token = item.Closed(&TypedEventHandler::new(move |_, _| {
            lock(&s).fail(Error::new(ITEM_CLOSED, "The captured monitor was disconnected or the window closed."));
            Ok(())
        }))?;

        let session = pool.CreateCaptureSession(&item)?;
        let class = h!("Windows.Graphics.Capture.GraphicsCaptureSession");
        // Both properties exist only on newer Windows builds (cursor: 2004, border: 11).
        if has_property(class, "IsCursorCaptureEnabled") {
            session.SetIsCursorCaptureEnabled(cursor)?;
        }
        if has_property(class, "IsBorderRequired") {
            // Without permission Windows keeps its yellow border, which is only cosmetic.
            let _ = session.SetIsBorderRequired(false);
        }
        if let Some(interval) = min_interval
            && has_property(class, "MinUpdateInterval")
        {
            // Only saves work; capture is still correct without it.
            let _ = session.SetMinUpdateInterval(TimeSpan { Duration: (interval.as_nanos() / 100) as i64 });
        }
        session.StartCapture()?;
        Ok(Capture { item, pool, session, frame_token, closed_token })
    }
}

/// Hands the pool's next frame to `on_frame`, or follows a resolution change.
fn deliver<F: FnMut(Result<Frame>)>(
    pool: &Direct3D11CaptureFramePool,
    device: &ID3D11Device,
    s: &mut State<F>,
) -> Result<()> {
    // No frame left: an earlier event already took it.
    let Ok(frame) = pool.TryGetNextFrame() else { return Ok(()) };
    let content = frame.ContentSize()?;
    if content != s.size {
        // Resolution changed: the textures are the old size. Skip this frame; the next one
        // arrives at the new size and the caller rebuilds its converter.
        s.size = content;
        drop(frame);
        return pool.Recreate(&winrt_device(device)?, s.format, BUFFERS, content);
    }
    let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
    // SAFETY: the surface of a frame from our device's pool is an ID3D11Texture2D.
    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
    let time = frame.SystemRelativeTime()?.Duration as u64;
    let size = (content.Width as u32, content.Height as u32);
    (s.on_frame)(Ok(Frame { texture: &texture, size, time, handle: Some(&frame) }));
    Ok(()) // `frame` drops here and goes back to the pool unless the callback kept a clone
}

/// Check the active colour space on the adapter driving this monitor, which may differ
/// from the adapter encoding video on a hybrid PC. An error says why it couldn't tell.
pub fn monitor_hdr_enabled(monitor: HMONITOR) -> std::result::Result<bool, String> {
    use windows::Win32::Graphics::Dxgi::{Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, *};
    // SAFETY: creates a fresh factory to enumerate the current display topology.
    let factory = unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
    for i in 0.. {
        // SAFETY: the factory is live. The call fails at the end of the adapter list.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        for j in 0.. {
            // SAFETY: the adapter is live. The call fails at the end of the output list.
            let Ok(output) = (unsafe { adapter.EnumOutputs(j) }) else { break };
            let Ok(output) = output.cast::<IDXGIOutput6>() else { continue };
            // SAFETY: output is a live DXGI output interface.
            if let Ok(desc) = unsafe { output.GetDesc1() }
                && desc.Monitor == monitor
            {
                return Ok(
                    desc.AttachedToDesktop.as_bool() && desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
                );
            }
        }
    }
    Err(format!("no DXGI output found for monitor {monitor:?}"))
}

/// The graphics card DXGI lists `monitor` under, for the log: on a laptop with two cards, a
/// monitor wired to the other card than the one encoding makes Windows carry every captured
/// frame across (cost undocumented, platform-notes.md). `None` when DXGI doesn't say.
pub(crate) fn monitor_adapter(monitor: HMONITOR) -> Option<String> {
    use windows::Win32::Graphics::Dxgi::*;
    // SAFETY: creates a fresh factory to enumerate the current display topology.
    let factory = unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }.ok()?;
    for i in 0.. {
        // SAFETY: the factory is live. The call fails at the end of the adapter list.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else { break };
        for j in 0.. {
            // SAFETY: the adapter is live. The call fails at the end of the output list.
            let Ok(output) = (unsafe { adapter.EnumOutputs(j) }) else { break };
            // SAFETY: COM calls on the live output and adapter.
            let (Ok(out), Ok(card)) = (unsafe { (output.GetDesc(), adapter.GetDesc1()) }) else { continue };
            if out.Monitor == monitor {
                let len = card.Description.iter().position(|&c| c == 0).unwrap_or(card.Description.len());
                return Some(String::from_utf16_lossy(&card.Description[..len]));
            }
        }
    }
    None
}

/// The monitor's size in physical pixels, as capture delivers it.
pub fn monitor_size(monitor: HMONITOR) -> Result<(u32, u32)> {
    let SizeInt32 { Width, Height } = monitor_item(monitor)?.Size()?;
    Ok((Width as u32, Height as u32))
}

fn window_item(window: HWND) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    // SAFETY: `window` is an HWND; a stale one makes the interop return an error, not an item.
    unsafe { interop.CreateForWindow(window) }
}

fn monitor_item(monitor: HMONITOR) -> Result<GraphicsCaptureItem> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    // SAFETY: `monitor` is an HMONITOR; the interop returns a new item or an error.
    unsafe { interop.CreateForMonitor(monitor) }
}

fn winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice> {
    // SAFETY: wraps a live device for WinRT; the wrapper holds its own reference.
    unsafe { CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>()?) }?.cast()
}

fn has_property(class: &HSTRING, name: &str) -> bool {
    ApiInformation::IsPropertyPresent(class, &HSTRING::from(name)).unwrap_or(false)
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.pool.RemoveFrameArrived(self.frame_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "needs a desktop with a monitor"]
    fn monitors_have_ids_and_one_main() {
        let list = super::monitors();
        println!("{list:#?}");
        assert_eq!(list.iter().filter(|m| m.primary).count(), 1);
        assert!(list.iter().all(|m| !m.id.is_empty()));
    }

    #[test]
    fn desktop_and_taskbar_are_not_windows_to_capture() {
        for class in ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"] {
            assert!(super::is_shell_class(class), "{class}");
        }
        assert!(!super::is_shell_class("Notepad"));
        assert!(!super::is_shell_class("UnrealWindow"));
    }
}
