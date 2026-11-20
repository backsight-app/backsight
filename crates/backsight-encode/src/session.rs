//! Encoder sessions: frames or samples in, packets out. Part of the FFmpeg edge.
//!
//! Each session owns one reused input frame and one reused output packet, so steady-state
//! encoding allocates nothing on the Rust side. Packets are handed to a sink as borrowed
//! [`Encoded`] values; the caller decides whether to copy them (the replay buffer does).

use crate::{
    catalog::{AudioEncoder, Catalog, VideoEncoder},
    ffmpeg::{find_encoder, open_with_options, set_if_known},
    options::{FfmpegOptions, audio_options, limit_bframes, video_options, without_bframes},
};
use backsight_config::EncoderOptions;
use rsmpeg::{
    avcodec::{AVCodecContext, AVCodecParameters, AVPacket},
    avutil::{AVChannelLayout, AVFrame, AVHWDeviceContext, AVHWFramesContext, ra},
    ffi,
};
use std::ffi::c_void;

pub type Result<T> = std::result::Result<T, String>;

/// What a video session is opened for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Sdr,
    Hdr,
    /// SDR, without the retry that takes B-frames out when NVENC refuses them.
    Exact,
}

/// One encoded packet, borrowed from the session until the sink returns.
#[derive(Debug, Clone, Copy)]
pub struct Encoded<'a> {
    pub data: &'a [u8],
    /// Presentation and decode timestamps in microseconds.
    pub pts_us: i64,
    pub dts_us: i64,
    /// Packet duration in microseconds; 0 when the encoder does not provide it.
    pub duration_us: i64,
    pub keyframe: bool,
}

/// A stream's codec parameters for [`crate::mux::Muxer::create`]. Taken once after the session
/// opens, so clips can be saved on another thread while the session keeps encoding.
#[derive(Clone)]
pub struct StreamParams {
    pub(crate) codecpar: AVCodecParameters,
    pub(crate) time_base: ffi::AVRational,
}

/// BT.709 limited range, the SDR colour the capture pipeline produces (`capture-pipeline.md` §3).
const SDR_COLOR: &[(&str, &str)] =
    &[("colorspace", "bt709"), ("color_primaries", "bt709"), ("color_trc", "bt709"), ("color_range", "tv")];

/// BT.2020 PQ limited range with left-sited chroma, as the HDR shader writes it. NVENC doesn't put
/// the chroma location in the stream (HEVC then assumes left); MKV records it.
const HDR_COLOR: &[(&str, &str)] = &[
    ("colorspace", "bt2020nc"),
    ("color_primaries", "bt2020"),
    ("color_trc", "smpte2084"),
    ("color_range", "tv"),
    ("chroma_sample_location", "left"),
];

/// Up to three writable planes, each with its stride in bytes.
pub type Planes<'a> = [Option<(&'a mut [u8], usize)>; 3];

/// Our `ID3D11Device`, handed to FFmpeg so captured frames reach the encoder without leaving
/// the GPU. Built by the engine (`capture/d3d.rs`), which owns the lock.
pub struct D3d11Device {
    /// An AddRef'd `ID3D11Device*`; FFmpeg releases it when the session ends.
    pub device: *mut c_void,
    pub lock: unsafe extern "C" fn(*mut c_void),
    pub unlock: unsafe extern "C" fn(*mut c_void),
    /// Passed to `lock`/`unlock`.
    pub lock_ctx: *mut c_void,
    /// Keeps whatever `lock_ctx` points to alive; the session drops it after everything FFmpeg owns.
    pub keepalive: Box<dyn Send>,
}

/// The encoder's next input texture, from [`VideoSession::next_gpu_frame`]. It borrows the
/// session, so it can't outlive the FFmpeg frame that owns the texture.
pub struct GpuFrame<'a> {
    texture: *mut c_void,
    index: u32,
    _session: std::marker::PhantomData<&'a mut VideoSession>,
}

impl GpuFrame<'_> {
    /// An NV12 (P010 for HDR) `ID3D11Texture2D*` of the session's size, on the session's D3D11 device.
    pub fn texture(&self) -> *mut c_void {
        self.texture
    }

    /// The subresource to copy into (0 with our pool of standalone textures).
    pub fn index(&self) -> u32 {
        self.index
    }
}

/// `AVD3D11VADeviceContext` and `AVD3D11VAFramesContext` from FFmpeg 8.1's
/// `libavutil/hwcontext_d3d11va.h` (bindgen skips that header).
#[repr(C)]
struct D3d11vaDeviceContext {
    device: *mut c_void,
    device_context: *mut c_void,
    video_device: *mut c_void,
    video_context: *mut c_void,
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    lock_ctx: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
}

#[repr(C)]
struct D3d11vaFramesContext {
    texture: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
    texture_infos: *mut c_void,
}

const D3D11_BIND_RENDER_TARGET: u32 = 0x20;

/// `AVQSVFramesContext` from FFmpeg 8.1's `libavutil/hwcontext_qsv.h` (not bound by rusty_ffmpeg).
#[repr(C)]
struct QsvFramesContext {
    surfaces: *mut c_void,
    nb_surfaces: i32,
    frame_type: i32,
    info: *mut c_void,
}

/// `MFX_MEMTYPE_VIDEO_MEMORY_DECODER_TARGET`: one decoder-bound texture array, which we copy into.
/// `PROCESSOR_TARGET` asks for a render-target NV12 array, which D3D11 refuses (E_INVALIDARG on Iris Xe).
const MFX_MEMTYPE_VIDEO_MEMORY_DECODER_TARGET: i32 = 0x10;

/// Quick Sync's input pool: async_depth (4) frames in flight, B-frame reordering, and the one being
/// filled. A fixed pool, so [`VideoSession::next_gpu_frame`] can look its textures up once each.
const QSV_POOL: i32 = 16;

pub struct VideoSession {
    ctx: AVCodecContext,
    frame: AVFrame,
    packet: AVPacket,
    /// Set when the session takes D3D11 (or Quick Sync) frames instead of system memory.
    gpu_frames: Option<AVHWFramesContext>,
    /// Quick Sync: each pool surface's (`mfxFrameSurface1*`, texture, array index), found once by
    /// `av_hwframe_map`, so later frames need no mapping.
    qsv_textures: Vec<(usize, usize, u32)>,
    /// The next frame sent is encoded as a keyframe ([`Self::request_keyframe`]).
    force_key: bool,
    /// An NVENC session ([`Self::can_rest`]).
    rests: bool,
    /// The last decode time handed out, in encoder ticks ([`rising_dts`]); `i64::MIN` before any.
    last_dts: i64,
    /// Last, so it drops after everything above that may still call the lock.
    _keepalive: Option<Box<dyn Send>>,
}

impl VideoSession {
    /// A session fed from system memory (CPU encoders, and the encoder probe).
    ///
    /// `global_header` puts codec headers in the container instead of every keyframe; MP4,
    /// MOV and MKV want it, MPEG-TS and FLV don't (see [`crate::mux::needs_global_header`]).
    pub fn open(
        encoder: &VideoEncoder,
        values: &EncoderOptions,
        width: u32,
        height: u32,
        fps: u32,
        global_header: bool,
    ) -> Result<Self> {
        Self::open_with(encoder, values, (width, height), fps, global_header, None, Mode::Sdr)
    }

    /// Like [`Self::open`], but fails as FFmpeg does instead of retrying without B-frames: the
    /// encoder check needs to see what the card refuses.
    pub(crate) fn open_exact(
        encoder: &VideoEncoder,
        values: &EncoderOptions,
        (width, height): (u32, u32),
        fps: u32,
    ) -> Result<Self> {
        Self::open_with(encoder, values, (width, height), fps, false, None, Mode::Exact)
    }

    /// A session fed with D3D11 textures on `device` (zero copy; see [`Self::takes_d3d11`]).
    pub fn open_d3d11(
        encoder: &VideoEncoder,
        values: &EncoderOptions,
        size: (u32, u32),
        fps: u32,
        global_header: bool,
        device: D3d11Device,
    ) -> Result<Self> {
        Self::open_with(encoder, values, size, fps, global_header, Some(device), Mode::Sdr)
    }

    /// HEVC Main 10 in limited-range BT.2020 PQ, fed with P010 textures on `device`, or with
    /// system-memory P010 frames without one.
    pub fn open_hdr(
        encoder: &VideoEncoder,
        values: &EncoderOptions,
        size: (u32, u32),
        fps: u32,
        global_header: bool,
        device: Option<D3d11Device>,
    ) -> Result<Self> {
        if !encoder.hdr {
            return Err("HDR video requires NVIDIA NVENC HEVC".into());
        }
        Self::open_with(encoder, values, size, fps, global_header, device, Mode::Hdr)
    }

    /// NVENC and AMF take D3D11 frames directly; Quick Sync takes QSV frames whose textures are
    /// on our device (a QSV device derived from it), so it is zero copy too.
    pub fn takes_d3d11(encoder: &VideoEncoder) -> bool {
        matches!(encoder.family.as_str(), "nvenc" | "amf" | "qsv")
    }

    fn open_with(
        encoder: &VideoEncoder,
        values: &EncoderOptions,
        (width, height): (u32, u32),
        fps: u32,
        global_header: bool,
        device: Option<D3d11Device>,
        mode: Mode,
    ) -> Result<Self> {
        let hdr = mode == Mode::Hdr;
        let (w, h) = (to_i32(width)?, to_i32(height)?);
        // First, so FFmpeg owns the device reference whatever fails next.
        let texture_format = if hdr { ffi::AV_PIX_FMT_P010LE } else { ffi::AV_PIX_FMT_NV12 };
        let qsv = encoder.family == "qsv";
        let gpu_frames = device.as_ref().map(|d| d3d11_frames(d, w, h, texture_format, qsv)).transpose()?;
        let hw_format = if qsv { ffi::AV_PIX_FMT_QSV } else { ffi::AV_PIX_FMT_D3D11 };
        let keepalive = device.map(|d| d.keepalive);
        let codec = find_encoder(&encoder.ffmpeg)?;
        // A context that failed to open is dropped and rebuilt for the retry below.
        let build = |opts: FfmpegOptions| -> Result<(AVCodecContext, i32)> {
            let mut ctx = AVCodecContext::new(&codec);
            // Hardware encoders take NV12, which the GPU converter produces; CPU encoders that
            // can't (x265, SVT-AV1, aom) take yuv420p.
            let supported = ctx.get_supported_pix_fmts(None).unwrap_or_default();
            let formats: &[_] =
                if hdr { &[ffi::AV_PIX_FMT_P010LE] } else { &[ffi::AV_PIX_FMT_NV12, ffi::AV_PIX_FMT_YUV420P] };
            let sw_fmt = formats
                .iter()
                .copied()
                .find(|f| supported.is_empty() || supported.contains(f))
                .ok_or_else(|| format!("{} does not accept the required input pixel format", encoder.ffmpeg))?;
            ctx.set_width(w);
            ctx.set_height(h);
            ctx.set_time_base(ra(1, to_i32(fps)?));
            ctx.set_framerate(ra(to_i32(fps)?, 1));
            match &gpu_frames {
                Some(frames) => {
                    ctx.set_pix_fmt(hw_format);
                    ctx.set_hw_frames_ctx(frames.clone());
                }
                None => ctx.set_pix_fmt(sw_fmt),
            }
            // A requested keyframe must be an IDR: NVENC's default makes a plain intra frame that
            // later frames may reference across and that isn't flagged as a keyframe (platform-notes.md).
            // NVENC, x264 and x265 call it `forced-idr`, AMF and Quick Sync `forced_idr`; others have none.
            for name in ["forced-idr", "forced_idr"] {
                set_if_known(&mut ctx, name, "1");
            }
            open(&mut ctx, opts, global_header)?;
            Ok((ctx, sw_fmt))
        };
        let options = |values: &EncoderOptions| {
            let mut opts = video_options(encoder, values, fps);
            if hdr {
                opts.push(("profile".into(), "main10".into()));
            }
            let color = if hdr { HDR_COLOR } else { SDR_COLOR };
            opts.extend(color.iter().map(|(k, v)| (k.to_string(), v.to_string())));
            opts
        };
        let (ctx, sw_fmt) = match build(options(values)) {
            // Safety net for a session started before the encoder check learned this card's limit.
            Err(first) if mode != Mode::Exact => match without_bframes(encoder, values) {
                Some(no_bframes) => build(options(&no_bframes)).map_err(|_| first).inspect(|_| {
                    tracing::warn!("{} refused B-frames; using none from now on", encoder.id);
                    limit_bframes(&encoder.id, 0);
                })?,
                None => return Err(first),
            },
            opened => opened?,
        };

        let mut frame = AVFrame::new();
        if gpu_frames.is_none() {
            frame.set_format(sw_fmt);
            frame.set_width(w);
            frame.set_height(h);
            frame.alloc_buffer().map_err(|e| e.to_string())?;
        }
        let rests = encoder.family == "nvenc";
        Ok(VideoSession {
            ctx,
            frame,
            packet: AVPacket::new(),
            gpu_frames,
            qsv_textures: Vec::with_capacity(if qsv { QSV_POOL as usize } else { 0 }),
            force_key: false,
            rests,
            last_dts: i64::MIN,
            _keepalive: keepalive,
        })
    }

    /// The next input frame: copy the converted NV12 texture into it, then call [`Self::send`].
    /// D3D11 sessions only.
    // shortcut: av_hwframe_get_buffer makes two small buffer-reference allocations per frame inside
    // FFmpeg (the CPU path's send_frame does too); this API has no way around it.
    pub fn next_gpu_frame(&mut self) -> Result<GpuFrame<'_>> {
        let frames = self.gpu_frames.as_mut().ok_or("this session takes system-memory frames")?;
        // SAFETY: drops our reference to the previous frame (the encoder keeps its own).
        unsafe { ffi::av_frame_unref(self.frame.as_mut_ptr()) };
        frames.get_buffer(&mut self.frame).map_err(|e| format!("no free GPU frame: {e}"))?;
        if self.frame.format != ffi::AV_PIX_FMT_QSV {
            // D3D11 frames: data[0] is the ID3D11Texture2D, data[1] the array index (hwcontext_d3d11va.h).
            let (texture, index) = (self.frame.data[0].cast(), self.frame.data[1] as usize as u32);
            return Ok(GpuFrame { texture, index, _session: std::marker::PhantomData });
        }
        // QSV frames: data[3] is the pool surface; mapping it to D3D11 gives its texture and index.
        let surface = self.frame.data[3] as usize;
        let (texture, index) = match self.qsv_textures.iter().find(|t| t.0 == surface) {
            Some(&(_, texture, index)) => (texture, index),
            None => {
                let mut mapped = AVFrame::new();
                mapped.set_format(ffi::AV_PIX_FMT_D3D11);
                // SAFETY: both frames are live; the mapped frame holds a reference to the pool, and
                // the texture outlives it (the pool owns it until the session ends).
                let ret = unsafe { ffi::av_hwframe_map(mapped.as_mut_ptr(), self.frame.as_ptr(), 0) };
                if ret < 0 {
                    return Err(format!("can't reach Quick Sync's GPU frame ({ret})"));
                }
                let found = (mapped.data[0] as usize, mapped.data[1] as usize as u32);
                self.qsv_textures.push((surface, found.0, found.1));
                found
            }
        };
        Ok(GpuFrame { texture: texture as *mut c_void, index, _session: std::marker::PhantomData })
    }

    /// `NV12` or `yuv420p`: which planes [`Self::planes_mut`] returns.
    pub fn is_nv12(&self) -> bool {
        self.frame.format == ffi::AV_PIX_FMT_NV12
    }

    /// The next input frame's planes with their strides in bytes: Y and interleaved UV for
    /// NV12 or P010 (the third is `None`), or Y, U and V for yuv420p.
    pub fn planes_mut(&mut self) -> Result<Planes<'_>> {
        if self.gpu_frames.is_some() {
            return Err("this session takes GPU frames".into());
        }
        // Only copies if the encoder still holds a reference to the previous frame.
        self.frame.make_writable().map_err(|e| e.to_string())?;
        let chroma_h = (self.frame.height as usize).div_ceil(2);
        let heights = [self.frame.height as usize, chroma_h, chroma_h];
        let count = if self.is_nv12() || self.frame.format == ffi::AV_PIX_FMT_P010LE { 2 } else { 3 };
        let (data, lines) = (self.frame.data, self.frame.linesize);
        Ok(std::array::from_fn(|i| {
            (i < count).then(|| {
                let stride = lines[i] as usize;
                // SAFETY: av_frame_get_buffer allocated each plane with `linesize[i]` bytes per row
                // for the plane's height; planes don't overlap, and `&mut self` keeps the frame
                // alive and unaliased while the slices live.
                (unsafe { std::slice::from_raw_parts_mut(data[i], stride * heights[i]) }, stride)
            })
        }))
    }

    /// The next input frame itself, for writers inside this crate (swscale in [`crate::media`]).
    pub(crate) fn frame_mut(&mut self) -> Result<&mut AVFrame> {
        if self.gpu_frames.is_some() {
            return Err("this session takes GPU frames".into());
        }
        self.frame.make_writable().map_err(|e| e.to_string())?;
        Ok(&mut self.frame)
    }

    /// Makes the next frame sent a keyframe (an IDR where the encoder has `forced-idr`). It comes
    /// out after the encoder's usual delay (look-ahead, B-frames), with the pts it was sent at.
    /// An encoder that ignores the request still makes its regular keyframes.
    pub fn request_keyframe(&mut self) {
        self.force_key = true;
    }

    /// The encoder's regular keyframe interval in microseconds (2 s if it has none).
    pub fn keyframe_interval_us(&self) -> i64 {
        match self.ctx.gop_size {
            g if g > 1 => to_us(g.into(), self.ctx.time_base),
            _ => 2_000_000,
        }
    }

    /// Encodes the frame written through [`Self::planes_mut`] at `pts` (in frames, from the pacer).
    pub fn send(&mut self, pts: i64, sink: &mut dyn FnMut(Encoded)) -> Result<()> {
        self.frame.set_pts(pts);
        let key = std::mem::take(&mut self.force_key);
        self.frame.set_pict_type(if key { ffi::AV_PICTURE_TYPE_I } else { ffi::AV_PICTURE_TYPE_NONE });
        self.ctx.send_frame(Some(&self.frame)).map_err(|e| e.to_string())?;
        self.receive(sink)
    }

    /// Flushes the frames the encoder still holds (look-ahead, B-frames).
    pub fn finish(&mut self, sink: &mut dyn FnMut(Encoded)) -> Result<()> {
        self.ctx.send_frame(None).map_err(|e| e.to_string())?;
        self.receive(sink)
    }

    /// Whether [`Self::rest`] works with this encoder: NVENC, with or without B-frames.
    // shortcut: FFmpeg's `AV_CODEC_CAP_ENCODER_FLUSH` doesn't promise it (x264 has the flag and then
    // refuses frames); add AMF or Quick Sync once the still-screen test passes on such a card.
    pub fn can_rest(&self) -> bool {
        self.rests
    }

    /// Gets every frame the encoder still holds out now, and leaves it ready for more: for when
    /// no frames will be sent for a while (a still screen), so none of them come out late.
    /// Only where [`Self::can_rest`].
    pub fn rest(&mut self, sink: &mut dyn FnMut(Encoded)) -> Result<()> {
        self.finish(sink)?;
        // SAFETY: the context is open and NVENC supports flushing (`can_rest`); this ends the
        // draining state `finish` put it in.
        unsafe { ffi::avcodec_flush_buffers(self.ctx.as_mut_ptr()) };
        Ok(())
    }

    fn receive(&mut self, sink: &mut dyn FnMut(Encoded)) -> Result<()> {
        let last_dts = self.rests.then_some(&mut self.last_dts);
        drain(&mut self.ctx, &mut self.packet, 1, last_dts, sink)
    }

    pub fn params(&self) -> StreamParams {
        StreamParams { codecpar: self.ctx.extract_codecpar(), time_base: self.ctx.time_base }
    }
}

pub struct AudioSession {
    ctx: AVCodecContext,
    frame: AVFrame,
    packet: AVPacket,
    channels: usize,
    /// Samples per channel already written into `frame`.
    filled: usize,
    next_pts: i64,
}

impl AudioSession {
    pub fn open(
        encoder: &AudioEncoder,
        values: &EncoderOptions,
        sample_rate: u32,
        channels: u8,
        global_header: bool,
    ) -> Result<Self> {
        let codec = find_encoder(&encoder.ffmpeg)?;
        let mut ctx = AVCodecContext::new(&codec);
        let supported = ctx.get_supported_sample_fmts(None).unwrap_or_default();
        let sample_fmt = SAMPLE_FORMATS
            .into_iter()
            .find(|f| supported.is_empty() || supported.contains(f))
            .ok_or_else(|| format!("{} takes no sample format Backsight can produce", encoder.ffmpeg))?;
        let rate = to_i32(sample_rate)?;
        ctx.set_sample_fmt(sample_fmt);
        ctx.set_sample_rate(rate);
        ctx.set_time_base(ra(1, rate));
        let layout = AVChannelLayout::from_nb_channels(channels.into());
        ctx.set_ch_layout(layout.clone().into_inner());
        open(&mut ctx, audio_options(encoder, values), global_header)?;

        // Encoders without a fixed frame size (FLAC, PCM) get ~21 ms frames like AAC.
        let frame_size = if ctx.frame_size > 0 { ctx.frame_size } else { 1024 };
        let mut frame = AVFrame::new();
        frame.set_format(sample_fmt);
        frame.set_nb_samples(frame_size);
        frame.set_sample_rate(rate);
        frame.set_ch_layout(layout.into_inner());
        frame.alloc_buffer().map_err(|e| e.to_string())?;
        Ok(AudioSession { ctx, frame, packet: AVPacket::new(), channels: channels.into(), filled: 0, next_pts: 0 })
    }

    /// Number of samples per channel submitted together to the codec.
    pub fn frame_size(&self) -> usize {
        self.frame.nb_samples as usize
    }

    /// Interleaved float samples (WASAPI's format), any length. Encodes each full frame.
    pub fn push(&mut self, mut interleaved: &[f32], sink: &mut dyn FnMut(Encoded)) -> Result<()> {
        let frame_size = self.frame_size();
        while !interleaved.is_empty() {
            if self.filled == 0 {
                self.frame.make_writable().map_err(|e| e.to_string())?;
            }
            let take = (frame_size - self.filled).min(interleaved.len() / self.channels);
            if take == 0 {
                return Err("audio input must hold whole sample frames".into());
            }
            let (now, rest) = interleaved.split_at(take * self.channels);
            self.write_samples(now);
            interleaved = rest;
            if self.filled == frame_size {
                self.send_frame(sink)?;
            }
        }
        Ok(())
    }

    /// Pads the last frame with silence, then flushes the encoder.
    pub fn finish(&mut self, sink: &mut dyn FnMut(Encoded)) -> Result<()> {
        if self.filled > 0 {
            let missing = (self.frame_size() - self.filled) * self.channels;
            self.push(&vec![0.0; missing], sink)?;
        }
        self.ctx.send_frame(None).map_err(|e| e.to_string())?;
        drain(&mut self.ctx, &mut self.packet, 0, None, sink)
    }

    fn send_frame(&mut self, sink: &mut dyn FnMut(Encoded)) -> Result<()> {
        self.frame.set_pts(self.next_pts);
        self.next_pts += i64::from(self.frame.nb_samples);
        self.filled = 0;
        self.ctx.send_frame(Some(&self.frame)).map_err(|e| e.to_string())?;
        drain(&mut self.ctx, &mut self.packet, 0, None, sink)
    }

    fn write_samples(&mut self, interleaved: &[f32]) {
        let fmt = self.frame.format;
        // SAFETY: pure lookups on a valid sample format.
        let (planar, bytes) = unsafe { (ffi::av_sample_fmt_is_planar(fmt) != 0, ffi::av_get_bytes_per_sample(fmt)) };
        let bytes = bytes as usize;
        for (i, sample_frame) in interleaved.chunks_exact(self.channels).enumerate() {
            let pos = self.filled + i;
            for (c, v) in sample_frame.iter().enumerate() {
                let (plane, index) = if planar { (c, pos) } else { (0, pos * self.channels + c) };
                // SAFETY: the frame was allocated for `nb_samples` samples of `channels` channels in
                // `fmt`, `pos < nb_samples`, and it was made writable before the first write.
                let dst = unsafe { self.frame.data[plane].add(index * bytes) };
                // SAFETY: `dst` points at `bytes` writable bytes inside that buffer (unaligned writes).
                unsafe { write_sample(dst, fmt, *v) };
            }
        }
        self.filled += interleaved.len() / self.channels;
    }

    pub fn params(&self) -> StreamParams {
        StreamParams { codecpar: self.ctx.extract_codecpar(), time_base: self.ctx.time_base }
    }
}

/// Sample formats in order of preference: float first, since WASAPI gives floats.
const SAMPLE_FORMATS: [ffi::AVSampleFormat; 6] = [
    ffi::AV_SAMPLE_FMT_FLTP,
    ffi::AV_SAMPLE_FMT_FLT,
    ffi::AV_SAMPLE_FMT_S16,
    ffi::AV_SAMPLE_FMT_S16P,
    ffi::AV_SAMPLE_FMT_S32,
    ffi::AV_SAMPLE_FMT_S32P,
];

/// # Safety
/// `dst` must point at `av_get_bytes_per_sample(fmt)` writable bytes.
unsafe fn write_sample(dst: *mut u8, fmt: ffi::AVSampleFormat, v: f32) {
    let v = v.clamp(-1.0, 1.0);
    // SAFETY: the caller guarantees room for one sample of `fmt`; writes are unaligned-safe.
    unsafe {
        match fmt {
            ffi::AV_SAMPLE_FMT_S16 | ffi::AV_SAMPLE_FMT_S16P => dst.cast::<i16>().write_unaligned((v * 32767.0) as i16),
            ffi::AV_SAMPLE_FMT_S32 | ffi::AV_SAMPLE_FMT_S32P => {
                dst.cast::<i32>().write_unaligned((f64::from(v) * 2_147_483_647.0) as i32)
            }
            _ => dst.cast::<f32>().write_unaligned(v),
        }
    }
}

fn open(ctx: &mut AVCodecContext, mut opts: FfmpegOptions, global_header: bool) -> Result<()> {
    if global_header {
        opts.push(("flags".into(), "+global_header".into()));
    }
    open_with_options(ctx, &opts)
}

/// A pool of NV12 (or P010 for HDR) D3D11 textures on `device` for the encoder, or with `qsv` a
/// pool of QSV frames on a Quick Sync device derived from `device` (textures on our device too).
fn d3d11_frames(
    device: &D3d11Device,
    w: i32,
    h: i32,
    format: ffi::AVPixelFormat,
    qsv: bool,
) -> Result<AVHWFramesContext> {
    let mut device_ctx = AVHWDeviceContext::alloc(ffi::AV_HWDEVICE_TYPE_D3D11VA);
    // SAFETY: the buffer of a D3D11VA device context is an AVHWDeviceContext whose `hwctx` is an
    // AVD3D11VADeviceContext (layout above). Setting `device` hands FFmpeg our reference.
    unsafe {
        let hw = (*(device_ctx.data as *mut ffi::AVHWDeviceContext)).hwctx as *mut D3d11vaDeviceContext;
        (*hw).device = device.device;
        (*hw).lock = Some(device.lock);
        (*hw).unlock = Some(device.unlock);
        (*hw).lock_ctx = device.lock_ctx;
    }
    device_ctx.init().map_err(|e| format!("FFmpeg can't use the D3D11 device: {e}"))?;
    if qsv {
        // The device must be on the Intel adapter (Gpu::open picks the encoder's vendor).
        let qsv_device = device_ctx
            .create_derived(ffi::AV_HWDEVICE_TYPE_QSV)
            .map_err(|e| format!("Quick Sync can't use the D3D11 device: {e}"))?;
        let mut frames = qsv_device.hwframe_ctx_alloc();
        let fc = frames.data();
        fc.format = ffi::AV_PIX_FMT_QSV;
        fc.sw_format = format;
        fc.width = w;
        fc.height = h;
        fc.initial_pool_size = QSV_POOL;
        // SAFETY: a QSV frames context's `hwctx` is an AVQSVFramesContext (layout above).
        unsafe { (*(fc.hwctx as *mut QsvFramesContext)).frame_type = MFX_MEMTYPE_VIDEO_MEMORY_DECODER_TARGET };
        frames.init().map_err(|e| format!("can't create Quick Sync frames for the encoder: {e}"))?;
        return Ok(frames);
    }
    let mut frames = device_ctx.hwframe_ctx_alloc();
    let fc = frames.data();
    fc.format = ffi::AV_PIX_FMT_D3D11;
    fc.sw_format = format;
    fc.width = w;
    fc.height = h;
    // 0: one standalone texture per frame, allocated as the encoder needs them. A preallocated
    // pool is one texture array, and NVIDIA's driver rejects NV12 arrays of more than 2 slices
    // unless they are decoder-bound (platform-notes.md).
    fc.initial_pool_size = 0;
    // SAFETY: a D3D11 frames context's `hwctx` is an AVD3D11VAFramesContext (layout above).
    // Render-target binding lets the video processor write into NV12 textures later. P010 frames
    // are only copied into, so they ask for nothing a GPU might not support.
    let bind = if format == ffi::AV_PIX_FMT_NV12 { D3D11_BIND_RENDER_TARGET } else { 0 };
    unsafe { (*(fc.hwctx as *mut D3d11vaFramesContext)).bind_flags = bind };
    frames.init().map_err(|e| format!("can't create GPU frames for the encoder: {e}"))?;
    Ok(frames)
}

/// Hands every packet the encoder has ready to `sink`, reusing one packet. With `last_dts`, decode
/// times are kept rising across rests ([`rising_dts`]).
fn drain(
    ctx: &mut AVCodecContext,
    packet: &mut AVPacket,
    default_duration: i64,
    mut last_dts: Option<&mut i64>,
    sink: &mut dyn FnMut(Encoded),
) -> Result<()> {
    let tb = ctx.time_base;
    loop {
        // SAFETY: both pointers come from live rsmpeg wrappers borrowed mutably for this call;
        // the packet is unreferenced (empty) on entry.
        let ret = unsafe { ffi::avcodec_receive_packet(ctx.as_mut_ptr(), packet.as_mut_ptr()) };
        if ret == ffi::AVERROR(ffi::EAGAIN) || ret == ffi::AVERROR_EOF {
            return Ok(());
        }
        if ret < 0 {
            return Err(format!("encoding failed ({ret})"));
        }
        let data = if packet.size > 0 {
            // SAFETY: a received packet owns `size` bytes at `data` until av_packet_unref below.
            unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) }
        } else {
            &[]
        };
        let mut dts = packet.dts;
        if let Some(last) = last_dts.as_deref_mut() {
            dts = rising_dts(dts, *last);
            *last = dts.max(*last);
        }
        sink(Encoded {
            data,
            pts_us: to_us(packet.pts, tb),
            dts_us: to_us(dts, tb),
            // Some video encoders omit duration; our input is constant-frame-rate.
            duration_us: to_us(if packet.duration > 0 { packet.duration } else { default_duration }, tb),
            keyframe: packet.flags & ffi::AV_PKT_FLAG_KEY as i32 != 0,
        });
        // SAFETY: releases the packet's buffer reference; the `data` slice died with the sink call.
        unsafe { ffi::av_packet_unref(packet.as_mut_ptr()) };
    }
}

/// A decode time, in encoder ticks, one tick past `last` if it would not rise.
///
/// After a flush (a rest) FFmpeg 8.1's `nvenc_set_timestamp` restarts: the first `delay` packets
/// (`delay` = B-frames) get `first_pts - D * (delay - k) / delay`, where `D` is the gap from the
/// run's first input to its `delay`-th (`delay` ticks when fewer are queued), and later packets
/// the queued input times in order. A gap early in the run (in practice only the pacer's stall jump, which skips 6+ slots) makes `D` larger than
/// `delay`, which can reach back to the last decode time before the flush. Bumping in whole ticks
/// always fits: the last decode time before a run is at least `delay` ticks before the previous
/// run's last picture, so `delay` ticks fit between it and the run's first picture, below every
/// picture of the run, and the run's later decode times (its input times) are past them.
/// Microsecond bumps don't: `last + 1 tick` in rounded microseconds can land 1 µs past the next
/// tick and drift on by a microsecond per packet until decode passes display time.
fn rising_dts(dts: i64, last: i64) -> i64 {
    // i64::MIN is AV_NOPTS_VALUE: no decode time at all.
    if dts == i64::MIN { dts } else { dts.max(last.saturating_add(1)) }
}

/// Time-base units to microseconds, rounded to nearest.
pub(crate) fn to_us(v: i64, tb: ffi::AVRational) -> i64 {
    rescale(v, i64::from(tb.num), i64::from(tb.den), 1, 1_000_000)
}

/// `v × (num1/den1) ÷ (num2/den2)`, rounded to nearest, without overflow: a result past i64 is
/// capped, and a zero divisor (a damaged file's time base) gives 0.
pub(crate) fn rescale(v: i64, num1: i64, den1: i64, num2: i64, den2: i64) -> i64 {
    let (n, d) = (i128::from(v) * i128::from(num1) * i128::from(den2), i128::from(den1) * i128::from(num2));
    if d == 0 {
        return 0;
    }
    ((n + n.signum() * d / 2) / d).clamp(i64::MIN.into(), i64::MAX.into()) as i64
}

fn to_i32(v: u32) -> Result<i32> {
    i32::try_from(v).map_err(|_| format!("{v} is out of range"))
}

/// Defaults for an encoder, for the probe and tests.
pub(crate) fn default_values(encoder: &VideoEncoder) -> EncoderOptions {
    crate::catalog::resolve(&Catalog::get().video_options(encoder), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// x264 has FFmpeg's flush flag but refuses frames after handing over what it held
    /// ("lookahead thread is already stopped"), so only NVENC, checked on real hardware, rests.
    #[test]
    fn only_nvenc_rests() {
        crate::ffmpeg::errors_only_logging();
        let enc = Catalog::get().video_encoder("x264").unwrap();
        assert!(!VideoSession::open(enc, &default_values(enc), 256, 144, 60, true).unwrap().can_rest());
    }

    /// FFmpeg 8.1 `nvenc.c`'s `nvenc_set_timestamp` and `ff_nvenc_encode_flush`, as far as decode
    /// times go (time base 1/fps, so the frame-rate fallback is `delay` ticks).
    struct NvencDts {
        delay: usize,
        queue: std::collections::VecDeque<i64>,
        out: usize,
        delay_time: i64,
    }

    impl NvencDts {
        fn dts(&mut self) -> i64 {
            let d = self.delay;
            if self.out >= d {
                self.out += 1;
                return self.queue.pop_front().unwrap();
            }
            if self.delay_time == 0 {
                self.delay_time = self.queue.get(d).map_or(d as i64, |t1| t1 - self.queue[0]);
            }
            let dts = self.queue[0] - self.delay_time * (d - self.out) as i64 / d as i64;
            self.out += 1;
            dts
        }
    }

    /// `(pts, dts)` in encoder ticks, in decode order.
    type Packets = Vec<(i64, i64)>;

    /// Packets `(pts, dts)` NVENC with `b` B-frames gives for `runs` of input times, each run
    /// ended by a rest. Inputs are grouped into B…B P (a short last group at the end of a run; the
    /// first frame on its own when `first_alone`), P coming out before its Bs, `lag` inputs after
    /// its group is complete.
    fn nvenc(b: usize, runs: &[Vec<i64>], first_alone: bool, lag: usize) -> (Packets, Packets) {
        let mut enc = NvencDts { delay: b, queue: Default::default(), out: 0, delay_time: 0 };
        let (mut raw, mut fixed, mut last) = (vec![], vec![], i64::MIN);
        for run in runs {
            let mut groups = vec![];
            let mut start = 0;
            while start < run.len() {
                let len = if first_alone && start == 0 { 1 } else { (b + 1).min(run.len() - start) };
                groups.push(start..start + len);
                start += len;
            }
            let mut emit = |enc: &mut NvencDts, g: &std::ops::Range<usize>| {
                for i in std::iter::once(g.end - 1).chain(g.start..g.end - 1) {
                    let dts = enc.dts();
                    raw.push((run[i], dts));
                    last = rising_dts(dts, last);
                    fixed.push((run[i], last));
                }
            };
            let mut next = 0;
            for (i, &pts) in run.iter().enumerate() {
                enc.queue.push_back(pts);
                while next < groups.len() && groups[next].end - 1 + lag <= i {
                    emit(&mut enc, &groups[next]);
                    next += 1;
                }
            }
            groups[next..].iter().for_each(|g| emit(&mut enc, g));
            // ff_nvenc_encode_flush
            enc.queue.clear();
            (enc.out, enc.delay_time) = (0, 0);
        }
        (raw, fixed)
    }

    fn valid(packets: &[(i64, i64)]) -> bool {
        packets.windows(2).all(|w| w[0].1 < w[1].1) && packets.iter().all(|&(pts, dts)| dts <= pts)
    }

    /// Still-screen patterns from `still_slot`: motion encodes every slot, a still screen one
    /// slot in 4 with a rest after each, and a rest always takes a slot. Every pattern, with 1 to 4
    /// B-frames, keeps decode times rising and at or before display times.
    #[test]
    fn decode_times_keep_rising_across_rests_with_b_frames() {
        let mut seed = 7u64;
        let mut rand = |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        for b in 1..=4 {
            for (first_alone, lag) in [(false, 0), (true, 0), (false, 2), (true, 3)] {
                for _ in 0..200 {
                    let (mut runs, mut pts) = (vec![], 0i64);
                    for _ in 0..12 {
                        let kind = rand(4);
                        // Still frames 4 slots apart, motion (sometimes with a slot the pacer
                        // skipped), or a twitch no longer than the B-frame count.
                        let len = match kind {
                            0 => 1 + rand(3),
                            1 | 2 => 1 + rand(40),
                            _ => 1 + rand(b as u64 + 1),
                        };
                        let mut run = vec![];
                        for _ in 0..len {
                            run.push(pts);
                            pts += match kind {
                                0 => 4,
                                1 | 2 if rand(6) == 0 => 3,
                                _ => 1,
                            };
                        }
                        runs.push(run);
                        pts += 1 + rand(5) as i64; // the rest's slot, then a still screen
                    }
                    let (_, fixed) = nvenc(b, &runs, first_alone, lag);
                    assert!(valid(&fixed), "{b} B-frames: {runs:?}\n{fixed:?}");
                }
            }
        }
    }

    /// The case the fix is for: a skipped slot early in a run after a rest makes FFmpeg's first
    /// decode time reach back to the last one before the rest.
    #[test]
    fn a_skipped_slot_after_a_rest_needs_the_fix() {
        let runs = [(0..10).collect(), vec![11, 15, 16, 17, 18]];
        let (raw, fixed) = nvenc(2, &runs, false, 0);
        assert!(!valid(&raw), "{raw:?}");
        assert!(valid(&fixed), "{fixed:?}");
        assert_eq!(rising_dts(i64::MIN, 5), i64::MIN, "no decode time stays none");
    }

    #[test]
    fn rescale_rounds_to_nearest_both_ways() {
        assert_eq!(to_us(1, ra(1, 60)), 16_667);
        assert_eq!(to_us(-1, ra(1, 60)), -16_667);
        assert_eq!(to_us(1024, ra(1, 48_000)), 21_333);
        assert_eq!(rescale(1_000_000, 1, 1_000_000, 1, 90_000), 90_000);
        assert_eq!(rescale(3, 2, 1, 3, 1), 2, "3 × 2/1 ÷ 3/1");
    }

    #[test]
    fn the_keyframe_interval_follows_the_encoder() {
        crate::ffmpeg::errors_only_logging();
        let enc = Catalog::get().video_encoder("x264").unwrap();
        let mut values = default_values(enc);
        values.insert("keyint_sec".into(), 5.into());
        let mut s = VideoSession::open(enc, &values, 256, 144, 30, true).unwrap();
        assert_eq!(s.keyframe_interval_us(), 5_000_000);
        // SAFETY: a plain integer field of the open context, only read back below.
        unsafe { (*s.ctx.as_mut_ptr()).gop_size = 1 };
        assert_eq!(s.keyframe_interval_us(), 2_000_000, "every frame a keyframe: no interval");
    }

    #[test]
    fn video_packets_keep_their_keyframe_flag_and_duration() {
        crate::ffmpeg::errors_only_logging();
        let enc = Catalog::get().video_encoder("x264").unwrap();
        let mut s = VideoSession::open(enc, &default_values(enc), 256, 144, 30, true).unwrap();
        let mut got = vec![];
        for i in 0..10 {
            s.send(i, &mut |p| got.push((p.keyframe, p.duration_us))).unwrap();
        }
        s.finish(&mut |p| got.push((p.keyframe, p.duration_us))).unwrap();
        assert_eq!(got.len(), 10);
        assert!(got[0].0 && !got[1..].iter().any(|p| p.0), "{got:?}");
        assert!(got.iter().all(|p| p.1 == 33_333), "{got:?}");
    }

    /// The samples written to the frame waiting for the encoder, as f32 or i16 values.
    fn waiting(s: &AudioSession, plane: usize, n: usize) -> Vec<f32> {
        // SAFETY: the frame holds at least `n` samples of its format in `plane`.
        unsafe {
            match s.frame.format {
                ffi::AV_SAMPLE_FMT_S16 => {
                    (0..n).map(|i| f32::from(s.frame.data[plane].cast::<i16>().add(i).read_unaligned())).collect()
                }
                _ => (0..n).map(|i| s.frame.data[plane].cast::<f32>().add(i).read_unaligned()).collect(),
            }
        }
    }

    #[test]
    fn audio_samples_land_in_their_channel() {
        crate::ffmpeg::errors_only_logging();
        let c = Catalog::get();
        let two = [0.5, -0.5, 0.25, -0.25];
        // AAC takes planar floats: one plane per channel.
        let aac = c.audio_encoder("aac").unwrap();
        let mut s = AudioSession::open(aac, &crate::catalog::resolve(&aac.options, None), 48_000, 2, true).unwrap();
        s.push(&two, &mut |_| panic!("not a whole frame yet")).unwrap();
        assert_eq!((waiting(&s, 0, 2), waiting(&s, 1, 2)), (vec![0.5, 0.25], vec![-0.5, -0.25]));
        // 16-bit PCM takes them interleaved, in one plane.
        let pcm = c.audio_encoder("pcm-16").unwrap();
        let mut s = AudioSession::open(pcm, &crate::catalog::resolve(&pcm.options, None), 48_000, 2, true).unwrap();
        s.push(&two, &mut |_| panic!("not a whole frame yet")).unwrap();
        assert_eq!(waiting(&s, 0, 4), [16383.0, -16383.0, 8191.0, -8191.0]);
    }

    #[test]
    fn audio_packets_last_one_frame() {
        crate::ffmpeg::errors_only_logging();
        let aac = Catalog::get().audio_encoder("aac").unwrap();
        let mut s = AudioSession::open(aac, &crate::catalog::resolve(&aac.options, None), 48_000, 2, true).unwrap();
        let mut durations = vec![];
        s.push(&vec![0.25; 1024 * 2 * 4], &mut |p| durations.push(p.duration_us)).unwrap();
        s.finish(&mut |p| durations.push(p.duration_us)).unwrap();
        assert!(!durations.is_empty() && durations.iter().all(|d| *d == 21_333), "{durations:?}");
    }

    #[test]
    fn rescale_of_damaged_values_caps_instead_of_wrapping() {
        assert_eq!(rescale(i64::MAX, 1, 1, 1, 1_000_000), i64::MAX, "past i64 is capped");
        assert_eq!(rescale(i64::MIN, 1, 1, 1, 1_000_000), i64::MIN);
        assert_eq!(to_us(5, ra(1, 0)), 0, "a zero time base");
        assert_eq!(rescale(5, 1, 1, 0, 1), 0);
    }
}
