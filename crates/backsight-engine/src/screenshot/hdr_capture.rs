//! One-shot floating-point WGC capture, independent of the recording frame pool.
use super::Target;
use crate::capture::d3d::Gpu;
use std::{
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
use windows::{
    Foundation::{Metadata::ApiInformation, TypedEventHandler},
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::{E_FAIL, E_INVALIDARG},
        Graphics::{
            Direct3D11::*,
            Dxgi::{Common::*, IDXGIDevice},
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
    },
    core::{Error, Interface, Ref, Result, h},
};

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::R16G16B16A16Float;

pub(super) fn capture(gpu: Arc<Gpu>, target: Target, cursor: bool) -> Result<(Vec<u8>, u32, u32)> {
    let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    // SAFETY: The handles come from Windows. Interop rejects stale or invalid handles.
    let item: GraphicsCaptureItem = unsafe {
        match target {
            Target::Window(window) => interop.CreateForWindow(window),
            Target::Monitor(monitor) => interop.CreateForMonitor(monitor),
        }
    }?;
    let size = item.Size()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&winrt_device(&gpu)?, FORMAT, 2, size)?;
    let session = pool.CreateCaptureSession(&item)?;
    let (send, receive) = mpsc::sync_channel(1);
    let closed = send.clone();
    let state = Mutex::new((size, false));
    let frame_token = pool.FrameArrived(&TypedEventHandler::new(move |pool: Ref<Direct3D11CaptureFramePool>, _| {
        let pool = pool.ok()?;
        let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.1 {
            return Ok(());
        }
        let Ok(frame) = pool.TryGetNextFrame() else { return Ok(()) };
        let result = (|| {
            let content = frame.ContentSize()?;
            if content != state.0 {
                drop(frame);
                pool.Recreate(&winrt_device(&gpu)?, FORMAT, 2, content)?;
                state.0 = content;
                return Ok(None);
            }
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            // SAFETY: Our D3D11 frame pool supplies a texture on this GPU.
            let texture: ID3D11Texture2D = unsafe { access.GetInterface() }?;
            read(&gpu, &texture, content.Width as u32, content.Height as u32).map(Some)
        })();
        if let Some(result) = result.transpose() {
            state.1 = true;
            let _ = send.try_send(result);
        }
        Ok(())
    }))?;
    let mut closed_token = None;
    let result = (|| {
        closed_token = Some(item.Closed(&TypedEventHandler::new(move |_, _| {
            let _ = closed.try_send(Err(Error::new(E_FAIL, "The capture target closed.")));
            Ok(())
        }))?);
        let class = h!("Windows.Graphics.Capture.GraphicsCaptureSession");
        if ApiInformation::IsPropertyPresent(class, h!("IsCursorCaptureEnabled")).unwrap_or(false) {
            session.SetIsCursorCaptureEnabled(cursor)?;
        }
        if ApiInformation::IsPropertyPresent(class, h!("IsBorderRequired")).unwrap_or(false) {
            let _ = session.SetIsBorderRequired(false);
        }
        session.StartCapture()?;
        receive.recv_timeout(Duration::from_secs(5)).map_err(|e| Error::new(E_FAIL, e.to_string()))?
    })();
    if let Some(token) = closed_token {
        let _ = item.RemoveClosed(token);
    }
    let _ = pool.RemoveFrameArrived(frame_token);
    let _ = session.Close();
    let _ = pool.Close();
    result
}

fn winrt_device(gpu: &Gpu) -> Result<IDirect3DDevice> {
    let device = gpu.device.cast::<IDXGIDevice>()?;
    // SAFETY: The wrapper holds its own reference to this live DXGI device.
    unsafe { CreateDirect3D11DeviceFromDXGIDevice(&device) }?.cast()
}

fn read(gpu: &Gpu, source: &ID3D11Texture2D, width: u32, height: u32) -> Result<(Vec<u8>, u32, u32)> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: The source is a live texture and the descriptor is a writable local.
    unsafe { source.GetDesc(&mut desc) };
    if width == 0
        || height == 0
        || width > desc.Width
        || height > desc.Height
        || desc.Format != DXGI_FORMAT_R16G16B16A16_FLOAT
    {
        return Err(Error::new(E_INVALIDARG, "Unexpected HDR texture size or format."));
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: desc.Format,
        SampleDesc: desc.SampleDesc,
        Usage: D3D11_USAGE_STAGING,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        ..Default::default()
    };
    let row = width as usize * 8;
    let mut pixels = vec![0; row * height as usize];
    let mut staging = None;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    let area = D3D11_BOX { left: 0, top: 0, front: 0, right: width, bottom: height, back: 1 };
    let _lock = gpu.lock();
    // SAFETY: Copy uses matching same-device formats and a checked rectangle. Map waits for the
    // copy, and each mapped row has at least the requested width in bytes.
    unsafe {
        gpu.device.CreateTexture2D(&desc, None, Some(&mut staging))?;
        let staging = staging.expect("texture created");
        gpu.context.CopySubresourceRegion(&staging, 0, 0, 0, 0, source, 0, Some(&area));
        gpu.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        for (y, out) in pixels.chunks_exact_mut(row).enumerate() {
            std::ptr::copy_nonoverlapping(
                mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                out.as_mut_ptr(),
                row,
            );
        }
        gpu.context.Unmap(&staging, 0);
    }
    for pixel in pixels.as_chunks_mut::<8>().0 {
        pixel[6..8].copy_from_slice(&0x3c00u16.to_le_bytes()); // opaque half-float alpha
    }
    Ok((pixels, width, height))
}
