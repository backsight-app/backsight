//! Writing encoded packets into the catalog's containers. Part of the FFmpeg edge.

use crate::{
    catalog::Container,
    session::{Encoded, Result, StreamParams, rescale},
};
use rsmpeg::{
    avcodec::AVPacket,
    avformat::{AVFormatContextOutput, AVOutputFormat},
    avutil::AVDictionary,
    ffi,
};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString, OsString},
    fs,
    path::{Path, PathBuf},
};

/// What a recording's file name ends with until it is finished, after the name it will have:
/// `Backsight 2026-11-10 14.03.07.mp4.recording.part`. Only Backsight writes it, so one left
/// behind is an interrupted recording of ours (ADR 0008).
pub const RECORDING_PART: &str = ".recording.part";

/// `<folder>\<stem>.<ext>`, or ` (2)`, ` (3)` … added to the stem when that file exists or a
/// save, recording or copy is being written under it, other than `besides` (the interrupted
/// recording being recovered into its own name).
pub fn free_name(folder: &Path, stem: &str, ext: &str, besides: Option<&Path>) -> PathBuf {
    let name = |n: u32| match n {
        1 => format!("{stem}.{ext}"),
        n => format!("{stem} ({n}).{ext}"),
    };
    let taken = |p: &PathBuf| {
        let with = |suffix: &str| {
            let mut part = p.clone().into_os_string();
            part.push(suffix);
            Some(Path::new(&part)) != besides && Path::new(&part).exists()
        };
        // A clip waiting for its vertical copy (ADR 0015) still owns its name.
        let waiting = p.with_file_name(format!(
            "{} (vertical).{ext}.source.part",
            p.file_stem().unwrap_or_default().to_string_lossy()
        ));
        p.exists() || waiting.exists() || [".part", RECORDING_PART, ".chapters.part"].into_iter().any(with)
    };
    (1..).map(|n| folder.join(name(n))).find(|p| !taken(p)).expect("some name is free")
}

/// Whether encoders feeding this container should put codec headers in the container
/// (MP4, MOV, MKV) rather than in every keyframe (MPEG-TS, FLV).
pub fn needs_global_header(container: &Container) -> bool {
    CString::new(container.muxer.as_str())
        .ok()
        .and_then(|m| AVOutputFormat::guess_format(Some(&m), None, None))
        .is_some_and(|f| f.flags & ffi::AVFMT_GLOBALHEADER as i32 != 0)
}

/// Whether `container` keeps markers (as chapters). MPEG-TS and FLV can't.
pub fn holds_chapters(container: &Container) -> bool {
    muxer_holds_chapters(&container.muxer)
}

// shortcut: FFmpeg has no flag for chapter support, so the muxers are named here; move it to
// content/encoders.json if containers are added.
fn muxer_holds_chapters(muxer: &str) -> bool {
    matches!(muxer, "mp4" | "mov" | "matroska" | "webm")
}

/// Fragmented MP4/MOV write their index (moov) with the first fragment and never again, so
/// chapters known only at the end need a final copy (docs/reference/platform-notes.md).
fn fragmented(options: &BTreeMap<String, String>) -> bool {
    options.get("movflags").is_some_and(|f| f.contains("frag_"))
}

const MICROSECONDS: ffi::AVRational = ffi::AVRational { num: 1, den: 1_000_000 };

/// Appends a chapter to `ctx`, which frees it with the context. FFmpeg writes chapters added
/// before the header (any muxer that has them) or, when there were none then, before the trailer
/// (MKV, and MP4/MOV unless fragmented).
pub(crate) fn push_chapter(
    ctx: *mut ffi::AVFormatContext,
    time_base: ffi::AVRational,
    start: i64,
    end: i64,
    title: &CStr,
) -> Result<()> {
    // SAFETY: `ctx` is a live format context. The chapter is zeroed, filled and appended to the
    // context's av_malloc'd array, which avformat_free_context frees with its metadata; if it
    // can't be appended it is freed here instead.
    unsafe {
        let c = ffi::av_mallocz(size_of::<ffi::AVChapter>()).cast::<ffi::AVChapter>();
        if c.is_null() {
            return Err("out of memory while adding a marker".into());
        }
        (*c).id = i64::from((*ctx).nb_chapters) + 1;
        (*c).time_base = time_base;
        (*c).start = start;
        (*c).end = end;
        let mut ret = ffi::av_dict_set(&mut (*c).metadata, c"title".as_ptr(), title.as_ptr(), 0);
        if ret >= 0 {
            ret = ffi::av_dynarray_add_nofree(
                (&raw mut (*ctx).chapters).cast(),
                (&raw mut (*ctx).nb_chapters).cast(),
                c.cast(),
            );
        }
        if ret < 0 {
            ffi::av_dict_free(&mut (*c).metadata);
            ffi::av_free(c.cast());
            return Err("out of memory while adding a marker".into());
        }
    }
    Ok(())
}

/// Markers as chapters (µs from the file's start): a "Start" chapter, then one from each marker
/// (clamped into the file) to the next one or `end`. None without markers.
///
/// MP4/MOV need the chapter at zero: their chapter track's first sample always plays at zero
/// (no edit list), so a first marker at 1 s would read back at 0 s.
pub(crate) fn chapter_spans(mut starts: Vec<i64>, end: i64) -> Vec<(i64, i64)> {
    if starts.is_empty() {
        return Vec::new();
    }
    let end = end.max(0);
    starts.iter_mut().for_each(|s| *s = (*s).clamp(0, end));
    starts.push(0);
    starts.sort_unstable();
    starts.dedup();
    let ends = starts.iter().skip(1).copied().chain([end]);
    starts.iter().copied().zip(ends).collect()
}

/// Adds [`chapter_spans`] (µs) to `ctx`, named "Start", "Marker 1", "Marker 2"…
pub(crate) fn push_markers(ctx: *mut ffi::AVFormatContext, chapters: &[(i64, i64)]) -> Result<()> {
    chapters.iter().enumerate().try_for_each(|(n, &(start, end))| {
        let title = if n == 0 { c"Start".to_owned() } else { CString::new(format!("Marker {n}")).expect("no NUL") };
        push_chapter(ctx, MICROSECONDS, start, end, &title)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Video,
    Audio(usize),
}

pub struct Muxer {
    out: Option<AVFormatContextOutput>,
    part: PathBuf,
    path: PathBuf,
    time_bases: Vec<ffi::AVRational>,
    /// Timestamps are rebased so the file starts at zero (a clip starts mid-session).
    offset_us: Option<i64>,
    /// A failed recording keeps its `.part`; a failed clip leaves nothing.
    keep_part: bool,
    muxer: String,
    options: BTreeMap<String, String>,
    /// Marker times on the packets' clock, see [`Self::add_marker`].
    markers: Vec<i64>,
    /// End of the latest packet, from the file's start (µs), and the packets written.
    end_us: i64,
    packets: u64,
}

impl Muxer {
    /// Writes to `<path>.part` and renames it to `path` in [`Self::finish`], so a failed save
    /// never leaves a broken file under the real name.
    pub fn create(path: &Path, container: &Container, video: &StreamParams, audio: &[&StreamParams]) -> Result<Self> {
        Self::create_as(path, ".part", container, video, audio)
    }

    /// [`Self::create`] for a recording: writes `<path>.recording.part` ([`RECORDING_PART`]), so
    /// one that Backsight or Windows stopped before it was finished can be found and recovered.
    pub fn create_recording(
        path: &Path,
        container: &Container,
        video: &StreamParams,
        audio: &[&StreamParams],
    ) -> Result<Self> {
        Self::create_as(path, RECORDING_PART, container, video, audio)
    }

    fn create_as(
        path: &Path,
        suffix: &str,
        container: &Container,
        video: &StreamParams,
        audio: &[&StreamParams],
    ) -> Result<Self> {
        let mut part = OsString::from(path);
        part.push(suffix);
        let part = PathBuf::from(part);
        let cpart = CString::new(part.to_str().ok_or("save path is not valid Unicode")?).map_err(|e| e.to_string())?;
        let cmuxer = CString::new(container.muxer.as_str()).map_err(|e| e.to_string())?;
        let out = AVFormatContextOutput::builder()
            .format_name(&cmuxer)
            .filename(&cpart)
            .build()
            .map_err(|e| format!("can't create {}: {e}", part.display()))?;
        // From here on, dropping `muxer` closes the file, then removes it.
        let mut muxer = Muxer {
            out: Some(out),
            part,
            path: path.to_path_buf(),
            time_bases: Vec::new(),
            offset_us: None,
            keep_part: false,
            muxer: container.muxer.clone(),
            options: container.muxer_options.clone(),
            markers: Vec::new(),
            end_us: 0,
            packets: 0,
        };
        let out = muxer.out.as_mut().expect("just set");
        for params in std::iter::once(video).chain(audio.iter().copied()) {
            let mut par = params.codecpar.clone();
            // Clips and recordings start mid-stream, after the encoder's priming samples, so
            // there is no start delay to skip; Matroska would otherwise shift audio by a frame.
            // SAFETY: `par` is our own copy; initial_padding is a plain integer field.
            unsafe { (*par.as_mut_ptr()).initial_padding = 0 };
            let mut stream = out.new_stream();
            stream.set_codecpar(par);
            stream.set_time_base(params.time_base);
        }
        let mut dict = dictionary(&container.muxer_options)?;
        out.write_header(&mut dict).map_err(|e| format!("can't start the {} file: {e}", container.muxer))?;
        if let Some(unused) = dict {
            return Err(format!("unused muxer options: {:?}", unused.get_string(b'=', b',').unwrap_or_default()));
        }
        // The muxer may change the time bases while writing the header.
        muxer.time_bases = out.streams().iter().map(|s| s.time_base).collect();
        Ok(muxer)
    }

    /// For recordings: if the file can't be finished, keep the `.part` (a crash-safe container
    /// plays up to its last fragment) instead of removing it.
    pub fn keep_part_on_failure(&mut self) {
        self.keep_part = true;
    }

    /// Where the file is written until [`Self::finish`].
    pub fn part(&self) -> &Path {
        &self.part
    }

    /// Marks `at_us` (on the packets' clock) as the start of a chapter, written when the file is
    /// finished. Containers that can't hold chapters ([`holds_chapters`]) leave it out.
    pub fn add_marker(&mut self, at_us: i64) {
        self.markers.push(at_us);
    }

    pub fn write(&mut self, track: Track, p: &Encoded) -> Result<()> {
        let index = match track {
            Track::Video => 0,
            Track::Audio(i) => 1 + i,
        };
        let tb = *self.time_bases.get(index).ok_or("no such track in this file")?;
        let offset = *self.offset_us.get_or_insert(p.dts_us);
        self.end_us = self.end_us.max(p.pts_us + p.duration_us - offset);
        let to_tb = |us: i64| rescale(us - offset, 1, 1_000_000, tb.num.into(), tb.den.into());

        // shortcut: one FFmpeg packet allocation per write (C heap, on the save or recording writer thread,
        // never the capture/encode threads); interleave ourselves and use av_write_frame if it profiles hot.
        let mut pkt = AVPacket::new();
        let size = i32::try_from(p.data.len()).map_err(|_| "packet too large")?;
        // SAFETY: `pkt` is a fresh, empty packet; av_new_packet gives it an owned buffer of `size` bytes.
        let ret = unsafe { ffi::av_new_packet(pkt.as_mut_ptr(), size) };
        if ret < 0 {
            return Err("out of memory while saving".into());
        }
        // SAFETY: the packet buffer holds `size` bytes and can't overlap the borrowed source slice.
        unsafe { std::ptr::copy_nonoverlapping(p.data.as_ptr(), pkt.data, p.data.len()) };
        pkt.set_pts(to_tb(p.pts_us));
        pkt.set_dts(to_tb(p.dts_us));
        pkt.set_duration(rescale(p.duration_us, 1, 1_000_000, tb.num.into(), tb.den.into()));
        pkt.set_stream_index(index as i32);
        pkt.set_flags(if p.keyframe { ffi::AV_PKT_FLAG_KEY as i32 } else { 0 });
        let out = self.out.as_mut().ok_or("file already finished")?;
        out.interleaved_write_frame(&mut pkt).map_err(|e| format!("writing failed: {e}"))?;
        self.packets += 1;
        Ok(())
    }

    /// Finishes the file, with its markers as chapters, and moves it to its real name.
    pub fn finish(self) -> Result<PathBuf> {
        self.finish_noting_markers().map(|(path, _)| path)
    }

    /// [`Self::finish`], and whether markers were added but couldn't be kept (the file is whole
    /// without them), so the user can be told.
    pub fn finish_noting_markers(mut self) -> Result<(PathBuf, bool)> {
        let mut lost = false;
        let mut out = self.out.take().ok_or("file already finished")?;
        let chapters = match self.offset_us {
            Some(offset) if muxer_holds_chapters(&self.muxer) => {
                chapter_spans(self.markers.iter().map(|m| m - offset).collect(), self.end_us)
            }
            _ => Vec::new(),
        };
        let copy_for_chapters = !chapters.is_empty() && fragmented(&self.options);
        if !copy_for_chapters && let Err(e) = push_markers(out.as_mut_ptr(), &chapters) {
            tracing::warn!("markers left out of {}: {e}", self.path.display());
            lost = true;
        }
        let trailer = out.write_trailer().map_err(|e| format!("finishing the file failed: {e}"));
        let closed = close_output(out);
        trailer?;
        closed?;
        if copy_for_chapters {
            // shortcut: copying a long recording takes seconds to minutes and the stop waits for it;
            // try movflags=hybrid_fragmented (moov rewritten at the end, so no copy) if users mind.
            match self.copy_with_chapters(&chapters) {
                Ok(used) => {
                    self.path = PathBuf::new(); // published: Drop must leave it alone
                    return Ok((used, false));
                }
                // The file is whole without them: publish it as it is.
                Err(e) => {
                    tracing::warn!("markers left out of {}: {e}", self.path.display());
                    lost = true;
                }
            }
        }
        let used = publish(&self.part, &self.path)?;
        self.path = PathBuf::new(); // published: Drop must leave it alone
        Ok((used, lost))
    }

    /// Copies the finished fragmented file to `<path>.chapters.part` as plain MP4/MOV with the
    /// chapters known before its header, checks that every packet arrived, publishes the copy, then removes the
    /// `.part` (everything in it is now in the published file).
    fn copy_with_chapters(&self, chapters: &[(i64, i64)]) -> Result<PathBuf> {
        let mut tmp = OsString::from(&self.path);
        tmp.push(".chapters.part");
        let tmp = PathBuf::from(tmp);
        // Reserves the name: never overwrites a file that is already there.
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| format!("can't create {}: {e}", tmp.display()))?;
        // Unfragmented: a fragmented file's chapter track starts at zero (no edit list), which
        // moves the first marker to the start. The file is finished, so it needs no fragments.
        let copied = crate::remux::copy(&self.part, &tmp, &self.muxer, &BTreeMap::new(), chapters, false, None)
            .and_then(|c| {
                if c.packets == self.packets {
                    Ok(())
                } else {
                    Err(format!("the copy has {} of {} packets", c.packets, self.packets))
                }
            })
            .and_then(|()| {
                // The original is removed after this, so its length must survive too.
                let length = |p: &Path| crate::media::probe(p).ok().and_then(|m| m.duration_ms);
                match (length(&self.part), length(&tmp)) {
                    (Some(a), Some(b)) if a.abs_diff(b) <= 1000 => Ok(()),
                    (a, b) => Err(format!("the copy lasts {b:?} ms, the recording {a:?} ms")),
                }
            })
            .and_then(|()| publish(&tmp, &self.path));
        if copied.is_err() {
            let _ = fs::remove_file(&tmp); // our own unpublished copy
        } else {
            let _ = fs::remove_file(&self.part);
        }
        copied
    }
}

/// Muxer options as an FFmpeg dictionary.
pub(crate) fn dictionary(options: &BTreeMap<String, String>) -> Result<Option<AVDictionary>> {
    let mut dict = None;
    for (k, v) in options {
        let (k, v) = (
            CString::new(k.as_str()).map_err(|e| e.to_string())?,
            CString::new(v.as_str()).map_err(|e| e.to_string())?,
        );
        dict = Some(match dict {
            Some(d) => AVDictionary::set(d, &k, &v, 0),
            None => AVDictionary::new(&k, &v, 0),
        });
    }
    Ok(dict)
}

/// Moves a finished `.part` to its real name; moves `part` to `path`, never replacing a file: a taken name gets " (2)", " (3)"... Returns the
/// name used.
fn publish(part: &Path, path: &Path) -> Result<PathBuf> {
    crate::fsutil::publish(part, path).map_err(|e| format!("can't rename {}: {e}", part.display()))
}

impl Drop for Muxer {
    /// An unfinished clip leaves nothing behind; an unfinished recording keeps its `.part`.
    fn drop(&mut self) {
        // Closed first either way: Windows can't remove a file FFmpeg still holds open.
        if let Some(out) = self.out.take()
            && let Err(e) = close_output(out)
        {
            tracing::warn!("closing {}: {e}", self.part.display());
        }
        if !self.path.as_os_str().is_empty() && !self.keep_part {
            let _ = fs::remove_file(&self.part);
        }
    }
}

/// Closes and frees `out`, returning the close error (FFmpeg reports an earlier failed write,
/// such as a full disk or an unplugged drive, here). rsmpeg's drop unwraps that error instead,
/// and release builds abort on a panic.
pub(crate) fn close_output(out: AVFormatContextOutput) -> Result<()> {
    // Forgets the wrapper and its io_context field, which only points at `pb`.
    let ctx = out.into_raw().as_ptr();
    // SAFETY: `ctx` is a valid output context no one else owns any more. avio_closep closes and
    // frees `pb` (and nulls it) when the muxer uses one, as rsmpeg's drop would;
    // avformat_free_context then frees the context and its streams, leaving `pb` alone.
    let ret = unsafe {
        let ret =
            if (*(*ctx).oformat).flags & ffi::AVFMT_NOFILE as i32 == 0 { ffi::avio_closep(&mut (*ctx).pb) } else { 0 };
        ffi::avformat_free_context(ctx);
        ret
    };
    if ret < 0 {
        tracing::error!("closing an output file failed: FFmpeg error {ret}");
        return Err(
            "Backsight couldn't finish writing the file. Check that the drive has free space and is still connected, then try again."
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{chapter_spans, free_name};

    #[test]
    fn a_clip_waiting_for_its_vertical_copy_keeps_its_name() {
        let dir = std::env::temp_dir().join(format!("backsight-free-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("G T (vertical).mp4.source.part"), b"").unwrap();
        assert_eq!(free_name(&dir, "G T", "mp4", None), dir.join("G T (2).mp4"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn markers_become_sorted_chapters_inside_the_file() {
        assert_eq!(chapter_spans(vec![], 10), []);
        assert_eq!(chapter_spans(vec![5, 2], 10), [(0, 2), (2, 5), (5, 10)]);
        // Before the start, after the end and twice at the same time.
        assert_eq!(chapter_spans(vec![-3, 4, 4, 12], 10), [(0, 4), (4, 10), (10, 10)]);
    }
}
