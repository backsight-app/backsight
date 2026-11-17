//! A stand-in for a game: a borderless window over one monitor (the main one unless told), drawn on the NVIDIA card
//! with a pixel shader whose cost is a loop count, as fast as the card can (or one picture per
//! refresh). Every second it prints `SCENE <unix ms> <frames>`, so a run with Instant Replay on
//! and one with it off show what a game would lose. The picture is a function of time only, so
//! runs are comparable.
// shortcut: NVIDIA card only, like the rest of the cost harness.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::{
    Win32::{
        Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::{
            Direct3D::{Fxc::*, *},
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::{GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint},
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            WindowsAndMessaging::*,
        },
    },
    core::{PCSTR, s, w},
};

const SHADER: &str = r"
cbuffer C : register(b0) { float t; float load; float2 res; };
float4 vs(uint id : SV_VertexID) : SV_Position {
    return float4((id == 1) ? 3.0 : -1.0, (id == 2) ? -3.0 : 1.0, 0, 1);
}
float4 ps(float4 pos : SV_Position) : SV_Target {
    float2 uv = pos.xy / res;
    float2 p = uv * 6.0;
    float3 c = 0;
    int n = (int)load;
    [loop] for (int i = 0; i < n; i++) {
        p += 0.35 * float2(sin(p.y * 1.7 + t + i), cos(p.x * 1.3 - t * 0.7 + i));
        c += 0.5 + 0.5 * cos(float3(p.x, p.y, p.x + p.y) + float3(0, 2, 4));
    }
    c /= max(n, 1);
    // Fine detail sliding sideways across the lower part, like a panning game texture.
    float stripes = step(0.5, frac((uv.x + t * 0.1) * 80.0));
    return float4(lerp(c, c * (0.6 + 0.4 * stripes), step(0.7, uv.y)), 1);
}";

extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: forwards the message Windows gave us, unchanged.
    unsafe { DefWindowProcW(window, message, w, l) }
}

fn compile(entry: PCSTR, target: PCSTR) -> ID3DBlob {
    let (mut code, mut errors) = (None, None);
    // SAFETY: the pointer and length describe SHADER; names are NUL-terminated literals.
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("scene.hlsl"),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        // SAFETY: an error blob holds `GetBufferSize` bytes of text.
        let text = errors.map(|b: ID3DBlob| unsafe {
            String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()))
                .into_owned()
        });
        panic!("scene shader: {e} {text:?}");
    }
    code.unwrap()
}

/// Draws for `time`. `load` is the shader's loop count (more is heavier); `vsync` draws one
/// picture per screen refresh instead of as many as the card can.
pub fn run(monitor: Option<isize>, time: Duration, load: f32, vsync: bool) {
    // SAFETY: plain Win32, DXGI and D3D11 calls on objects made here and used on this thread
    // only; every pointer passed is a live local.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let monitor =
            monitor.map_or_else(|| MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY), |m| HMONITOR(m as _));
        assert!(GetMonitorInfoW(monitor, &mut info).as_bool(), "monitor gone");
        let rect = info.rcMonitor;
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let factory = CreateDXGIFactory1::<IDXGIFactory2>().unwrap();
        let adapter = (0..)
            .map_while(|i| factory.EnumAdapters1(i).ok())
            .find(|a| a.GetDesc1().is_ok_and(|d| d.VendorId == 0x10DE))
            .expect("an NVIDIA graphics card");
        let (mut device, mut context) = (None, None);
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
        let (device, context): (ID3D11Device, ID3D11DeviceContext) = (device.unwrap(), context.unwrap());

        let instance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: w!("BacksightCostScene"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("BacksightCostScene"),
            w!("Backsight cost scene"),
            WS_POPUP,
            rect.left,
            rect.top,
            width,
            height,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .unwrap();
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width as u32,
            Height: height as u32,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        let swap = factory.CreateSwapChainForHwnd(&device, window, &desc, None, None::<&IDXGIOutput>).unwrap();

        let (vs_code, ps_code) = (compile(s!("vs"), s!("vs_5_0")), compile(s!("ps"), s!("ps_5_0")));
        let bytes = |b: &ID3DBlob| std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize());
        let (mut vs, mut ps, mut constants) = (None, None, None);
        device.CreateVertexShader(bytes(&vs_code), None, Some(&mut vs)).unwrap();
        device.CreatePixelShader(bytes(&ps_code), None, Some(&mut ps)).unwrap();
        let buffer = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        device.CreateBuffer(&buffer, None, Some(&mut constants)).unwrap();
        let constants = constants.unwrap();
        context.VSSetShader(vs.as_ref(), None);
        context.PSSetShader(ps.as_ref(), None);
        context.PSSetConstantBuffers(0, Some(&[Some(constants.clone())]));
        context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        let viewport =
            D3D11_VIEWPORT { Width: width as f32, Height: height as f32, MaxDepth: 1.0, ..Default::default() };
        context.RSSetViewports(Some(&[viewport]));

        // Flat out, each picture is drawn off screen and waited for, and one is shown about 60
        // times a second: a window that isn't in front can't present faster than the screen
        // refreshes, which would hide what the card can really do.
        let texture = D3D11_TEXTURE2D_DESC {
            Width: width as u32,
            Height: height as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let (mut picture, mut target, mut done) = (None, None, None);
        device.CreateTexture2D(&texture, None, Some(&mut picture)).unwrap();
        let picture = picture.unwrap();
        device.CreateRenderTargetView(&picture, None, Some(&mut target)).unwrap();
        device.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut done)).unwrap();
        let done = done.unwrap();
        context.OMSetRenderTargets(Some(&[target]), None);

        let began = Instant::now();
        let (mut second, mut shown, mut frames) = (began, began, 0u32);
        let mut msg = MSG::default();
        while began.elapsed() < time {
            let values = [began.elapsed().as_secs_f32(), load, width as f32, height as f32];
            context.UpdateSubresource(&constants, 0, None, values.as_ptr().cast(), 0, 0);
            context.Draw(3, 0);
            if !vsync {
                context.End(&done);
                let mut finished = 0i32;
                while finished == 0 {
                    // "Not ready" is a success code too, so the answer itself says when it's done.
                    let _ = context.GetData(&done, Some((&raw mut finished).cast()), 4, 0);
                    std::thread::yield_now();
                }
            }
            if vsync || shown.elapsed() >= Duration::from_micros(16_000) {
                shown = Instant::now();
                let back: ID3D11Texture2D = swap.GetBuffer(0).unwrap();
                context.CopyResource(&back, &picture);
                swap.Present(u32::from(vsync), DXGI_PRESENT(0)).ok().unwrap();
            }
            frames += 1;
            if second.elapsed() >= Duration::from_secs(1) {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
                println!("SCENE {now} {:.1}", f64::from(frames) / second.elapsed().as_secs_f64());
                (second, frames) = (Instant::now(), 0);
            }
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        drop(swap); // before its window goes
        let _ = DestroyWindow(window);
    }
}
