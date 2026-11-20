//! Reading saved media for the library: metadata, a thumbnail and the hover preview. Part of
//! the FFmpeg edge (`docs/architecture/library.md`).

use crate::{
    catalog::{Catalog, resolve},
    ffmpeg::open_with_options,
    mux::{Muxer, Track, needs_global_header},
    session::{Result, VideoSession, rescale, to_us},
};
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext},
    avformat::AVFormatContextInput,
    avutil::{AVChannelLayout, AVFrame, ra},
    error::RsmpegError,
    ffi,
    swresample::SwrContext,
    swscale::SwsContext,
};
use serde::{Deserialize, Serialize};
use std::{ffi::CString, fs, path::Path};

/// What the library shows about a file. `None` when the file doesn't say.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// FFmpeg's codec name, e.g. `h264` (the catalog's video codec ids).
    pub video_codec: Option<String>,
    pub audio_tracks: u32,
}

/// `AV_NOPTS_VALUE` (a macro bindgen skips).
const NOPTS: i64 = i64::MIN;

const THUMB_MAX: (i32, i32) = (480, 480);
const PREVIEW_MAX: (i32, i32) = (640, 360);
const PREVIEW_FPS: u32 = 30;
const SNIPPETS: i64 = 4;
const SNIPPET_US: i64 = 1_500_000;

pub(crate) fn open_input(path: &Path) -> Result<AVFormatContextInput> {
    let c = CString::new(path.to_str().ok_or("path is not valid Unicode")?).map_err(|e| e.to_string())?;
    // Also runs avformat_find_stream_info, which fills in sizes, frame rates and durations.
    let input = AVFormatContextInput::open(&c).map_err(|e| format!("can't open {}: {e}", path.display()))?;
    // Without streams FFmpeg's list is a null pointer, which rsmpeg's `streams()` turns into a slice
    // (undefined behaviour; a debug build stops on it). A file with no tracks has nothing to read anyway.
    if input.nb_streams == 0 {
        return Err(format!("can't open {}: it has no video or sound", path.display()));
    }
    Ok(input)
}

fn positive(v: i32) -> Option<u32> {
    u32::try_from(v).ok().filter(|v| *v > 0)
}

/// Reads the container and stream headers (no decoding beyond what FFmpeg needs to fill them in).
pub fn probe(path: &Path) -> Result<MediaInfo> {
    let input = open_input(path)?;
    let streams = input.streams();
    let mut info = MediaInfo {
        duration_ms: (input.duration > 0).then(|| input.duration as u64 / 1000),
        audio_tracks: streams.iter().filter(|s| s.codecpar().codec_type == ffi::AVMEDIA_TYPE_AUDIO).count() as u32,
        ..MediaInfo::default()
    };
    // find_best_stream also needs a decoder; the slim FFmpeg has none for rarer codecs (ProRes, DNxHD),
    // so fall back to the first video stream and still report length and size.
    // shortcut: no thumbnail for those codecs, add their decoders to ffmpeg.yml if users keep such files.
    let best = input
        .find_best_stream(ffi::AVMEDIA_TYPE_VIDEO)
        .ok()
        .flatten()
        .map(|(i, _)| i)
        .or_else(|| streams.iter().position(|s| s.codecpar().codec_type == ffi::AVMEDIA_TYPE_VIDEO));
    if let Some(i) = best {
        let s = &streams[i];
        let par = s.codecpar();
        (info.width, info.height) = (positive(par.width), positive(par.height));
        // Not avg_frame_rate: frames ÷ duration, which reads 30.5 for a 2 s clip at 30 fps.
        let r = s.guess_framerate().unwrap_or(ra(0, 1));
        info.fps = (r.num > 0 && r.den > 0).then(|| f64::from(r.num) / f64::from(r.den));
        // SAFETY: avcodec_get_name returns a static NUL-terminated string for any id.
        let name = unsafe { std::ffi::CStr::from_ptr(ffi::avcodec_get_name(par.codec_id)) };
        info.video_codec = Some(name.to_string_lossy().into_owned());
    }
    Ok(info)
}

/// The best video track of a file, decoded in software.
struct Decoder {
    input: AVFormatContextInput,
    ctx: AVCodecContext,
    stream: usize,
    tb: ffi::AVRational,
    /// The stream's first timestamp (MPEG-TS starts around 1.4 s, MP4 at 0).
    start: i64,
    duration_us: i64,
    flushed: bool,
    /// A packet was read since opening: seeking to the start is then a real seek.
    read_any: bool,
}

impl Decoder {
    fn open(path: &Path) -> Result<Self> {
        let input = open_input(path)?;
        let (stream, codec) =
            input.find_best_stream(ffi::AVMEDIA_TYPE_VIDEO).map_err(|e| e.to_string())?.ok_or("no video track")?;
        let s = &input.streams()[stream];
        let (tb, start) = (s.time_base, if s.start_time == NOPTS { 0 } else { s.start_time });
        let mut ctx = AVCodecContext::new(&codec);
        ctx.apply_codecpar(&s.codecpar()).map_err(|e| e.to_string())?;
        ctx.set_pkt_timebase(tb);
        open_with_options(&mut ctx, &vec![("threads".into(), "auto".into())])?;
        let duration_us = input.duration.max(0);
        Ok(Decoder { input, ctx, stream, tb, start, duration_us, flushed: false, read_any: false })
    }

    /// Jumps to the keyframe at or before `us` from the file's start.
    fn seek(&mut self, us: i64) -> Result<()> {
        // Already there. MPEG-TS seeks by byte position and can land past a short file's only
        // keyframe, even when seeking to 0.
        if us <= 0 && !self.read_any {
            return Ok(());
        }
        let ts = self.start.saturating_add(rescale(us, 1, 1_000_000, self.tb.num.into(), self.tb.den.into()));
        self.input
            .seek(self.stream as i32, ts, ffi::AVSEEK_FLAG_BACKWARD as i32)
            .map_err(|e| format!("can't seek: {e}"))?;
        // Frames still queued in the decoder are from before the seek.
        self.ctx.flush_buffers();
        self.flushed = false;
        Ok(())
    }

    /// The next frame and its time in microseconds from the file's start; `None` at the end.
    fn next(&mut self) -> Result<Option<(AVFrame, i64)>> {
        loop {
            match self.ctx.receive_frame() {
                Ok(f) if f.best_effort_timestamp == NOPTS => continue,
                Ok(f) => {
                    let us = to_us(f.best_effort_timestamp.saturating_sub(self.start), self.tb);
                    return Ok(Some((f, us)));
                }
                Err(RsmpegError::DecoderDrainError) => {}
                Err(RsmpegError::DecoderFlushedError) => return Ok(None),
                Err(e) => return Err(format!("decoding failed: {e}")),
            }
            if self.flushed {
                return Ok(None);
            }
            self.read_any = true;
            match self.input.read_packet().map_err(|e| format!("reading failed: {e}"))? {
                Some(p) if p.stream_index as usize == self.stream => {
                    self.ctx.send_packet(Some(&p)).map_err(|e| format!("decoding failed: {e}"))?
                }
                Some(_) => {}
                None => {
                    // Drains the frames the decoder still holds.
                    self.flushed = true;
                    self.ctx.send_packet(None).map_err(|e| format!("decoding failed: {e}"))?;
                }
            }
        }
    }
}

/// `w`×`h` scaled to fit `max`, keeping the aspect ratio, never enlarged, even sizes (4:2:0).
fn fit(w: i32, h: i32, (max_w, max_h): (i32, i32)) -> (i32, i32) {
    let scale = (f64::from(max_w) / f64::from(w)).min(f64::from(max_h) / f64::from(h)).min(1.0);
    let even = |v: i32| ((f64::from(v) * scale / 2.0).round() as i32 * 2).max(2);
    (even(w), even(h))
}

fn scaler(src: &AVFrame, dst: &AVFrame) -> Result<SwsContext> {
    SwsContext::get_context(
        src.width,
        src.height,
        src.format,
        dst.width,
        dst.height,
        dst.format,
        ffi::SWS_BILINEAR,
        None,
        None,
        None,
    )
    .ok_or_else(|| "can't scale this video's frames".into())
}

/// Writes `bytes` to `<path>.tmp`, then renames, so a crash never leaves half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

/// A JPEG of the keyframe at about 10% in, at most 480 px on each side.
pub fn thumbnail(src: &Path, dst: &Path) -> Result<()> {
    let mut d = Decoder::open(src)?;
    d.seek(d.duration_us / 10)?;
    // The first frame after the seek is the keyframe: no need to decode up to the exact time.
    let frame = match d.next()? {
        Some((f, _)) => f,
        None => {
            // The seek landed past a short file's only keyframe (see `Decoder::seek`): read it from the start.
            Decoder::open(src)?.next()?.ok_or("the video has no frames")?.0
        }
    };
    let (w, h) = fit(frame.width, frame.height, THUMB_MAX);

    let codec = AVCodec::find_encoder(ffi::AV_CODEC_ID_MJPEG).ok_or("FFmpeg has no JPEG encoder")?;
    let mut enc = AVCodecContext::new(&codec);
    enc.set_width(w);
    enc.set_height(h);
    // Full-range 4:2:0, what JPEG means; mjpeg rejects limited-range yuv420p by default.
    enc.set_pix_fmt(ffi::AV_PIX_FMT_YUVJ420P);
    enc.set_time_base(ra(1, 1));
    open_with_options(&mut enc, &vec![("flags".into(), "+qscale".into())])?;

    let mut out = AVFrame::new();
    out.set_format(ffi::AV_PIX_FMT_YUVJ420P);
    out.set_width(w);
    out.set_height(h);
    out.alloc_buffer().map_err(|e| e.to_string())?;
    // With +qscale the frame carries the quality (qscale 2 is best, 31 worst), as `ffmpeg -q:v` does.
    // SAFETY: `out` is our own frame; quality is a plain integer field.
    unsafe { (*out.as_mut_ptr()).quality = 5 * ffi::FF_QP2LAMBDA as i32 };
    if frame.color_trc == ffi::AVCOL_TRC_SMPTE2084 {
        let rgb = hdr_to_sdr_rgb(&frame, w, h)?;
        scaler(&rgb, &out)?.scale_frame(&rgb, 0, h, &mut out).map_err(|e| e.to_string())?;
    } else {
        scaler(&frame, &out)?.scale_frame(&frame, 0, frame.height, &mut out).map_err(|e| e.to_string())?;
    }
    enc.send_frame(Some(&out)).map_err(|e| e.to_string())?;
    enc.send_frame(None).map_err(|e| e.to_string())?;
    let pkt = enc.receive_packet().map_err(|e| format!("JPEG encoding failed: {e}"))?;
    // SAFETY: a received packet owns `size` bytes at `data` while `pkt` lives.
    let jpeg = unsafe { std::slice::from_raw_parts(pkt.data, pkt.size as usize) };
    write_atomic(dst, jpeg)
}

/// An HDR10 (PQ, BT.2020) frame scaled to `w`×`h` and tone-mapped to SDR RGB48, so its thumbnail
/// isn't grey and washed out the way PQ read as plain video looks.
// shortcut: thumbnails only, HLG and hover previews still read as plain video; reuse this per
// frame in `preview` if HDR previews look wrong to users.
fn hdr_to_sdr_rgb(frame: &AVFrame, w: i32, h: i32) -> Result<AVFrame> {
    let mut rgb = AVFrame::new();
    rgb.set_format(ffi::AV_PIX_FMT_RGB48LE);
    rgb.set_width(w);
    rgb.set_height(h);
    rgb.alloc_buffer().map_err(|e| e.to_string())?;
    let mut sws = scaler(frame, &rgb)?;
    let full_range = i32::from(frame.color_range == ffi::AVCOL_RANGE_JPEG);
    // SAFETY: `sws` is a live context; sws_getCoefficients returns a static table for any id.
    let ok = unsafe {
        let bt2020 = ffi::sws_getCoefficients(ffi::SWS_CS_BT2020 as i32);
        ffi::sws_setColorspaceDetails(sws.as_mut_ptr(), bt2020, full_range, bt2020, 1, 0, 1 << 16, 1 << 16)
    };
    if ok < 0 {
        return Err("can't read this HDR video's colours".into());
    }
    sws.scale_frame(frame, 0, frame.height, &mut rgb).map_err(|e| e.to_string())?;
    for y in 0..h as usize {
        // SAFETY: `rgb` owns `h` rows of `linesize[0]` bytes at `data[0]`, 2-byte aligned
        // (av_frame_get_buffer); each row's first `w` RGB48 pixels were just written by swscale.
        let row = unsafe {
            std::slice::from_raw_parts_mut(rgb.data[0].add(y * rgb.linesize[0] as usize).cast::<u16>(), w as usize * 3)
        };
        for px in row.as_chunks_mut::<3>().0 {
            *px = pq_to_sdr(px.map(|v| f32::from(v) / 65535.0)).map(|v| (v * 65535.0).round() as u16);
        }
    }
    Ok(rgb)
}

/// One PQ-coded BT.2020 RGB pixel (0..1) as sRGB (0..1). 203 nits, HDR's "paper white", becomes
/// near-white; brighter highlights roll off softly instead of clipping.
fn pq_to_sdr(pq: [f32; 3]) -> [f32; 3] {
    const SDR_WHITE_NITS: f32 = 203.0;
    let nits = |e: f32| {
        let (m1, m2, c1, c2, c3) = (0.159_301_76, 78.843_75, 0.835_937_5, 18.851_563, 18.687_5);
        let p = e.clamp(0.0, 1.0).powf(1.0 / m2);
        ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1) * 10_000.0
    };
    let [r, g, b] = pq.map(|e| nits(e) / SDR_WHITE_NITS);
    // BT.2020 to BT.709 primaries, in linear light; out-of-gamut colours clip at 0.
    let lin = [
        1.660_5 * r - 0.587_6 * g - 0.072_9 * b,
        -0.124_6 * r + 1.132_9 * g - 0.008_3 * b,
        -0.018_2 * r - 0.100_6 * g + 1.118_8 * b,
    ]
    .map(|v| v.max(0.0));
    // Linear up to the knee, then a soft roll-off towards 1 on the brightest channel (keeps hue).
    const KNEE: f32 = 0.75;
    let peak = lin.iter().copied().fold(0.0, f32::max);
    let scale = if peak > KNEE {
        let t = (peak - KNEE) / (1.0 - KNEE);
        (KNEE + (1.0 - KNEE) * t / (1.0 + t)) / peak
    } else {
        1.0
    };
    lin.map(|v| {
        let v = (v * scale).min(1.0);
        if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    })
}

/// Where each preview snippet starts and ends, in microseconds: `SNIPPETS` of `SNIPPET_US`
/// spread across the video, or its start when it's too short for that. An unknown length (0)
/// also gets only the start: never transcode a whole file of unknown length.
fn snippets(duration_us: i64) -> Vec<(i64, i64)> {
    if duration_us <= SNIPPETS * SNIPPET_US {
        return vec![(0, SNIPPETS * SNIPPET_US)];
    }
    // Divided first: a damaged file can claim a length near i64::MAX.
    (1..=SNIPPETS).map(|k| duration_us / (SNIPPETS + 1) * k - SNIPPET_US / 2).map(|s| (s, s + SNIPPET_US)).collect()
}

/// Encoder and file of a preview, opened at its first frame (which sets the size).
struct Output {
    enc: VideoSession,
    mux: Muxer,
    sws: SwsContext,
    src: (i32, i32, i32),
}

/// A short muted H.264 MP4 (360p, 30 fps) of [`SNIPPETS`] snippets from across `src`, for the
/// library's hover preview. Written to `<dst>.part` and renamed, like clips.
pub fn preview(src: &Path, dst: &Path) -> Result<()> {
    let c = Catalog::get();
    let container = c.container("mp4").ok_or("the catalog has no mp4 container")?;
    let venc = c.video_encoder("x264").ok_or("the catalog has no x264 encoder")?;
    let mut values = resolve(&c.video_options(venc), None);
    values.insert("cq".into(), toml::Value::Integer(30)); // small file; it plays at thumbnail size

    let mut d = Decoder::open(src)?;
    let step = 1_000_000 / i64::from(PREVIEW_FPS);
    let (mut out, mut pts, mut failed): (Option<Output>, i64, Option<String>) = (None, 0, None);
    for (start, end) in snippets(d.duration_us) {
        d.seek(start)?;
        let mut next = start;
        while let Some((frame, t)) = d.next()? {
            if t >= end {
                break;
            }
            // One frame per output slot, with a quarter slot of slack for timestamp jitter.
            if t < next - step / 4 {
                continue;
            }
            next += step;
            if t >= next - step / 4 {
                next = t + step; // after a gap, restart the slots here
            }
            if out.is_none() {
                let (w, h) = fit(frame.width, frame.height, PREVIEW_MAX);
                let mut enc =
                    VideoSession::open(venc, &values, w as u32, h as u32, PREVIEW_FPS, needs_global_header(container))?;
                let mux = Muxer::create(dst, container, &enc.params(), &[])?;
                let sws = scaler(&frame, enc.frame_mut()?)?;
                out = Some(Output { enc, mux, sws, src: (0, 0, -1) });
            }
            let o = out.as_mut().expect("opened above");
            let target = o.enc.frame_mut()?;
            if o.src != (frame.width, frame.height, frame.format) {
                // A size change mid-file would make swscale read past the smaller frame.
                o.sws = scaler(&frame, target)?;
                o.src = (frame.width, frame.height, frame.format);
            }
            o.sws.scale_frame(&frame, 0, frame.height, target).map_err(|e| e.to_string())?;
            let mux = &mut o.mux;
            o.enc.send(pts, &mut |p| {
                if let Err(e) = mux.write(Track::Video, &p) {
                    failed.get_or_insert(e);
                }
            })?;
            pts += 1;
        }
    }
    let Some(mut o) = out else { return Err("the video has no frames".into()) };
    let mux = &mut o.mux;
    o.enc.finish(&mut |p| {
        if let Err(e) = mux.write(Track::Video, &p) {
            failed.get_or_insert(e);
        }
    })?;
    if let Some(e) = failed {
        return Err(e);
    }
    o.mux.finish().map(|_| ())
}

/// The sample rate captions read speech at (Whisper's).
pub const SPEECH_RATE: i32 = 16_000;

/// Audio track `track` of `path` (counting audio tracks only; the last one when `None`) as mono
/// floats at [`SPEECH_RATE`], from the file's start: silence fills any gap before the track's
/// first sample, so times match the video. For captions; runs after the save, never per frame.
pub fn read_speech(path: &Path, track: Option<usize>) -> Result<Vec<f32>> {
    let mut input = open_input(path)?;
    let audio: Vec<usize> = (0..input.streams().len())
        .filter(|&i| input.streams()[i].codecpar().codec_type == ffi::AVMEDIA_TYPE_AUDIO)
        .collect();
    let stream = match track {
        Some(t) => audio.get(t).copied(),
        None => audio.last().copied(),
    }
    .ok_or("the file has no such sound track")?;
    let s = &input.streams()[stream];
    let tb = s.time_base;
    let codec = AVCodec::find_decoder(s.codecpar().codec_id).ok_or("FFmpeg can't decode this sound track")?;
    let mut ctx = AVCodecContext::new(&codec);
    ctx.apply_codecpar(&s.codecpar()).map_err(|e| e.to_string())?;
    ctx.set_pkt_timebase(tb);
    ctx.open(None).map_err(|e| format!("can't open the sound decoder: {e}"))?;
    let mono = AVChannelLayout::from_nb_channels(1);
    let mut swr =
        SwrContext::new(&mono, ffi::AV_SAMPLE_FMT_FLT, SPEECH_RATE, &ctx.ch_layout, ctx.sample_fmt, ctx.sample_rate)
            .map_err(|e| format!("can't resample this sound track: {e}"))?;
    swr.init().map_err(|e| format!("can't resample this sound track: {e}"))?;
    let start_us = if input.start_time == NOPTS { 0 } else { input.start_time };
    let duration_us = input.duration;

    let mut out = Vec::new();
    let mut first = true;
    let mut drain = |ctx: &mut AVCodecContext, swr: &mut SwrContext, out: &mut Vec<f32>| -> Result<()> {
        loop {
            match ctx.receive_frame() {
                Ok(f) => {
                    if std::mem::take(&mut first) && f.best_effort_timestamp != NOPTS {
                        let lead_us = to_us(f.best_effort_timestamp, tb).saturating_sub(start_us);
                        out.resize(lead_samples(lead_us, duration_us), 0.0);
                    }
                    resample(swr, Some(&f), out)?;
                }
                Err(RsmpegError::DecoderDrainError | RsmpegError::DecoderFlushedError) => return Ok(()),
                Err(e) => return Err(format!("decoding the sound failed: {e}")),
            }
        }
    };
    while let Some(p) = input.read_packet().map_err(|e| format!("reading failed: {e}"))? {
        if p.stream_index as usize == stream {
            ctx.send_packet(Some(&p)).map_err(|e| format!("decoding the sound failed: {e}"))?;
            drain(&mut ctx, &mut swr, &mut out)?;
        }
    }
    ctx.send_packet(None).map_err(|e| format!("decoding the sound failed: {e}"))?;
    drain(&mut ctx, &mut swr, &mut out)?;
    // The resampler's last few samples.
    while resample(&mut swr, None, &mut out)? > 0 {}
    Ok(out)
}

/// Silence before a sound track's first sample (`lead_us` after the file's start), in samples at
/// [`SPEECH_RATE`]. A damaged file's timestamps can claim hours (gigabytes of silence): the gap
/// never exceeds the file's length (`duration_us`, unknown when not positive) or [`MAX_LEAD_US`].
fn lead_samples(lead_us: i64, duration_us: i64) -> usize {
    let most = if duration_us > 0 { duration_us.min(MAX_LEAD_US) } else { MAX_LEAD_US };
    let lead_us = lead_us.clamp(0, most);
    (lead_us * i64::from(SPEECH_RATE) / 1_000_000) as usize
}

/// Ten minutes (38 MB of samples); a real clip's sound starts within its first seconds.
// shortcut: a track that really starts later (an outside file whose mic joins late) gets early
// captions; raise the cap or pad lazily if such files ever need captions.
const MAX_LEAD_US: i64 = 600_000_000;

/// Appends `frame` (or, with `None`, what the resampler still holds) to `out`; returns the count.
fn resample(swr: &mut SwrContext, frame: Option<&AVFrame>, out: &mut Vec<f32>) -> Result<usize> {
    let n = frame.map_or(0, |f| f.nb_samples);
    let room = swr.get_out_samples(n).max(0) as usize;
    let len = out.len();
    out.resize(len + room, 0.0);
    let mut dst = out[len..].as_mut_ptr().cast::<u8>();
    let src = frame.map_or(std::ptr::null(), |f| f.extended_data as *const *const u8);
    // SAFETY: `dst` points at `room` free f32 samples (mono, packed: one plane); `src` is the
    // decoded frame's plane pointers for `n` samples, or null to flush.
    let got = unsafe { swr.convert(&mut dst, room as i32, src, n) }.map_err(|e| format!("resampling failed: {e}"))?;
    out.truncate(len + got as usize);
    Ok(got as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_aspect_and_never_enlarges() {
        assert_eq!(fit(1920, 1080, PREVIEW_MAX), (640, 360));
        assert_eq!(fit(2560, 1080, PREVIEW_MAX), (640, 270));
        assert_eq!(fit(1080, 1920, PREVIEW_MAX), (202, 360));
        assert_eq!(fit(256, 144, PREVIEW_MAX), (256, 144));
        assert_eq!(fit(3840, 2160, THUMB_MAX), (480, 270));
    }

    #[test]
    fn snippets_spread_across_long_videos_and_cover_short_ones() {
        assert_eq!(snippets(3_000_000), [(0, 6_000_000)]);
        assert_eq!(snippets(0), [(0, 6_000_000)]);
        let s = snippets(60_000_000);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0], (11_250_000, 12_750_000));
        assert_eq!(s[3], (47_250_000, 48_750_000));
        // A damaged header's length doesn't overflow.
        assert!(snippets(i64::MAX).iter().all(|(a, b)| 0 < *a && a < b));
    }

    /// Found by fuzzing: a file FFmpeg opens without any stream used to reach a slice made from a
    /// null pointer. Every reader now refuses it plainly.
    #[test]
    fn a_file_without_streams_is_refused() {
        // An MP4 whose `moov` holds only its header (`mvhd`), no track.
        let head = [&b"\0\0\0\x14ftypisom\0\0\0\0isom\0\0\0\x74moov\0\0\0\x6cmvhd"[..], &[0; 100]].concat();
        let path = std::env::temp_dir().join(format!("backsight-no-streams-{}.mp4", std::process::id()));
        fs::write(&path, head).unwrap();
        let no_tracks = |r: Result<()>| r.is_err_and(|e| e.contains("no video or sound"));
        assert!(no_tracks(probe(&path).map(|_| ())), "{:?}", probe(&path));
        assert!(no_tracks(read_speech(&path, None).map(|_| ())));
        assert!(no_tracks(thumbnail(&path, &path.with_extension("jpg"))));
        let copy =
            crate::remux::copy(&path, &path.with_extension("out.mp4"), "mp4", &Default::default(), &[], true, None);
        assert!(no_tracks(copy.map(|_| ())));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn silence_before_the_sound_is_bounded() {
        assert_eq!(lead_samples(1_500_000, 60_000_000), 24_000, "1.5 s");
        assert_eq!(lead_samples(-5, 60_000_000), 0);
        assert_eq!(lead_samples(90_000_000, 60_000_000), 960_000, "never past the end");
        assert_eq!(lead_samples(1_000_000, i64::MIN), 16_000, "unknown length");
        assert_eq!(lead_samples(1_000_000, 0), 16_000, "a length of 0 is unknown too");
        // A damaged timestamp near i64::MAX: at most ten minutes, not an overflow or exabytes.
        assert_eq!(lead_samples(i64::MAX, i64::MAX), 9_600_000);
    }

    #[test]
    fn pq_maps_paper_white_near_white_and_never_clips_hard() {
        let grey = |pq: f32| pq_to_sdr([pq; 3]);
        assert_eq!(grey(0.0), [0.0; 3]);
        let white = grey(0.58)[0]; // about 203 nits
        assert!((0.9..0.97).contains(&white), "{white}");
        let mid = grey(0.5)[0]; // about 92 nits
        assert!((0.65..0.75).contains(&mid), "{mid}");
        assert!(grey(0.75)[0] > white && grey(0.75)[0] < 1.0, "highlights roll off");
        assert!(grey(1.0)[0] <= 1.0);
        // A pure BT.2020 red stays red.
        let [r, g, b] = pq_to_sdr([0.58, 0.0, 0.0]);
        assert!(r > 0.8 && g < 0.05 && b < 0.05, "{r} {g} {b}");
    }

    #[test]
    fn greys_stay_grey_and_bright_ones_roll_off_by_the_curve() {
        for pq in [0.3, 0.5, 0.58, 0.75, 0.9] {
            let [r, g, b] = pq_to_sdr([pq; 3]);
            assert!((r - g).abs() < 1e-3 && (g - b).abs() < 1e-3, "PQ {pq}: {r} {g} {b}");
        }
        // Pure primaries don't leak into a channel the matrix takes away from.
        assert_eq!(pq_to_sdr([0.0, 0.58, 0.0])[0], 0.0);
        assert_eq!(pq_to_sdr([0.0, 0.0, 0.58])[1], 0.0);
        // Paper white (linear 1.0) is a quarter past the knee: 0.75 + 0.25 × ½, then sRGB's curve.
        let white = pq_to_sdr([0.58; 3])[0];
        assert!((white - 0.942).abs() < 2e-3, "{white}");
        let bright = pq_to_sdr([0.75; 3])[0]; // about 1000 nits
        assert!((bright - 0.9936).abs() < 5e-4, "{bright}");
    }

    #[test]
    fn sizes_must_be_positive() {
        assert_eq!([-1, 0, 5].map(positive), [None, None, Some(5)]);
    }
}
