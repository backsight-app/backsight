//! Vertical (9:16) clips made from a saved clip: decode, crop the centre, encode, copy the audio
//! (ADR 0015). Part of the FFmpeg edge; runs after the save, never in the capture path.

use crate::{
    catalog::{Container, VideoEncoder},
    ffmpeg::open_with_options,
    media::open_input,
    mux::{Muxer, Track, needs_global_header},
    session::{Encoded, Result, StreamParams, VideoSession, rescale, to_us},
};
use backsight_config::EncoderOptions;
use rsmpeg::{
    avcodec::{AVCodecContext, AVCodecParameters},
    avutil::AVFrame,
    error::RsmpegError,
    ffi,
    swscale::SwsContext,
};
use std::path::{Path, PathBuf};

/// The tallest vertical clip; taller crops are scaled down to `MAX_WIDTH`×`MAX_HEIGHT`.
const MAX_HEIGHT: u32 = 1920;
const MAX_WIDTH: u32 = 1080;
/// `AV_NOPTS_VALUE` (a macro bindgen skips).
const NOPTS: i64 = i64::MIN;

/// The part of a frame a vertical clip shows, and the size it is encoded at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerticalCrop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub out_width: u32,
    pub out_height: u32,
}

/// The centred, exactly 9:16 crop of a `width`×`height` frame: as tall as fits, rounded down to a
/// multiple of 32 so the width is a whole even number. Kept at that size up to 1920 tall, scaled
/// down to 1080×1920 above that; never enlarged. Offsets are even (4:2:0 chroma). `None` when the
/// frame is too small to be worth it.
pub fn vertical_crop(width: u32, height: u32) -> Option<VerticalCrop> {
    let tall = height.min(width.saturating_mul(16) / 9) / 32 * 32;
    if tall < 64 {
        return None;
    }
    let wide = tall / 16 * 9;
    let (out_width, out_height) = if tall > MAX_HEIGHT { (MAX_WIDTH, MAX_HEIGHT) } else { (wide, tall) };
    Some(VerticalCrop {
        x: ((width - wide) / 2) & !1,
        y: ((height - tall) / 2) & !1,
        width: wide,
        height: tall,
        out_width,
        out_height,
    })
}

/// An audio packet read before the first video packet was written (see [`Output::video`]).
struct Held {
    track: usize,
    data: Vec<u8>,
    pts_us: i64,
    dts_us: i64,
    duration_us: i64,
    keyframe: bool,
}

/// The file being written. Audio waits for the first video packet, because the muxer starts the
/// file's clock at the first packet it gets; audio from before that is dropped.
struct Output {
    mux: Muxer,
    held: Option<Vec<Held>>,
    failed: Option<String>,
}

impl Output {
    fn video(&mut self, p: &Encoded) {
        let held = self.held.take();
        let mut written = self.mux.write(Track::Video, p);
        for h in held.into_iter().flatten().filter(|h| h.dts_us >= p.dts_us) {
            let e = Encoded {
                data: &h.data,
                pts_us: h.pts_us,
                dts_us: h.dts_us,
                duration_us: h.duration_us,
                keyframe: h.keyframe,
            };
            written = written.and_then(|()| self.mux.write(Track::Audio(h.track), &e));
        }
        if let Err(e) = written {
            self.failed.get_or_insert(e);
        }
    }

    fn audio(&mut self, track: usize, p: &Encoded) {
        match &mut self.held {
            Some(held) => held.push(Held {
                track,
                data: p.data.to_vec(),
                pts_us: p.pts_us,
                dts_us: p.dts_us,
                duration_us: p.duration_us,
                keyframe: p.keyframe,
            }),
            None => {
                if let Err(e) = self.mux.write(Track::Audio(track), p) {
                    self.failed.get_or_insert(e);
                }
            }
        }
    }
}

/// Makes a vertical copy of the clip `src` at `dst` (through `<dst>.part`, renamed when done; ` (2)`
/// if `dst` is taken) and returns the file written. Video is decoded on this thread only, cropped
/// with [`vertical_crop`] and encoded with `encoder` and `values`; HDR clips stay HDR. Every
/// audio track is copied unchanged.
pub fn make_vertical(
    src: &Path,
    dst: &Path,
    container: &Container,
    encoder: &VideoEncoder,
    values: &EncoderOptions,
) -> Result<PathBuf> {
    let mut input = open_input(src)?;
    let (vi, codec) =
        input.find_best_stream(ffi::AVMEDIA_TYPE_VIDEO).map_err(|e| e.to_string())?.ok_or("no video track")?;
    let size = |v: i32| u32::try_from(v).unwrap_or(0);
    let mut dec = AVCodecContext::new(&codec);
    let (vtb, crop, fps, hdr) = {
        let s = &input.streams()[vi];
        let par = s.codecpar();
        let crop = vertical_crop(size(par.width), size(par.height)).ok_or("the clip is too small to crop")?;
        let rate = s.guess_framerate().filter(|r| r.num > 0 && r.den > 0).ok_or("the clip has no frame rate")?;
        let fps = (i64::from(rate.num) + i64::from(rate.den) / 2) / i64::from(rate.den);
        dec.apply_codecpar(&par).map_err(|e| e.to_string())?;
        let hdr = par.color_trc == ffi::AVCOL_TRC_SMPTE2084;
        (s.time_base, crop, u32::try_from(fps).unwrap_or(0).max(1), hdr)
    };
    dec.set_pkt_timebase(vtb);
    // One thread: the job's thread runs at background priority, FFmpeg's own threads would not.
    open_with_options(&mut dec, &vec![("threads".into(), "1".into())])?;

    // `tracks[i]` is the output audio track of input stream `i`, with the stream's time base.
    let mut audio = Vec::new();
    let tracks: Vec<Option<(usize, ffi::AVRational)>> = input
        .streams()
        .iter()
        .map(|s| {
            let p = s.codecpar();
            (p.codec_type == ffi::AVMEDIA_TYPE_AUDIO).then(|| {
                let mut copy = AVCodecParameters::new();
                copy.copy(&p);
                // SAFETY: `copy` is our own copy; codec_tag is a plain integer field. 0 lets the
                // muxer pick its own tag.
                unsafe { (*copy.as_mut_ptr()).codec_tag = 0 };
                audio.push(StreamParams { codecpar: copy, time_base: s.time_base });
                (audio.len() - 1, s.time_base)
            })
        })
        .collect();

    let (out_size, global) = ((crop.out_width, crop.out_height), needs_global_header(container));
    let mut enc = if hdr {
        VideoSession::open_hdr(encoder, values, out_size, fps, global, None)?
    } else {
        VideoSession::open(encoder, values, out_size.0, out_size.1, fps, global)?
    };
    let mux = Muxer::create(dst, container, &enc.params(), &audio.iter().collect::<Vec<_>>())?;
    let mut out = Output { mux, held: Some(Vec::new()), failed: None };
    let mut sws: Option<(SwsContext, (i32, i32, i32))> = None;
    let mut last_pts = None;

    let mut encode = |frame: &mut AVFrame, enc: &mut VideoSession, out: &mut Output| -> Result<()> {
        if frame.best_effort_timestamp == NOPTS {
            return Ok(());
        }
        let pts = rescale(to_us(frame.best_effort_timestamp, vtb), i64::from(fps), 1_000_000, 1, 1);
        if last_pts.is_some_and(|last| pts <= last) {
            return Ok(()); // two frames in one slot: keep the first
        }
        last_pts = Some(pts);
        let c = vertical_crop(size(frame.width), size(frame.height)).filter(|c| *c == crop);
        let c = c.ok_or("the clip changes size partway")?;
        // SAFETY: a software frame we own; the crop lies inside it (vertical_crop), so
        // av_frame_apply_cropping only moves the plane pointers and shrinks width and height.
        let ret = unsafe {
            let f = frame.as_mut_ptr();
            (*f).crop_left = c.x as usize;
            (*f).crop_top = c.y as usize;
            (*f).crop_right = (frame.width as u32 - c.x - c.width) as usize;
            (*f).crop_bottom = (frame.height as u32 - c.y - c.height) as usize;
            ffi::av_frame_apply_cropping(f, ffi::AV_FRAME_CROP_UNALIGNED)
        };
        if ret < 0 {
            return Err(format!("cropping failed ({ret})"));
        }
        let target = enc.frame_mut()?;
        let src = (frame.width, frame.height, frame.format);
        if sws.as_ref().is_none_or(|(_, s)| *s != src) {
            let ctx = SwsContext::get_context(
                frame.width,
                frame.height,
                frame.format,
                target.width,
                target.height,
                target.format,
                ffi::SWS_BICUBIC,
                None,
                None,
                None,
            )
            .ok_or("can't scale this clip's frames")?;
            sws = Some((ctx, src));
        }
        let (ctx, _) = sws.as_mut().expect("set above");
        ctx.scale_frame(frame, 0, frame.height, target).map_err(|e| e.to_string())?;
        enc.send(pts, &mut |p| out.video(&p))
    };

    let mut flushed = false;
    while !flushed {
        match input.read_packet().map_err(|e| format!("reading the clip failed: {e}"))? {
            Some(p) if p.stream_index as usize == vi => {
                dec.send_packet(Some(&p)).map_err(|e| format!("decoding failed: {e}"))?
            }
            Some(p) => {
                if let Some(Some((track, tb))) = tracks.get(p.stream_index as usize) {
                    let dts = if p.dts == NOPTS { p.pts } else { p.dts };
                    let data = match p.size {
                        // SAFETY: a read packet owns `size` bytes at `data` while `p` lives.
                        n if n > 0 => unsafe { std::slice::from_raw_parts(p.data, n as usize) },
                        _ => &[],
                    };
                    let e = Encoded {
                        data,
                        pts_us: to_us(if p.pts == NOPTS { dts } else { p.pts }, *tb),
                        dts_us: to_us(dts, *tb),
                        duration_us: to_us(p.duration, *tb),
                        keyframe: p.flags & ffi::AV_PKT_FLAG_KEY as i32 != 0,
                    };
                    out.audio(*track, &e);
                }
                continue;
            }
            None => {
                flushed = true;
                dec.send_packet(None).map_err(|e| format!("decoding failed: {e}"))?;
            }
        }
        loop {
            match dec.receive_frame() {
                Ok(mut frame) => encode(&mut frame, &mut enc, &mut out)?,
                Err(RsmpegError::DecoderDrainError | RsmpegError::DecoderFlushedError) => break,
                Err(e) => return Err(format!("decoding failed: {e}")),
            }
        }
        if let Some(e) = out.failed.take() {
            return Err(e);
        }
    }
    enc.finish(&mut |p| out.video(&p))?;
    if let Some(e) = out.failed {
        return Err(e);
    }
    if out.held.is_some() {
        return Err("the clip has no frames".into());
    }
    out.mux.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_are_centred_9_16_and_never_enlarged() {
        let c = |w, h| vertical_crop(w, h).map(|c| (c.x, c.y, c.width, c.height, c.out_width, c.out_height));
        // 1080p: 1080 rounds down to 1056, kept as it is.
        assert_eq!(c(1920, 1080), Some((662, 12, 594, 1056, 594, 1056)));
        assert_eq!(c(2560, 1440), Some((874, 0, 810, 1440, 810, 1440)));
        // Ultrawide: same height, more cut off at the sides.
        assert_eq!(c(3440, 1440), Some((1314, 0, 810, 1440, 810, 1440)));
        // 4K: taller than 1920, scaled down.
        assert_eq!(c(3840, 2160), Some((1316, 8, 1206, 2144, 1080, 1920)));
        // Already vertical: the whole picture.
        assert_eq!(c(1080, 1920), Some((0, 0, 1080, 1920, 1080, 1920)));
        // Narrower than 9:16: the full width, cut at top and bottom.
        assert_eq!(c(720, 1600), Some((0, 160, 720, 1280, 720, 1280)));
        assert_eq!(c(100, 50), None);
        for (w, h) in [(1280, 720), (1366, 768), (1600, 900), (1920, 1200), (5120, 1440), (854, 480)] {
            let k = vertical_crop(w, h).unwrap();
            assert_eq!(k.width * 16, k.height * 9, "{w}x{h} is 9:16");
            assert!([k.width, k.x, k.y].iter().all(|v| v.is_multiple_of(2)), "{w}x{h} even for 4:2:0");
            assert!(k.x + k.width <= w && k.y + k.height <= h, "{w}x{h} inside the frame");
            assert!(k.out_height <= k.height.min(MAX_HEIGHT), "{w}x{h} never enlarged");
        }
    }
}
