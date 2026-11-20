//! Rewrapping files with stream copy (no re-encode): a finished MKV recording as MP4, a
//! fragmented recording with its markers, and what plays of an interrupted recording. Part of
//! the FFmpeg edge. The input is never touched: the caller decides what happens to it.

use crate::{
    mux::{close_output, dictionary, free_name, push_chapter, push_markers},
    session::Result,
};
use rsmpeg::{avcodec::AVCodecParameters, avformat::AVFormatContextOutput, ffi};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString, OsString},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering::Relaxed},
};

/// Starts every [`copy`] error that means the output couldn't be written (a full or unplugged
/// drive), as opposed to bad data in the input.
pub const CANT_WRITE: &str = "can't write: ";
/// The [`copy`] error when its `cancel` flag stopped it.
pub const CANCELLED: &str = "copy cancelled";

/// The MP4 and what it left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remuxed {
    pub path: PathBuf,
    /// One line per MKV stream MP4 can't hold (subtitles, attachments, unsupported codecs).
    pub skipped: Vec<String>,
}

/// What [`copy`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// One line per input stream the output can't hold.
    pub skipped: Vec<String>,
    /// Packets written.
    pub packets: u64,
    /// When salvaging: why the copy ended before the end of the input (it was cut off).
    pub cut_off: Option<String>,
    /// When salvaging: packets the muxer refused (a bad timestamp) and that were left out.
    pub refused: u64,
}

/// Removes the `.part` file unless `keep` is set. Declare it before the output so the output
/// (and its open file handle) is dropped first.
struct Part {
    path: PathBuf,
    keep: bool,
}

impl Drop for Part {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn with_part(path: &Path) -> PathBuf {
    let mut name = OsString::from(path);
    name.push(".part");
    PathBuf::from(name)
}

/// Copies every video and audio stream of `input` into a new MP4 next to it and returns its path.
/// See [`remux_to_mp4_report`] for what was left out.
pub fn remux_to_mp4(input: &Path) -> Result<PathBuf> {
    remux_to_mp4_report(input, &mut |_| {}).map(|r| r.path)
}

/// "Convert to MP4 when done": remuxes `input` and keeps the MP4 only when it holds every stream
/// and probes to the same length (within a second); otherwise removes it and says why. The MKV is
/// always kept (Backsight never deletes a recording by itself). `reserved` is as in [`remux_to_mp4_report`].
pub fn remux_checked(input: &Path, reserved: &mut dyn FnMut(&Path)) -> Result<PathBuf> {
    let r = remux_to_mp4_report(input, reserved)?;
    let length = |p: &Path| crate::media::probe(p).ok().and_then(|m| m.duration_ms);
    let problem = if !r.skipped.is_empty() {
        Some(format!("the MP4 would leave out: {}", r.skipped.join("; ")))
    } else {
        match (length(input), length(&r.path)) {
            (Some(a), Some(b)) if a.abs_diff(b) <= 1000 => None,
            (a, b) => Some(format!("the MP4's length ({b:?} ms) doesn't match the MKV ({a:?} ms)")),
        }
    };
    match problem {
        None => Ok(r.path),
        Some(p) => {
            let _ = fs::remove_file(&r.path); // our own bad output, never the recording
            Err(p)
        }
    }
}

/// Like [`remux_to_mp4`], also listing the streams that didn't fit in MP4. Chapters (markers)
/// are copied too.
///
/// Writes `<name>.mp4.part` (with `+faststart`, so the file plays while it downloads) and renames
/// it, never replacing a file. On any failure the `.part` is removed and `input` is left as it was.
/// `reserved` is told the `.part`'s path once it is ours, so a caller can remove it later if this
/// process is cut off mid-copy (quitting, shutdown).
pub fn remux_to_mp4_report(input: &Path, reserved: &mut dyn FnMut(&Path)) -> Result<Remuxed> {
    let stem = input.file_stem().and_then(|s| s.to_str()).ok_or("the recording has no usable file name")?;
    let dir = input.parent().unwrap_or(Path::new(""));
    // Reserves a name, so a `.part` someone else created at the same moment is never overwritten;
    // losing that race moves on to the next free name. Bounded: a name `free_name` sees as free
    // but `create_new` refuses (a broken link) would otherwise come back every time.
    let mut tries = 0;
    let (target, part_path) = loop {
        let target = free_name(dir, stem, "mp4", None);
        let part_path = with_part(&target);
        match fs::OpenOptions::new().write(true).create_new(true).open(&part_path) {
            Ok(_) => break (target, part_path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 100 => tries += 1,
            Err(e) => return Err(format!("{CANT_WRITE}can't create {}: {e}", part_path.display())),
        }
    };
    reserved(&part_path);
    let mut part = Part { path: part_path, keep: false };
    let options = BTreeMap::from([("movflags".to_string(), "+faststart".to_string())]);
    let copied = copy(input, &part.path, "mp4", &options, &[], false, None)?;

    // A name taken since `free_name` picked it gets the next free one; nothing is replaced.
    let path = crate::fsutil::publish(&part.path, &target)
        .map_err(|e| format!("{CANT_WRITE}can't rename {}: {e}", part.path.display()))?;
    part.keep = true;
    Ok(Remuxed { path, skipped: copied.skipped })
}

/// Copies every video and audio stream `muxer` can hold from `input` into `output`, a file the
/// caller owns (it is overwritten), with the muxer `options`. Keeps `input`'s chapters and adds
/// `markers` (µs from the start) as chapters, all known before the header so every container
/// that has chapters keeps them, fragmented MP4 included.
///
/// With `salvage`, a read error, or a run of packets the muxer refuses, ends the copy and the file
/// is finished with what was copied (an interrupted recording is cut off somewhere);
/// [`Copied::cut_off`] says why. A few refused packets (a bad timestamp) are left out instead,
/// counted in [`Copied::refused`]. An error writing to the drive is always an error, starting with
/// [`CANT_WRITE`]. Setting `cancel` stops the copy with [`CANCELLED`]; `output` is then unfinished.
pub fn copy(
    input: &Path,
    output: &Path,
    muxer: &str,
    options: &BTreeMap<String, String>,
    markers: &[(i64, i64)],
    salvage: bool,
    cancel: Option<&AtomicBool>,
) -> Result<Copied> {
    let mut src = crate::media::open_input(input)?;
    let cout = CString::new(output.to_str().ok_or("path is not valid Unicode")?).map_err(|e| e.to_string())?;
    let cmuxer = CString::new(muxer).map_err(|e| e.to_string())?;
    let mut out = AVFormatContextOutput::builder()
        .format_name(&cmuxer)
        .filename(&cout)
        .build()
        .map_err(|e| format!("{CANT_WRITE}can't create {}: {e}", output.display()))?;
    // Every exit closes `out` through close_output: rsmpeg's drop panics on a failed close.
    let copied = (|| {
        // `map[i]` is the output stream index of input stream `i`, if it is kept.
        let mut map = Vec::new();
        let mut skipped = Vec::new();
        for (i, s) in src.streams().iter().enumerate() {
            let par = s.codecpar();
            let kind = par.codec_type;
            let av = kind == ffi::AVMEDIA_TYPE_VIDEO || kind == ffi::AVMEDIA_TYPE_AUDIO;
            // SAFETY: the format is valid for the call; avcodec_get_name returns a static NUL-terminated string for any id.
            let (name, fits) = unsafe {
                let name = CStr::from_ptr(ffi::avcodec_get_name(par.codec_id)).to_string_lossy().into_owned();
                (
                    name,
                    ffi::avformat_query_codec(out.oformat().as_ptr(), par.codec_id, ffi::FF_COMPLIANCE_NORMAL as i32),
                )
            };
            if !av || fits != 1 {
                skipped.push(format!("stream {i} ({name}) can't be stored in {muxer}"));
                map.push(None);
                continue;
            }
            let mut copy = AVCodecParameters::new();
            copy.copy(&par);
            // SAFETY: `copy` is our own copy; codec_tag is a plain integer field. 0 lets the muxer
            // pick its own tag (MKV's tags don't always exist in MP4).
            unsafe { (*copy.as_mut_ptr()).codec_tag = 0 };
            map.push(Some(map.iter().flatten().count()));
            let mut o = out.new_stream();
            o.set_codecpar(copy);
            o.set_time_base(s.time_base);
        }
        if map.iter().all(Option::is_none) {
            return Err(format!("{} has no video or audio {muxer} can hold", input.display()));
        }

        let chapters: &[*mut ffi::AVChapter] = match src.nb_chapters {
            0 => &[],
            // SAFETY: the input owns `nb_chapters` valid chapters for as long as `src` lives.
            n => unsafe { std::slice::from_raw_parts(src.chapters, n as usize) },
        };
        for (n, &c) in chapters.iter().enumerate() {
            // SAFETY: each entry points at a valid chapter; av_dict_get returns null or a valid entry
            // whose value is a NUL-terminated string the dictionary owns.
            let (c, title) = unsafe {
                let entry = ffi::av_dict_get((*c).metadata, c"title".as_ptr(), std::ptr::null(), 0);
                let title = if entry.is_null() {
                    CString::new(format!("Chapter {}", n + 1)).expect("no NUL in a number")
                } else {
                    CStr::from_ptr((*entry).value).to_owned()
                };
                (&*c, title)
            };
            push_chapter(out.as_mut_ptr(), c.time_base, c.start, c.end, &title)?;
        }
        push_markers(out.as_mut_ptr(), markers)?;

        let mut dict = dictionary(options)?;
        out.write_header(&mut dict).map_err(|e| write_error(&out, format!("can't start the {muxer} file: {e}")))?;
        if let Some(unused) = dict {
            return Err(format!("unused muxer options: {:?}", unused.get_string(b'=', b',').unwrap_or_default()));
        }
        // The muxer may change the time bases while writing the header.
        let out_tb: Vec<_> = out.streams().iter().map(|s| s.time_base).collect();
        let in_tb: Vec<_> = src.streams().iter().map(|s| s.time_base).collect();

        let mut packets = 0;
        let mut cut_off = None;
        let (mut refused, mut refused_in_a_row) = (0, 0);
        loop {
            if cancel.is_some_and(|c| c.load(Relaxed)) {
                return Err(CANCELLED.into());
            }
            let mut pkt = match src.read_packet() {
                Ok(Some(pkt)) => pkt,
                Ok(None) => break,
                Err(e) if salvage => {
                    cut_off = Some(format!("reading stopped: {e}"));
                    break;
                }
                Err(e) => return Err(format!("reading {} failed: {e}", input.display())),
            };
            let i = pkt.stream_index as usize;
            let Some(Some(o)) = map.get(i).copied() else { continue };
            // shortcut: Matroska stores no DTS, so the first B-frame-delay video packets arrive without one;
            // the MP4 muxer fills them in correctly (checked with ffprobe) but logs "Timestamps are unset".
            // Synthesize them here if FFmpeg stops accepting that.
            pkt.rescale_ts(in_tb[i], out_tb[o]);
            pkt.set_stream_index(o as i32);
            pkt.set_pos(-1);
            match out.interleaved_write_frame(&mut pkt) {
                Ok(()) => {
                    packets += 1;
                    refused_in_a_row = 0;
                }
                // Never salvaged when the drive failed: a full disk must not publish a short copy.
                // The muxer checks a packet before writing any of it, so skipping one is safe.
                Err(e) if salvage && !io_failed(&out) => {
                    refused += 1;
                    refused_in_a_row += 1;
                    tracing::warn!("left out a packet the {muxer} muxer refused: {e}");
                    // shortcut: 100 refusals in a row means the rest is broken too; tune if real files say otherwise.
                    if refused_in_a_row >= 100 {
                        cut_off = Some(format!("packets couldn't be copied: {e}"));
                        break;
                    }
                }
                Err(e) => return Err(write_error(&out, format!("writing the {muxer} file failed: {e}"))),
            }
        }
        out.write_trailer().map_err(|e| write_error(&out, format!("finishing the {muxer} file failed: {e}")))?;
        Ok(Copied { skipped, packets, cut_off, refused })
    })();
    let closed = close_output(out);
    let copied = copied?;
    closed.map_err(|e| format!("{CANT_WRITE}{e}"))?;
    Ok(copied)
}

/// The output's file reported an I/O error (FFmpeg keeps the first one in `pb->error`).
fn io_failed(out: &AVFormatContextOutput) -> bool {
    // SAFETY: `out` is a valid output context; `pb` is null or its open AVIOContext.
    unsafe {
        let pb = (*out.as_ptr()).pb;
        !pb.is_null() && (*pb).error < 0
    }
}

/// `e`, marked [`CANT_WRITE`] when the drive failed.
fn write_error(out: &AVFormatContextOutput, e: String) -> String {
    if io_failed(out) { format!("{CANT_WRITE}{e}") } else { e }
}
