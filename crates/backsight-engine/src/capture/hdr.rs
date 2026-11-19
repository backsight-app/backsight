//! Convert linear scRGB capture into limited-range BT.2020 PQ P010 with a compute shader.
//! The D3D11 video processor can't do this conversion (platform-notes.md, "HDR video").

use super::{convert::texture, d3d::Gpu, target, wgc::Frame};
use backsight_encode::session::GpuFrame;
use std::sync::OnceLock;
use windows::{
    Win32::{
        Foundation::E_FAIL,
        Graphics::{
            Direct3D::{Fxc::*, ID3DBlob},
            Direct3D11::*,
            Dxgi::Common::*,
        },
    },
    core::{Error, Interface, Result, s},
};

/// One thread per 2×2 block: four luma samples and one chroma sample. Chroma is left-sited, as
/// HEVC assumes when the stream doesn't say (NVENC writes no chroma location): level with the
/// block's even column, filtered [1/4 1/2 1/4] across its neighbours, between its two rows.
/// P010 keeps each 10-bit code in the top bits of 16, so a code is stored as `code * 64 / 65535` UNORM.
// shortcut: one bilinear tap per output pixel aliases fine detail when downscaling more than 2:1
// (4K to 1080p and below); sample a box of taps, or mip the input, if such clips look jagged.
const SHADER: &str = r"
Texture2D<float4> src : register(t0);
SamplerState linear_clamp : register(s0);
RWTexture2D<unorm float> luma : register(u0);
RWTexture2D<unorm float2> chroma : register(u1);
cbuffer Params : register(b0) { float4 rect; uint2 size; uint2 pad; };

static const float3x3 to_bt2020 = {
    0.627404, 0.329283, 0.043313,
    0.069097, 0.919540, 0.011363,
    0.016391, 0.088013, 0.895596 };

// scRGB 1.0 = 80 cd/m2. ST 2084 encodes absolute light up to 10,000 cd/m2.
// saturate also turns NaN into 0.
float3 pq(float3 scrgb) {
    float3 l = pow(saturate(scrgb * 0.008), 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * l) / (1.0 + 18.6875 * l), 78.84375);
}

// Y, Cb, Cr (Y 0..1, chroma -0.5..0.5) of output pixel p; black bars outside the picture.
float3 ycc(int2 p) {
    float2 at = float2(p) + 0.5;
    if (any(at < rect.xy) || any(at >= rect.zw)) return 0;
    float3 rgb = src.SampleLevel(linear_clamp, (at - rect.xy) / (rect.zw - rect.xy), 0).rgb;
    rgb = pq(mul(to_bt2020, rgb));
    float y = dot(rgb, float3(0.2627, 0.6780, 0.0593));
    return float3(y, (rgb.b - y) / 1.8814, (rgb.r - y) / 1.4746);
}

float code(float c) { return round(c) * 64.0 / 65535.0; }

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    int2 block = int2(id.xy * 2);
    if (block.x >= int(size.x) || block.y >= int(size.y)) return;
    float2 uv = 0;
    [unroll] for (int row = 0; row < 2; row++) {
        int2 p = block + int2(0, row);
        float3 left = ycc(p - int2(p.x > 0, 0)), here = ycc(p), right = ycc(p + int2(1, 0));
        luma[p] = code(64.0 + 876.0 * here.x);
        luma[p + int2(1, 0)] = code(64.0 + 876.0 * right.x);
        uv += (0.25 * left.yz + 0.5 * here.yz + 0.25 * right.yz) * 0.5;
    }
    chroma[id.xy] = float2(code(512.0 + 896.0 * uv.x), code(512.0 + 896.0 * uv.y));
}
";

/// The most textures a capture pool hands out (`wgc::BUFFERS` is 2, but WGC may use 3); past it
/// the oldest view goes. `forget_views` drops them all when a new window capture starts, so a
/// replaced pool's textures (66 MB each at 4K FP16) aren't kept alive.
const MAX_VIEWS: usize = 3;

pub(crate) struct Converter {
    shader: ID3D11ComputeShader,
    params: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    /// Shader views of the capture pool's few textures, each made the first time it shows up, so
    /// the shader reads WGC's own texture: no copy of the capture (30 MB per 1440p frame) first.
    views: Vec<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    /// Made when a capture texture can't be read by a shader: such frames are copied here first.
    copy: Option<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    output: ID3D11Texture2D,
    planes: [Option<ID3D11UnorderedAccessView>; 2],
    source: (u32, u32),
    groups: (u32, u32),
}

/// The shader's bytecode, compiled the first time a converter is made: a resize rebuilds the
/// converter inside the capture callback, which must stay quick.
fn bytecode() -> Result<&'static [u8]> {
    static CODE: OnceLock<std::result::Result<Vec<u8>, String>> = OnceLock::new();
    let code = CODE.get_or_init(|| {
        let blob = compile().map_err(|e| e.message())?;
        // SAFETY: the blob owns this many bytes of shader code until it drops, after this copy.
        Ok(unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize()) }.to_vec())
    });
    code.as_deref().map_err(|e| Error::new(E_FAIL, e.as_str()))
}

fn compile() -> Result<ID3DBlob> {
    let (mut code, mut errors) = (None, None);
    // SAFETY: the source pointer and length describe SHADER; names are NUL-terminated literals.
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("hdr.hlsl"),
            None,
            None,
            s!("main"),
            s!("cs_5_0"),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        let detail = errors.map(|b| blob_text(&b)).unwrap_or_default();
        return Err(Error::new(e.code(), format!("HDR shader didn't compile: {detail}")));
    }
    Ok(code.expect("D3DCompile returned code"))
}

fn blob_text(blob: &ID3DBlob) -> String {
    // SAFETY: the blob owns this many bytes until it drops, after this copy.
    unsafe {
        String::from_utf8_lossy(std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())).into()
    }
}

impl Converter {
    /// `output` must have even width and height (P010).
    pub(crate) fn new(gpu: &Gpu, source: (u32, u32), output: (u32, u32)) -> Result<Self> {
        let bytes = bytecode()?;
        let (left, top, right, bottom) = target::fit(source, output);
        let params: [u32; 8] = [
            (left as f32).to_bits(),
            (top as f32).to_bits(),
            (right as f32).to_bits(),
            (bottom as f32).to_bits(),
            output.0,
            output.1,
            0,
            0,
        ];
        let buffer = D3D11_BUFFER_DESC {
            ByteWidth: size_of_val(&params) as u32,
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: params.as_ptr().cast(), ..Default::default() };
        let sampler = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let out = texture(gpu, output, DXGI_FORMAT_P010, D3D11_BIND_UNORDERED_ACCESS)?;
        let device3: ID3D11Device3 = gpu.device.cast()?;
        let plane = |format, slice| -> Result<Option<ID3D11UnorderedAccessView>> {
            let desc = D3D11_UNORDERED_ACCESS_VIEW_DESC1 {
                Format: format,
                ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC1_0 {
                    Texture2D: D3D11_TEX2D_UAV1 { MipSlice: 0, PlaneSlice: slice },
                },
            };
            let mut view = None;
            // SAFETY: `out` is a live P010 texture with UAV binding; descriptor and out-pointer are locals.
            unsafe { device3.CreateUnorderedAccessView1(&out, Some(&desc), Some(&mut view)) }?;
            Ok(view.map(|v: ID3D11UnorderedAccessView1| v.into()))
        };
        let (mut shader, mut params_buffer, mut sampler_state) = (None, None, None);
        // SAFETY: COM calls on the live device; descriptors, initial data and out-pointers are locals.
        unsafe {
            gpu.device.CreateComputeShader(bytes, None, Some(&mut shader))?;
            gpu.device.CreateBuffer(&buffer, Some(&init), Some(&mut params_buffer))?;
            gpu.device.CreateSamplerState(&sampler, Some(&mut sampler_state))?;
        }
        Ok(Self {
            shader: shader.expect("shader created"),
            params: params_buffer.expect("buffer created"),
            sampler: sampler_state.expect("sampler created"),
            views: Vec::with_capacity(MAX_VIEWS),
            copy: None,
            output: out.clone(),
            planes: [plane(DXGI_FORMAT_R16_UNORM, 0)?, plane(DXGI_FORMAT_R16G16_UNORM, 1)?],
            source,
            groups: (output.0.div_ceil(16), output.1.div_ceil(16)),
        })
    }

    pub(crate) fn input_size(&self) -> (u32, u32) {
        self.source
    }

    /// Drops the cached views (and the textures they hold): a new window capture has its own pool.
    pub(crate) fn forget_views(&mut self) {
        self.views.clear();
    }

    /// Converts a captured FP16 frame into the P010 output on the GPU. The caller holds the
    /// device lock (the immediate context is shared with the encoder).
    pub(crate) fn convert_from(&mut self, gpu: &Gpu, frame: &Frame<'_>) -> Result<()> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: frame holds a live capture texture.
        unsafe { frame.texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_R16G16B16A16_FLOAT
            || frame.size != self.source
            || desc.Width < self.source.0
            || desc.Height < self.source.1
        {
            return Err(Error::new(E_FAIL, "Capture did not deliver the expected scRGB texture"));
        }
        let view = self.view_of(gpu, frame.texture, &desc)?;
        let ctx = &gpu.context;
        let none_views: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
        // SAFETY: same-device FP16 textures and a box that fits both. Every view, buffer and
        // shader lives in `self`; the bindings are cleared again before returning.
        unsafe {
            ctx.CSSetShader(&self.shader, None);
            ctx.CSSetShaderResources(0, Some(&[Some(view)]));
            ctx.CSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.CSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            ctx.CSSetUnorderedAccessViews(0, 2, Some(self.planes.as_ptr()), None);
            ctx.Dispatch(self.groups.0, self.groups.1, 1);
            ctx.CSSetUnorderedAccessViews(0, 2, Some(none_views.as_ptr()), None);
            ctx.CSSetShaderResources(0, Some(&[None]));
            ctx.CSSetShader(None::<&ID3D11ComputeShader>, None);
        }
        Ok(())
    }

    /// The view the shader reads `src` through: its own (cached) when it is exactly the capture
    /// size and allows shader views, else a view of a copy of it made now. Microsoft doesn't
    /// document the capture pool's bind flags (platform-notes.md), hence the check and the copy.
    fn view_of(
        &mut self,
        gpu: &Gpu,
        src: &ID3D11Texture2D,
        desc: &D3D11_TEXTURE2D_DESC,
    ) -> Result<ID3D11ShaderResourceView> {
        if let Some((_, view)) = self.views.iter().find(|(t, _)| t == src) {
            return Ok(view.clone());
        }
        // The shader samples the whole texture, so a larger one must be cut to size by the copy.
        let readable = desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0
            && (desc.Width, desc.Height) == self.source
            && desc.MipLevels == 1;
        // Once copying, keep copying: a refused view is then tried (and logged) only once.
        if readable && self.copy.is_none() {
            let mut view = None;
            // SAFETY: `src` lives on this device; the out-pointer is a valid local.
            match unsafe { gpu.device.CreateShaderResourceView(src, None, Some(&mut view)) } {
                Ok(()) => {
                    let view = view.expect("view created");
                    if self.views.len() == MAX_VIEWS {
                        // Expected never: a pool cycling more textures than this makes a view a frame.
                        static ONCE: std::sync::Once = std::sync::Once::new();
                        ONCE.call_once(|| tracing::warn!("HDR capture: more textures than its pool; views are remade"));
                        self.views.remove(0);
                    }
                    self.views.push((src.clone(), view.clone()));
                    return Ok(view);
                }
                Err(e) => tracing::warn!("a shader can't read this capture texture ({e})"),
            }
        }
        if self.copy.is_none() {
            tracing::warn!(
                "HDR capture textures can't be read directly (bind flags {:#x}, {}x{}); copying each frame",
                desc.BindFlags,
                desc.Width,
                desc.Height
            );
            let tex = texture(gpu, self.source, DXGI_FORMAT_R16G16B16A16_FLOAT, D3D11_BIND_SHADER_RESOURCE)?;
            let mut view = None;
            // SAFETY: `tex` was just made on this device with shader-resource binding.
            unsafe { gpu.device.CreateShaderResourceView(&tex, None, Some(&mut view)) }?;
            self.copy = Some((tex, view.expect("view created")));
        }
        let (tex, view) = self.copy.as_ref().expect("made above");
        let area = D3D11_BOX { right: self.source.0, bottom: self.source.1, back: 1, ..Default::default() };
        // SAFETY: same-device FP16 textures; `area` is exactly `tex` and fits `src` (the caller
        // checked it is at least the capture size).
        unsafe { gpu.context.CopySubresourceRegion(tex, 0, 0, 0, 0, src, 0, Some(&area)) };
        Ok(view.clone())
    }

    /// Copies the latest converted frame into the encoder's P010 input frame, which must come
    /// from a session opened with this `gpu`'s `for_ffmpeg()` at this converter's output size.
    pub(crate) fn copy_to_encoder(&self, gpu: &Gpu, frame: GpuFrame<'_>) {
        let texture = frame.texture();
        // SAFETY: `frame` borrows its session, so the FFmpeg P010 texture on `gpu`'s device is
        // alive for this call; we only borrow it.
        unsafe {
            let dst = ID3D11Texture2D::from_raw_borrowed(&texture).expect("FFmpeg returned a texture");
            gpu.context.CopySubresourceRegion(dst, frame.index(), 0, 0, 0, &self.output, 0, None);
        }
    }

    #[cfg(test)]
    fn output(&self) -> &ID3D11Texture2D {
        &self.output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PCI vendor id of WARP (Microsoft Basic Render Driver), which CI uses.
    const MICROSOFT: u32 = 0x1414;

    /// Y, Cb and Cr for linear scRGB, worked out on the CPU: BT.709 → BT.2020 primaries in
    /// linear light, ST 2084 at 80 nits per 1.0, then the BT.2020 non-constant-luminance matrix.
    fn ycc(rgb: [f32; 3]) -> [f32; 3] {
        let pq = |scrgb: f32| {
            let l = (scrgb * 0.008).clamp(0.0, 1.0).powf(0.159_301_76);
            ((0.835_937_5 + 18.851_563 * l) / (1.0 + 18.687_5 * l)).powf(78.843_75)
        };
        let [r, g, b] = rgb.map(|x| if x.is_nan() { 0.0 } else { x });
        let r2 = pq(0.627_404 * r + 0.329_283 * g + 0.043_313 * b);
        let g2 = pq(0.069_097 * r + 0.919_540 * g + 0.011_363 * b);
        let b2 = pq(0.016_391 * r + 0.088_013 * g + 0.895_596 * b);
        let y = 0.2627 * r2 + 0.6780 * g2 + 0.0593 * b2;
        [y, (b2 - y) / 1.8814, (r2 - y) / 1.4746]
    }

    /// Limited-range 10-bit codes.
    fn codes([y, cb, cr]: [f32; 3]) -> [u16; 3] {
        let code = |base: f32, scale: f32, x: f32| (base + scale * x).round() as u16;
        [code(64.0, 876.0, y), code(512.0, 896.0, cb), code(512.0, 896.0, cr)]
    }

    fn reference(rgb: [f32; 3]) -> [u16; 3] {
        codes(ycc(rgb))
    }

    #[test]
    fn reference_maths() {
        // 80 nits, a 280-nit SDR white, 1,000 and 10,000 nits.
        for (scrgb, y) in [(0.0, 64), (1.0, 490), (3.5, 602), (12.5, 723), (125.0, 940)] {
            assert_eq!(reference([scrgb; 3]), [y, 512, 512], "{scrgb}");
        }
        // BT.2020 red at 80 nits, given in scRGB with negative green and blue.
        assert_eq!(reference([1.660_491, -0.124_550, -0.018_151]), [176, 451, 730]);
    }

    fn convert_solid(gpu: &Gpu, rgb: [f32; 3], input: (u32, u32), output: (u32, u32)) -> (Vec<u16>, Vec<u16>) {
        convert(gpu, &vec![rgb; (input.0 * input.1) as usize], input, output)
    }

    /// Converts a row-major scRGB picture and returns the P010 planes as 10-bit codes, without
    /// the textures' row padding.
    /// Both ways in: straight from a texture a shader may read (as WGC's are expected to be), and
    /// through the copy for one it may not. They must give the same picture.
    fn convert(gpu: &Gpu, picture: &[[f32; 3]], input: (u32, u32), output: (u32, u32)) -> (Vec<u16>, Vec<u16>) {
        let direct = convert_bound(gpu, picture, input, output, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE);
        let copied = convert_bound(gpu, picture, input, output, D3D11_BIND_RENDER_TARGET);
        assert_eq!(direct, copied, "{input:?} → {output:?}: reading the capture directly differs from copying it");
        direct
    }

    fn convert_bound(
        gpu: &Gpu,
        picture: &[[f32; 3]],
        input: (u32, u32),
        output: (u32, u32),
        bind: D3D11_BIND_FLAG,
    ) -> (Vec<u16>, Vec<u16>) {
        let pixels: Vec<u16> =
            picture.iter().flat_map(|p| [p[0], p[1], p[2], 1.0].map(|x| half::f16::from_f32(x).to_bits())).collect();
        assert_eq!(pixels.len(), (input.0 * input.1 * 4) as usize);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: input.0,
            Height: input.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind.0 as u32,
            ..Default::default()
        };
        let init =
            D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr().cast(), SysMemPitch: input.0 * 8, SysMemSlicePitch: 0 };
        let mut src = None;
        // SAFETY: `pixels` outlives the call and matches `desc`.
        unsafe { gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut src)) }.unwrap();
        let src = src.unwrap();
        let mut conv = Converter::new(gpu, input, output).unwrap();
        // Twice, as a capture pool hands the same texture out again: one view, made once.
        for _ in 0..2 {
            conv.convert_from(gpu, &Frame { texture: &src, size: input, time: 0, handle: None }).unwrap();
        }
        let readable = bind.0 & D3D11_BIND_SHADER_RESOURCE.0 != 0;
        assert_eq!((conv.views.len(), conv.copy.is_some()), (usize::from(readable), !readable));
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Width: output.0,
            Height: output.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_P010,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let (mut staging, mut mapped) = (None, D3D11_MAPPED_SUBRESOURCE::default());
        let (w, h) = (output.0 as usize, output.1 as usize);
        // SAFETY: same-size P010 textures on one device; Map waits for the copy. A mapped P010
        // texture is `pitch × h` bytes of Y, then `pitch × h/2` of interleaved UV, all u16.
        let (y, uv) = unsafe {
            gpu.device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).unwrap();
            let staging = staging.unwrap();
            gpu.context.CopyResource(&staging, conv.output());
            gpu.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).unwrap();
            let pitch = mapped.RowPitch as usize / 2;
            let data = std::slice::from_raw_parts(mapped.pData as *const u16, pitch * (h + h / 2));
            let rows = |from: usize, count: usize| -> Vec<u16> {
                (from..from + count).flat_map(|r| data[r * pitch..][..w].iter().copied()).collect()
            };
            let planes = (rows(0, h), rows(h, h / 2));
            gpu.context.Unmap(&staging, 0);
            planes
        };
        assert!(y.iter().chain(&uv).all(|v| v & 63 == 0), "10-bit codes in the top bits of both planes");
        (y.iter().map(|v| v >> 6).collect(), uv.iter().map(|v| v >> 6).collect())
    }

    /// The GPU shader matches the CPU maths: absolute light, BT.2020 primaries, colours outside
    /// BT.709 as negative scRGB, scaling (also by an uneven ratio), black bars for another shape
    /// and left-sited chroma. On the real GPU and on WARP (the software renderer CI has).
    #[test]
    fn converts_scrgb_to_bt2020_pq_p010_on_the_gpu() {
        let gpu = Gpu::open("cpu").unwrap();
        if gpu.vendor_id != MICROSOFT {
            matches_reference(&gpu);
        }
        matches_reference(&Gpu::warp().unwrap());
    }

    fn matches_reference(gpu: &Gpu) {
        println!("GPU: {}", gpu.name);
        for (input, output) in [((64, 64), (64, 64)), ((128, 72), (64, 36)), ((90, 50), (64, 36))] {
            for rgb in [
                [0.0; 3],
                [1.0; 3],
                [3.5; 3],
                [12.5; 3],
                [125.0; 3],
                [3.5, 0.0, 0.0],
                [0.0, 3.5, 0.0],
                [0.0, 0.0, 3.5],
                [1.660_491, -0.124_550, -0.018_151],
                [f32::NAN; 3],
            ] {
                let (y, uv) = convert_solid(gpu, rgb, input, output);
                let (w, h) = (output.0 as usize, output.1 as usize);
                let got = [y[h / 2 * w + w / 2], uv[h / 4 * w + w / 2], uv[h / 4 * w + w / 2 + 1]];
                let want = reference(rgb);
                assert!(
                    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1),
                    "{input:?} → {output:?}, {rgb:?}: got {got:?}, want {want:?}"
                );
            }
        }
        // A square picture in a wide frame: limited-range black bars left and right.
        let (y, uv) = convert_solid(gpu, [3.5; 3], (64, 64), (128, 64));
        let row = 32 * 128;
        assert_eq!([y[row + 4], y[row + 64], y[row + 124]], [64, 602, 64]);
        assert_eq!([uv[16 * 128 + 4], uv[16 * 128 + 5]], [512, 512]);

        // Left-sited chroma: red columns 0–3, blue 4–7. The chroma sample at column 4 takes
        // [1/4 1/2 1/4] of columns 3, 4 and 5 (centre siting would give pure blue there); the
        // one at column 0 repeats the edge and stays pure red.
        let (red, blue) = ([3.5, 0.0, 0.0], [0.0, 0.0, 3.5]);
        let picture: Vec<_> = (0..16).map(|i| if i % 8 < 4 { red } else { blue }).collect();
        let (_, uv) = convert(gpu, &picture, (8, 2), (8, 2));
        let (r, b) = (ycc(red), ycc(blue));
        let mixed = codes([0.0, 0.25 * r[1] + 0.75 * b[1], 0.25 * r[2] + 0.75 * b[2]]);
        let near = |got: &[u16], want: &[u16]| got.iter().zip(want).all(|(g, w)| g.abs_diff(*w) <= 1);
        assert!(near(&uv[4..6], &mixed[1..]), "column 4: got {:?}, want {:?}", &uv[4..6], &mixed[1..]);
        assert!(near(&uv[0..2], &reference(red)[1..]), "column 0: got {:?}", &uv[0..2]);
    }
}
