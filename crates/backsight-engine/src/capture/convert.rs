//! BGRA to NV12 on the GPU with `ID3D11VideoProcessor` (the driver picks the engine; it shares
//! the GPU with the game, see platform-notes.md), scaling to the output size in the same pass.
//! BT.709 limited range, as the encoder session tags the stream (`capture-pipeline.md` §3).

use super::d3d::Gpu;
use backsight_encode::session::{GpuFrame, Planes};
use std::mem::ManuallyDrop;
use windows::{
    Win32::{
        Foundation::E_NOTIMPL,
        Graphics::{Direct3D11::*, Dxgi::Common::*},
    },
    core::{Error, Interface, Result},
};

pub struct Converter {
    video_device: ID3D11VideoDevice,
    enumerator: ID3D11VideoProcessorEnumerator,
    video_ctx: ID3D11VideoContext1,
    processor: ID3D11VideoProcessor,
    /// Input views of the capture pool's few textures, each made the first time it shows up.
    views: Vec<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    views_made: usize,
    /// Set when the driver refuses a view on capture textures: copy each frame here instead.
    copy: Option<(ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    output: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
    in_size: (u32, u32),
}

/// More distinct textures than a capture pool ever has (`wgc::BUFFERS`); past it, start over.
const MAX_VIEWS: usize = 8;

const NO_VIDEO_PROCESSOR: &str =
    "Your graphics card can't convert video frames, so Backsight can't record. Install or update your graphics driver.";

fn texture(gpu: &Gpu, (w, h): (u32, u32), format: DXGI_FORMAT, bind: D3D11_BIND_FLAG) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as u32,
        ..Default::default()
    };
    let mut tex = None;
    // SAFETY: `desc` and the out-pointer are valid locals.
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)) }?;
    Ok(tex.expect("CreateTexture2D succeeded"))
}

/// A black picture of `size`, bound as WGC's pool textures are and cleared on the GPU: window
/// mode records it until a game window is captured, so files made meanwhile still have video. It
/// is made at the size the converter starts with, so nothing is scaled and the game's first frame
/// is no size change when it matches. FP16 (scRGB 0) when `hdr`.
pub fn black(gpu: &Gpu, size: (u32, u32), hdr: bool) -> Result<ID3D11Texture2D> {
    let format = if hdr { DXGI_FORMAT_R16G16B16A16_FLOAT } else { DXGI_FORMAT_B8G8R8A8_UNORM };
    let tex = texture(gpu, size, format, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE)?;
    let mut view = None;
    // SAFETY: `tex` was just made on this device with render-target binding; the out-pointer is a
    // valid local. The immediate context is shared with other threads, hence the device lock.
    unsafe {
        gpu.device.CreateRenderTargetView(&tex, None, Some(&mut view))?;
        let _device = gpu.lock();
        gpu.context.ClearRenderTargetView(&view.expect("view created"), &[0.0, 0.0, 0.0, 1.0]);
    }
    Ok(tex)
}

impl Converter {
    /// `output` must have even width and height (NV12).
    pub fn new(gpu: &Gpu, input: (u32, u32), output: (u32, u32), fps: u32) -> Result<Self> {
        // shortcut: no video processor (WARP, a PC with no GPU driver) means no capture; add a
        // shader or swscale fallback if virtual machines or remote desktops need recording.
        let video_device: ID3D11VideoDevice =
            gpu.device.cast().map_err(|e| Error::new(e.code(), NO_VIDEO_PROCESSOR))?;
        let video_ctx: ID3D11VideoContext1 = gpu.context.cast()?;
        let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let output_tex = texture(gpu, output, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET)?;
        // SAFETY: COM calls on live objects; descriptors and out-pointers are valid locals.
        unsafe {
            let enumerator = video_device.CreateVideoProcessorEnumerator(&desc)?;
            if enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?
                & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32
                == 0
            {
                return Err(Error::new(E_NOTIMPL, NO_VIDEO_PROCESSOR));
            }
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
            video_ctx.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            video_ctx.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            // No driver "enhancements" (denoise, edge sharpening, colour boosts).
            video_ctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            // A picture of another shape (a window, a monitor that changed resolution) keeps its
            // shape, centred, with black bars: the encoder's size never changes mid-stream.
            let (left, top, right, bottom) = super::target::fit(input, output);
            let rect = windows::Win32::Foundation::RECT {
                left: left as i32,
                top: top as i32,
                right: right as i32,
                bottom: bottom as i32,
            };
            video_ctx.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&rect));
            // Given in the output's own YCbCr (studio range: Y 16/255, chroma at the middle).
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    YCbCr: D3D11_VIDEO_COLOR_YCbCrA { Y: 16.0 / 255.0, Cb: 0.5, Cr: 0.5, A: 1.0 },
                },
            };
            video_ctx.VideoProcessorSetOutputBackgroundColor(&processor, true, &black);

            let mut output_view = None;
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            video_device.CreateVideoProcessorOutputView(&output_tex, &enumerator, &out_desc, Some(&mut output_view))?;
            Ok(Converter {
                video_device,
                enumerator,
                video_ctx,
                processor,
                views: Vec::with_capacity(MAX_VIEWS),
                views_made: 0,
                copy: None,
                output: output_tex,
                output_view: output_view.expect("view created"),
                in_size: input,
            })
        }
    }

    /// Converts a captured BGRA texture into the NV12 output, straight from `src` where the
    /// driver allows (no copy of the capture first), so the frame can go back to WGC at once.
    /// Returns `Ok(false)` and converts nothing when `size` isn't the size this converter was
    /// built for: the caller must rebuild it.
    pub fn convert_from(&mut self, gpu: &Gpu, src: &ID3D11Texture2D, size: (u32, u32)) -> Result<bool> {
        if size != self.in_size {
            return Ok(false);
        }
        let view = if let Some((tex, view)) = &self.copy {
            let area = D3D11_BOX { left: 0, top: 0, front: 0, right: size.0, bottom: size.1, back: 1 };
            // SAFETY: both textures live on `gpu`'s device and are BGRA; `area` is exactly `tex`
            // and `src` is at least `size` (checked above).
            unsafe { gpu.context.CopySubresourceRegion(tex, 0, 0, 0, 0, src, 0, Some(&area)) };
            view.clone()
        } else if let Some((_, v)) = self.views.iter().find(|(t, _)| t == src) {
            v.clone()
        } else {
            match self.input_view(src) {
                Ok(v) => {
                    if self.views.len() == MAX_VIEWS {
                        self.views.clear();
                    }
                    self.views.push((src.clone(), v.clone()));
                    self.views_made += 1;
                    v
                }
                Err(e) => {
                    tracing::warn!("the video processor can't read capture textures ({e}); copying each frame");
                    let bind = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
                    let tex = texture(gpu, size, DXGI_FORMAT_B8G8R8A8_UNORM, bind)?;
                    let view = self.input_view(&tex)?;
                    self.copy = Some((tex, view));
                    return self.convert_from(gpu, src, size);
                }
            }
        };
        let mut streams = [D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(view)),
            ..Default::default()
        }];
        // SAFETY: processor and views were created on this device; `src` is exactly `in_size`.
        let result = unsafe { self.video_ctx.VideoProcessorBlt(&self.processor, &self.output_view, 0, &streams) };
        // SAFETY: releases the view moved into the stream above, exactly once.
        unsafe { ManuallyDrop::drop(&mut streams[0].pInputSurface) };
        result.map(|()| true)
    }

    fn input_view(&self, src: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView> {
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut view = None;
        // SAFETY: `src` lives on this device; descriptor and out-pointer are valid locals.
        unsafe { self.video_device.CreateVideoProcessorInputView(src, &self.enumerator, &desc, Some(&mut view)) }?;
        Ok(view.expect("view created"))
    }

    /// The capture size this converter was built for.
    pub fn input_size(&self) -> (u32, u32) {
        self.in_size
    }

    /// How many capture textures got their own input view; stays at the pool size when the
    /// cache works.
    pub fn views_made(&self) -> usize {
        self.views_made
    }

    /// The latest converted frame (NV12, output size).
    pub fn output(&self) -> &ID3D11Texture2D {
        &self.output
    }

    /// The zero-copy path: copies the latest converted frame into the encoder's input frame.
    /// `frame` must come from a session opened with this `gpu`'s `for_ffmpeg()`, at this
    /// converter's output size.
    pub fn copy_to_encoder(&self, gpu: &Gpu, frame: GpuFrame<'_>) {
        let texture = frame.texture();
        // SAFETY: `frame` borrows its session, so the FFmpeg frame holding `texture` (an NV12
        // texture on `gpu`'s device) is alive for this call; we only borrow it.
        unsafe {
            let dst = ID3D11Texture2D::from_raw_borrowed(&texture).expect("FFmpeg returned a texture");
            gpu.context.CopySubresourceRegion(dst, frame.index(), 0, 0, 0, &self.output, 0, None);
        }
    }
}

/// Reads NV12 textures back into encoder frames, for encoders without GPU input (CPU
/// encoders, and Quick Sync for now). One GPU-to-CPU copy per frame.
pub struct Downloader {
    staging: ID3D11Texture2D,
    size: (u32, u32),
}

impl Downloader {
    pub fn new(gpu: &Gpu, size: (u32, u32)) -> Result<Self> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut staging = None;
        // SAFETY: `desc` and the out-pointer are valid locals.
        unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut staging)) }?;
        Ok(Downloader { staging: staging.expect("CreateTexture2D succeeded"), size })
    }

    /// Copies `src` (NV12, this size) into encoder planes: NV12 as is, or split into yuv420p.
    pub fn download(&self, gpu: &Gpu, src: &ID3D11Texture2D, planes: Planes) -> Result<()> {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: same-size NV12 textures on one device; Map waits for the copy to finish.
        unsafe {
            gpu.context.CopyResource(&self.staging, src);
            gpu.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        let pitch = mapped.RowPitch as usize;
        // SAFETY: a mapped NV12 staging texture is `pitch × h` bytes of Y followed by `pitch × h/2`
        // bytes of interleaved UV, readable until Unmap below.
        let data = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (h + h / 2)) };
        let (y_src, uv_src) = data.split_at(pitch * h);
        let [y, u, v] = planes;
        if let Some((y, stride)) = y {
            for row in 0..h {
                y[row * stride..][..w].copy_from_slice(&y_src[row * pitch..][..w]);
            }
        }
        match (u, v) {
            (Some((uv, stride)), None) => {
                for row in 0..h / 2 {
                    uv[row * stride..][..w].copy_from_slice(&uv_src[row * pitch..][..w]);
                }
            }
            (Some((u, us)), Some((v, vs))) => {
                for row in 0..h / 2 {
                    let line = &uv_src[row * pitch..][..w];
                    for (x, pair) in line.as_chunks::<2>().0.iter().enumerate() {
                        u[row * us + x] = pair[0];
                        v[row * vs + x] = pair[1];
                    }
                }
            }
            _ => {}
        }
        // SAFETY: balances the Map above; `data` is not used after this.
        unsafe { gpu.context.Unmap(&self.staging, 0) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Converts a solid BGRA colour and returns the centre pixel's (Y, U, V).
    /// `bind` as WGC's pool textures (render target and shader resource) takes the direct path;
    /// a driver that refuses a view on the source takes the copy fallback. Both must convert.
    fn convert_solid(gpu: &Gpu, bgra: [u8; 4], input: (u32, u32), output: (u32, u32), bind: bool) -> (u8, u8, u8) {
        let pixels: Vec<u8> = bgra.iter().copied().cycle().take((input.0 * input.1 * 4) as usize).collect();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: input.0,
            Height: input.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: if bind { (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32 } else { 0 },
            ..Default::default()
        };
        let init =
            D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr().cast(), SysMemPitch: input.0 * 4, SysMemSlicePitch: 0 };
        let mut src = None;
        // SAFETY: `pixels` outlives the call and matches `desc`.
        unsafe { gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut src)) }.unwrap();

        let mut conv = Converter::new(gpu, input, output, 60).unwrap();
        let src = src.unwrap();
        assert!(conv.convert_from(gpu, &src, input).unwrap());
        assert!(conv.convert_from(gpu, &src, input).unwrap(), "second frame");
        assert!(conv.views_made() <= 1, "the view is reused");
        assert!(!conv.convert_from(gpu, &src, (input.0 + 2, input.1)).unwrap(), "size change");
        let nv12 = conv.output();
        let (w, h) = (output.0 as usize, output.1 as usize);
        let (mut y, mut uv) = (vec![0u8; w * h], vec![0u8; w * h / 2]);
        Downloader::new(gpu, output)
            .unwrap()
            .download(gpu, nv12, [Some((&mut y, w)), Some((&mut uv, w)), None])
            .unwrap();
        let (cx, cy) = ((w / 2) & !1, h / 2);
        // The planar split for encoders that take yuv420p (x265, SVT-AV1, AOM): U and V must be
        // NV12's interleaved pairs, in that order, or their recordings come out tinted.
        let (cw, ch) = (w / 2, h / 2);
        let (mut y3, mut u, mut v) = (vec![0u8; w * h], vec![0u8; cw * ch], vec![0u8; cw * ch]);
        Downloader::new(gpu, output)
            .unwrap()
            .download(gpu, nv12, [Some((&mut y3, w)), Some((&mut u, cw)), Some((&mut v, cw))])
            .unwrap();
        let c = cy / 2 * cw + cx / 2;
        assert_eq!(y3, y, "planar Y matches NV12 Y");
        assert_eq!((u[c], v[c]), (uv[cy / 2 * w + cx], uv[cy / 2 * w + cx + 1]), "planar U and V");
        (y[cy * w + cx], uv[cy / 2 * w + cx], uv[cy / 2 * w + cx + 1])
    }

    fn assert_close(got: (u8, u8, u8), want: (u8, u8, u8), what: &str) {
        let close = |a: u8, b: u8| a.abs_diff(b) <= 2;
        assert!(
            close(got.0, want.0) && close(got.1, want.1) && close(got.2, want.2),
            "{what}: got {got:?}, want {want:?}"
        );
    }

    /// BT.709 limited range: Y = 16 + 219·Y', Cb/Cr = 128 + 224·(B'−Y')/1.8556 and (R'−Y')/1.5748.
    /// A BT.601 mix-up would give red Y ≈ 81 instead of 63.
    #[test]
    fn converts_to_bt709_limited_range_with_scaling() {
        let gpu = Gpu::open("cpu").unwrap();
        println!("GPU: {}", gpu.name);
        if gpu.vendor_id == MICROSOFT {
            println!("skipped: WARP has no video processor (CI); see warp_explains_why_it_cannot_convert");
            return;
        }
        for bind in [true, false] {
            for (input, output) in [((64, 64), (64, 64)), ((128, 72), (64, 36))] {
                let solid = |bgra| convert_solid(&gpu, bgra, input, output, bind);
                assert_close(solid([255, 255, 255, 255]), (235, 128, 128), "white");
                assert_close(solid([0, 0, 0, 255]), (16, 128, 128), "black");
                assert_close(solid([0, 0, 255, 255]), (63, 102, 240), "red");
            }
        }
        // Window mode's picture before a game is found.
        let mut conv = Converter::new(&gpu, (64, 36), (64, 36), 60).unwrap();
        assert!(conv.convert_from(&gpu, &black(&gpu, (64, 36), false).unwrap(), (64, 36)).unwrap());
        let (mut y, mut uv) = (vec![0u8; 64 * 36], vec![0u8; 64 * 18]);
        Downloader::new(&gpu, (64, 36))
            .unwrap()
            .download(&gpu, conv.output(), [Some((&mut y, 64)), Some((&mut uv, 64)), None])
            .unwrap();
        assert!(y.iter().all(|&v| v.abs_diff(16) <= 2) && uv.iter().all(|&v| v.abs_diff(128) <= 2), "all black");
    }

    /// A square picture in a wide frame keeps its shape: black bars left and right. On every GPU
    /// with a video processor: the dGPU and, on a hybrid laptop, the Intel iGPU.
    #[test]
    fn another_shape_gets_black_bars() {
        let mut tested = Vec::new();
        for vendor in ["cpu", "nvidia", "amd", "intel"] {
            let gpu = Gpu::open(vendor).unwrap();
            if gpu.vendor_id == MICROSOFT || tested.contains(&gpu.vendor_id) {
                continue; // WARP (CI) has no video processor
            }
            println!("black bars on {}", gpu.name);
            black_bars(&gpu);
            tested.push(gpu.vendor_id);
        }
    }

    fn black_bars(gpu: &Gpu) {
        let (input, output) = ((64u32, 64u32), (128u32, 64u32));
        let pixels = vec![255u8; (input.0 * input.1 * 4) as usize];
        let desc = D3D11_TEXTURE2D_DESC {
            Width: input.0,
            Height: input.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            ..Default::default()
        };
        let init =
            D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr().cast(), SysMemPitch: input.0 * 4, SysMemSlicePitch: 0 };
        let mut src = None;
        // SAFETY: `pixels` outlives the call and matches `desc`.
        unsafe { gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut src)) }.unwrap();
        let mut conv = Converter::new(gpu, input, output, 60).unwrap();
        assert!(conv.convert_from(gpu, &src.unwrap(), input).unwrap());
        let (w, h) = (output.0 as usize, output.1 as usize);
        let (mut y, mut uv) = (vec![0u8; w * h], vec![0u8; w * h / 2]);
        Downloader::new(gpu, output)
            .unwrap()
            .download(gpu, conv.output(), [Some((&mut y, w)), Some((&mut uv, w)), None])
            .unwrap();
        let row = h / 2 * w;
        assert!(y[row + 4].abs_diff(16) <= 2, "left bar is black: {}", y[row + 4]);
        assert!(y[row + w - 4].abs_diff(16) <= 2, "right bar is black: {}", y[row + w - 4]);
        assert!(y[row + w / 2].abs_diff(235) <= 2, "the picture is in the middle: {}", y[row + w / 2]);
    }

    /// PCI vendor id of WARP (Microsoft Basic Render Driver).
    const MICROSOFT: u32 = 0x1414;

    #[test]
    fn warp_explains_why_it_cannot_convert() {
        let gpu = Gpu::warp().unwrap();
        assert_eq!(gpu.vendor_id, MICROSOFT);
        let Err(e) = Converter::new(&gpu, (64, 64), (64, 64), 60) else { panic!("WARP gained a video processor") };
        assert_eq!(e.message(), NO_VIDEO_PROCESSOR);
    }
}
