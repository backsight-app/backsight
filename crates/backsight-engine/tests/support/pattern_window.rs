//! A borderless window covering one monitor that shows a fixed picture through a flip-model
//! swap chain in R10G10B10A2: HDR10 (PQ, BT.2020) when Windows shows that monitor in HDR,
//! sRGB otherwise. For the calibrated check in `hdr_video.rs`; the pattern and the expected codes
//! are worked out there. Facts used: platform-notes.md, "HDR test pattern".

use std::time::{Duration, Instant};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO},
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            WindowsAndMessaging::*,
        },
    },
    core::{Interface, w},
};

pub struct PatternWindow {
    window: HWND,
    context: ID3D11DeviceContext,
    swap: Option<IDXGISwapChain3>,
    picture: ID3D11Texture2D,
    /// Windows shows the monitor in HDR (its DXGI colour space is `G2084_NONE_P2020`).
    pub hdr: bool,
}

extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: forwards the message Windows gave us, unchanged.
    unsafe { DefWindowProcW(window, message, w, l) }
}

impl PatternWindow {
    /// Covers `monitor` (a raw HMONITOR) with `pixels(hdr, size)`: row-major packed R10G10B10A2
    /// (red in the low 10 bits) at the monitor's size, read as PQ BT.2020 if `hdr`, else as sRGB.
    pub fn open(monitor: isize, pixels: impl FnOnce(bool, (u32, u32)) -> Vec<u32>) -> PatternWindow {
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: `info` is a valid local with `cbSize` set; a stale handle only fails the call.
        unsafe {
            // Physical pixels, as capture sees them.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            assert!(GetMonitorInfoW(HMONITOR(monitor as _), &mut info).as_bool(), "monitor gone");
        }
        let rect = info.rcMonitor;
        let (width, height) = ((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32);

        // The adapter driving the monitor, and whether Windows shows it in HDR.
        // SAFETY: DXGI enumeration on a fresh factory; each call fails at the end of its list.
        let (factory, adapter, hdr) = unsafe {
            let factory = CreateDXGIFactory1::<IDXGIFactory2>().unwrap();
            let mut found = None;
            'adapters: for i in 0.. {
                let Ok(adapter) = factory.EnumAdapters1(i) else { break };
                for j in 0.. {
                    let Ok(output) = adapter.EnumOutputs(j) else { break };
                    if let Ok(desc) = output.cast::<IDXGIOutput6>().and_then(|o| o.GetDesc1())
                        && desc.Monitor.0 as isize == monitor
                    {
                        println!(
                            "DXGI output: colour space {:?}, peak {} nits, full-frame {} nits",
                            desc.ColorSpace, desc.MaxLuminance, desc.MaxFullFrameLuminance
                        );
                        found = Some((adapter, desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020));
                        break 'adapters;
                    }
                }
            }
            let (adapter, hdr) = found.expect("a DXGI output for the monitor");
            (factory, adapter, hdr)
        };
        let (mut device, mut context) = (None, None);
        // SAFETY: an explicit adapter needs D3D_DRIVER_TYPE_UNKNOWN; out-pointers are locals.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
        }
        let (device, context) = (device.unwrap(), context.unwrap());

        // SAFETY: plain window calls; the class name is static and the procedure lives forever.
        // Registering the class again (a second monitor) fails harmlessly.
        let window = unsafe {
            let instance = GetModuleHandleW(None).unwrap();
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: w!("BacksightHdrPattern"),
                ..Default::default()
            };
            RegisterClassW(&class);
            let window = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                w!("BacksightHdrPattern"),
                w!("Backsight HDR test pattern"),
                WS_POPUP,
                rect.left,
                rect.top,
                width as i32,
                height as i32,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
            window
        };

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_R10G10B10A2_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        let space =
            if hdr { DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 } else { DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 };
        // SAFETY: `desc` is a valid local; the device and window are live.
        let swap: IDXGISwapChain3 = unsafe {
            let swap = factory.CreateSwapChainForHwnd(&device, window, &desc, None, None::<&IDXGIOutput>).unwrap();
            let swap: IDXGISwapChain3 = swap.cast().unwrap();
            // Informational only: Microsoft says unreported spaces may still work, so the
            // SetColorSpace1 result is what counts.
            println!("colour space {space:?}: support flags {:?}", swap.CheckColorSpaceSupport(space));
            swap.SetColorSpace1(space).expect("the swap chain takes the colour space");
            swap
        };

        let pixels = pixels(hdr, (width, height));
        assert_eq!(pixels.len(), (width * height) as usize);
        let texture = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R10G10B10A2_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let init =
            D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr().cast(), SysMemPitch: width * 4, SysMemSlicePitch: 0 };
        let mut picture = None;
        // SAFETY: `pixels` outlives the call and matches `texture`.
        unsafe { device.CreateTexture2D(&texture, Some(&init), Some(&mut picture)) }.unwrap();
        PatternWindow { window, context, swap: Some(swap), picture: picture.unwrap(), hdr }
    }

    /// Presents the picture every refresh and handles the window's messages for `time`.
    pub fn show_for(&self, time: Duration) {
        let swap = self.swap.as_ref().unwrap();
        let end = Instant::now() + time;
        let mut msg = MSG::default();
        while Instant::now() < end {
            // SAFETY: the back buffer and the picture are same-size, same-format textures on one
            // device; `msg` is a valid local and the messages are for this thread's window.
            unsafe {
                let back: ID3D11Texture2D = swap.GetBuffer(0).unwrap();
                self.context.CopyResource(&back, &self.picture);
                swap.Present(1, DXGI_PRESENT(0)).ok().unwrap();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

impl Drop for PatternWindow {
    fn drop(&mut self) {
        self.swap = None; // before its window goes
        // SAFETY: the window belongs to this thread and this struct.
        unsafe {
            let _ = DestroyWindow(self.window);
        }
    }
}
