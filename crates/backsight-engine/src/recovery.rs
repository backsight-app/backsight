//! Interrupted recordings: files Backsight was still writing when it, or Windows, stopped. Only
//! Backsight's own are touched, recognised by name: `<game or Backsight> <stamp>.<ext>.recording.part`
//! ([`RECORDING_PART`], ADR 0008), in the video folder or a game's folder inside it. Any other `.part` is someone else's and is never opened,
//! except the copy a cut-off recovery names in its `….recovering.part` mark.
//!
//! [`recover`] copies whatever plays into a new file next to it and renames the original to
//! `….recovered.part`, so it isn't offered again; only a stray `….chapters.part` of Backsight's
//! own is removed. A file with nothing playable is kept byte-for-byte and renamed to
//! `….unrecoverable.part`, with a plain explanation.

use crate::{
    session::user_error,
    watchdog::{Step, Watch},
};
use backsight_encode::{
    Catalog, media,
    mux::{RECORDING_PART, free_name},
    remux,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::ErrorKind,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// What an original is renamed to once recovered, after the name it would have had.
pub const RECOVERED_PART: &str = ".recovered.part";

const NOT_OURS: &str = "Backsight only recovers recordings it was writing itself, so it left this file as it is.";
const GONE: &str = "Backsight can't open this recording any more. It may have been moved or deleted.";
const IN_USE: &str = "This recording is still being written. Stop the recording first.";
const EMPTY: &str = "This recording is empty: it stopped before anything was saved, so there's nothing to recover. Backsight kept the file, renamed to end in .unrecoverable.part; you can delete it.";
const NOTHING_PLAYABLE: &str = "Backsight found nothing it can play in this recording. It stopped before its first seconds were saved, or it was recorded in a file format that isn't crash-safe. Backsight kept the file, renamed to end in .unrecoverable.part; you can delete it.";

/// What an original that can't be recovered is renamed to, so it isn't offered again.
pub const UNRECOVERABLE_PART: &str = ".unrecoverable.part";

/// Next to a recording being recovered: names the copy being written, so a crash meanwhile
/// leaves nothing the next start can't find and remove.
pub const RECOVERING_PART: &str = ".recovering.part";

/// Always true of a recovered file.
pub const RECOVERED_NOTE: &str =
    "The last few seconds before the recording stopped, and any markers in it, couldn't be saved.";

/// A recording that was never finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interrupted {
    /// The unfinished file (`….recording.part`).
    pub path: PathBuf,
    /// The name it would have had, such as `Backsight 2026-11-10 14.03.07.mp4`.
    pub name: String,
    pub bytes: u64,
}

/// A recovered recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// The new, finished file.
    pub path: PathBuf,
    /// Where the unfinished original is kept (`….recovered.part`), or still is if it couldn't
    /// be renamed.
    pub original: PathBuf,
    /// Size of that original: it can be gigabytes.
    pub original_bytes: u64,
}

/// For the "saved" pop-up: where the damaged originals went and that they can be deleted
/// (ADR 0008 never deletes them itself). `None` when `recovered` is empty.
pub fn kept_originals_note(recovered: &[Recovered]) -> Option<String> {
    let total = size_text(recovered.iter().map(|r| r.original_bytes).sum());
    match recovered {
        [] => None,
        [one] => {
            let name = one.original.file_name()?.to_string_lossy();
            Some(format!("The damaged original ({total}) was kept as {name}. Delete it once you've checked the clip."))
        }
        many => Some(format!(
            "The {} damaged originals ({total} together) were kept, each ending in {RECOVERED_PART}. Delete them once you've checked the clips.",
            many.len()
        )),
    }
}

fn size_text(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b => format!("{} MB", (b >> 20).max(1)),
    }
}

/// Backsight's own interrupted recordings in `folder` and the folders directly inside it (one per
/// game), oldest first. Ones being written right now (by this or another Backsight) are left out.
/// A missing folder has none.
pub fn find_interrupted(folder: &Path) -> Vec<Interrupted> {
    let mut found: Vec<_> = files(folder)
        .into_iter()
        .filter_map(|e| {
            let path = e.path();
            let (stem, ext) = owned_name(&path)?;
            (!in_use(&path)).then(|| Interrupted {
                name: format!("{stem}.{ext}"),
                bytes: e.metadata().map_or(0, |m| m.len()),
                path,
            })
        })
        .collect();
    // Oldest first: names end with the time they started (and maybe " (2)").
    found.sort_by(|a, b| stamp_key(&a.name).cmp(&stamp_key(&b.name)));
    found
}

/// The files in `folder` and the folders directly inside it (one per game).
pub(crate) fn files(folder: &Path) -> Vec<fs::DirEntry> {
    let Ok(entries) = fs::read_dir(folder) else { return Vec::new() };
    let entries: Vec<_> = entries.flatten().collect();
    let games = entries.iter().filter(|e| e.file_type().is_ok_and(|t| t.is_dir()));
    let inside: Vec<_> = games.filter_map(|e| fs::read_dir(e.path()).ok()).flat_map(|d| d.flatten()).collect();
    entries.into_iter().chain(inside).filter(|e| e.file_type().is_ok_and(|t| t.is_file())).collect()
}

/// `<stem>.mkv.to-mp4.part` next to a recording's MKV: its MP4 conversion hasn't finished. Made by
/// [`mark_conversion`] before converting and removed once the conversion returned, so quitting at
/// any point leaves it behind. Holds the path of the `.part` the conversion reserved, so the next
/// attempt removes exactly that leftover. Not a name the conversion itself writes (`<stem>.mp4.part`, or
/// `<stem> (2).mp4.part` when a clip owns `<stem>.mp4`), so it never pushes the MP4 to ` (2)`.
fn conversion_mark(mkv: &Path) -> PathBuf {
    let mut name = mkv.as_os_str().to_owned();
    name.push(".to-mp4.part");
    PathBuf::from(name)
}

/// Notes that `mkv` is about to be converted to MP4, so a conversion cut off by quitting (or
/// Windows shutting down) is finished at the next start ([`unfinished_conversions`]).
pub fn mark_conversion(mkv: &Path) {
    let _ = fs::OpenOptions::new().write(true).create_new(true).open(conversion_mark(mkv));
}

/// Backsight's own MKV recordings in `folder` (and its game folders) whose MP4 conversion was cut
/// off: their [`mark_conversion`] mark lies next to them. Oldest first.
pub fn unfinished_conversions(folder: &Path) -> Vec<PathBuf> {
    let mut found: Vec<_> = files(folder)
        .into_iter()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mkv")))
        .filter(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| stamp_at(s).is_some()))
        .filter(|p| conversion_mark(p).is_file())
        .collect();
    let key = |p: &PathBuf| p.file_name().unwrap_or_default().to_string_lossy().into_owned();
    found.sort_by(|a, b| stamp_key(&key(a)).cmp(&stamp_key(&key(b))));
    found
}

/// Converts `mkv` to MP4 ([`remux::remux_checked`]), then removes its [`mark_conversion`] mark,
/// whether it worked or not (a failure is told, not retried). The MKV is always kept. A file
/// already named `<stem>.mp4` (a clip saved the same second) is never replaced.
pub fn finish_conversion(mkv: &Path) -> Result<PathBuf, String> {
    let _watch = Watch::start(Step::Convert { bytes: fs::metadata(mkv).map_or(0, |m| m.len()) });
    let mark = conversion_mark(mkv);
    remove_cut_off_conversion(mkv, &mark);
    let converted = remux::remux_checked(mkv, &mut |part| {
        let _ = fs::write(&mark, part.to_string_lossy().as_bytes());
    });
    // A full drive keeps the mark, so the next start tries again once space is freed.
    if !converted.as_ref().is_err_and(|e| e.starts_with(remux::CANT_WRITE)) {
        let _ = fs::remove_file(&mark);
    }
    converted
}

/// Gives up an unfinished conversion of `mkv` ("Convert to MP4" was turned off since): removes
/// its cut-off `.mp4.part` and its mark. The MKV is never touched.
pub fn drop_conversion(mkv: &Path) {
    let mark = conversion_mark(mkv);
    remove_cut_off_conversion(mkv, &mark);
    let _ = fs::remove_file(&mark);
}

/// An attempt cut off by quitting left its `.part`, named in `mark`: only that file goes.
fn remove_cut_off_conversion(mkv: &Path, mark: &Path) {
    if let Ok(old) = fs::read_to_string(mark) {
        let old = PathBuf::from(old);
        let ours = old.parent() == mkv.parent() && old.to_str().is_some_and(|s| s.ends_with(".mp4.part"));
        if ours && old.is_file() && !in_use(&old) {
            let _ = fs::remove_file(&old);
        }
    }
}

/// Copies what plays of the interrupted recording `path` into a new file named as the recording
/// would have been (` (2)` … added when taken; nothing is ever replaced), written through a
/// `.part` first. Then renames the original to `….recovered.part` and removes a stray
/// `<name>.chapters.part` (an unfinished marker copy) not in use. [`RECOVERED_NOTE`] says what a
/// recovered file lacks. An error says, in plain words, why nothing could be recovered; the
/// original keeps its bytes, and its name too unless nothing in it plays: then it is renamed to
/// `….unrecoverable.part` so it isn't offered again.
///
/// Setting `cancel` (quitting) stops the copy, removes it and returns [`remux::CANCELLED`]; the
/// original is left as it was, to be recovered at the next start.
pub fn recover(path: &Path, cancel: Option<&AtomicBool>) -> Result<Recovered, String> {
    let (stem, ext) = owned_name(path).ok_or(NOT_OURS)?;
    if in_use(path) {
        return Err(IN_USE.into());
    }
    let folder = path.parent().unwrap_or(Path::new(""));
    let cant_write = format!(
        "Backsight couldn't write the recovered video in {}. Check that the drive has free space, then try again. The unfinished recording is left as it was.",
        folder.display()
    );
    let original_bytes = match fs::metadata(path) {
        Ok(m) if m.len() == 0 => {
            set_aside(path, folder, &stem, &ext);
            return Err(EMPTY.into());
        }
        Ok(m) => m.len(),
        Err(e) => return Err(user_error(GONE, &e)),
    };
    let _watch = Watch::start(Step::Recover { bytes: original_bytes });
    let muxer = Catalog::get().containers.iter().find(|c| c.ext.eq_ignore_ascii_case(&ext)).ok_or(NOT_OURS)?;

    // An earlier attempt cut off by a crash left its unpublished copy, named in this mark: only
    // that file goes (it may be `(2)` or later when a clip had the name), or each crash adds one.
    let mark = folder.join(format!("{stem}.{ext}{RECOVERING_PART}"));
    if let Ok(old) = fs::read_to_string(&mark) {
        let old = PathBuf::from(old);
        let ours = old.parent() == Some(folder) && old.to_str().is_some_and(|s| s.ends_with(&format!(".{ext}.part")));
        if ours && old.is_file() && !in_use(&old) && fs::remove_file(&old).is_ok() {
            tracing::info!("removed {}, left by a recovery that was cut off", old.display());
        }
    }
    // Reserve `<target>.part` without replacing anything, then copy into it.
    let (target, part) = loop {
        let target = free_name(folder, &stem, &ext, Some(path));
        let part = with_suffix(&target, ".part");
        match fs::OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(_) => break (target, part),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(user_error(&cant_write, &e)),
        }
    };
    let _ = fs::write(&mark, part.to_string_lossy().as_bytes());
    // Gone however this attempt ends; only a crash leaves it, for the next start.
    struct Unmark(PathBuf);
    impl Drop for Unmark {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _unmark = Unmark(mark);
    let copied = remux::copy(path, &part, &muxer.muxer, &BTreeMap::new(), &[], true, cancel);
    let checked = match copied {
        Ok(c) if c.packets > 0 => match media::probe(&part) {
            Ok(m) if m.duration_ms.is_some_and(|d| d > 0) && m.video_codec.is_some() => Ok(c),
            other => Err(format!("the copy doesn't play: {other:?}")),
        },
        Ok(c) => Err(format!("no packets: {c:?}")),
        Err(e) if e == remux::CANCELLED => {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
        // Only a drive that failed keeps the original for another try; bad data never fixes itself.
        Err(e) if e.starts_with(remux::CANT_WRITE) => {
            let _ = fs::remove_file(&part);
            return Err(user_error(&cant_write, &format!("{}: {e}", part.display())));
        }
        Err(e) => Err(e),
    };
    let copied = match checked {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&part); // our own unpublished copy
            // The original doesn't open, or nothing in it plays.
            set_aside(path, folder, &stem, &ext);
            return Err(user_error(NOTHING_PLAYABLE, &format!("{}: {e}", path.display())));
        }
    };
    if copied.refused > 0 {
        tracing::info!("recovering {}: left out {} packets the muxer refused", path.display(), copied.refused);
    }
    if let Some(why) = &copied.cut_off {
        tracing::info!("recovering {}: {why}", path.display());
    }
    let mut target = target;
    for _ in 0..100 {
        match backsight_encode::fsutil::rename_no_replace(&part, &target) {
            Ok(()) => break,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => target = free_name(folder, &stem, &ext, Some(path)),
            Err(e) => {
                let _ = fs::remove_file(&part);
                return Err(user_error(&cant_write, &e));
            }
        }
    }
    if part.exists() {
        let _ = fs::remove_file(&part);
        return Err(user_error(&cant_write, &"no free name"));
    }
    let kept = with_suffix(&folder.join(format!("{stem}.{ext}")), RECOVERED_PART);
    let original = match backsight_encode::fsutil::rename_no_replace(path, &kept) {
        Ok(()) => kept,
        Err(e) => {
            tracing::warn!("recovered {} but kept its name: {e}", path.display());
            path.to_path_buf()
        }
    };
    // A crash while adding markers leaves our own unfinished copy too; it holds nothing the
    // recovered file lacks and would push later saves to `(2)`.
    let chapters = with_suffix(&folder.join(format!("{stem}.{ext}")), ".chapters.part");
    if !in_use(&chapters) && fs::remove_file(&chapters).is_ok() {
        tracing::info!("removed {}", chapters.display());
    }
    tracing::info!("recovered {} as {} ({} packets)", original.display(), target.display(), copied.packets);
    Ok(Recovered { path: target, original, original_bytes })
}

/// Renames an original nothing can be saved from (its bytes untouched), so startup doesn't
/// report it again.
fn set_aside(path: &Path, folder: &Path, stem: &str, ext: &str) {
    let to = with_suffix(&folder.join(format!("{stem}.{ext}")), UNRECOVERABLE_PART);
    if let Err(e) = backsight_encode::fsutil::rename_no_replace(path, &to) {
        tracing::warn!("couldn't set {} aside: {e}", path.display());
    }
}

/// `(stem, ext)` of a name only Backsight writes for a recording in progress:
/// `<name> YYYY-MM-DD HH.MM.SS[ (n)].<a catalog extension>.recording.part`.
fn owned_name(path: &Path) -> Option<(String, String)> {
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = name.strip_suffix(RECORDING_PART)?.rsplit_once('.')?;
    let known = Catalog::get().containers.iter().any(|c| c.ext.eq_ignore_ascii_case(ext));
    (known && stamp_at(stem).is_some()).then(|| (stem.to_owned(), ext.to_owned()))
}

/// Where the ` YYYY-MM-DD HH.MM.SS` time stamp starts in a stem Backsight wrote, after a name.
fn stamp_at(stem: &str) -> Option<usize> {
    // A taken name gets " (2)", " (3)" …
    let base = match stem.strip_suffix(')').and_then(|s| s.rsplit_once(" (")) {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => stem,
    };
    const PATTERN: &[u8] = b" 0000-00-00 00.00.00";
    let at = base.len().checked_sub(PATTERN.len()).filter(|&at| at > 0)?;
    let fits =
        base.as_bytes()[at..].iter().zip(PATTERN).all(|(&c, &p)| if p == b'0' { c.is_ascii_digit() } else { c == p });
    fits.then_some(at)
}

/// Sorts names by their time stamp, then the whole name.
fn stamp_key(name: &str) -> (&str, &str) {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    (stamp_at(stem).map_or("", |at| &stem[at..]), name)
}

/// Another handle has it open: a recording being written. Windows refuses an open that shares
/// nothing while any other handle exists (ERROR_SHARING_VIOLATION).
fn in_use(path: &Path) -> bool {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .is_err_and(|e| e.raw_os_error() == Some(ERROR_SHARING_VIOLATION))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path);
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_saved_pop_up_says_where_the_damaged_originals_are() {
        let r = |name: &str, bytes: u64| Recovered {
            path: PathBuf::from("a.mp4"),
            original: PathBuf::from(format!("C:\\Clips\\{name}{RECOVERED_PART}")),
            original_bytes: bytes,
        };
        assert_eq!(kept_originals_note(&[]), None);
        assert_eq!(
            kept_originals_note(&[r("Backsight 2026-11-10 14.03.07.mp4", 3 << 29)]).unwrap(),
            "The damaged original (1.5 GB) was kept as Backsight 2026-11-10 14.03.07.mp4.recovered.part. Delete it once you've checked the clip."
        );
        let many = kept_originals_note(&[r("a.mp4", 200 << 20), r("b.mp4", 300 << 20)]).unwrap();
        assert_eq!(
            many,
            "The 2 damaged originals (500 MB together) were kept, each ending in .recovered.part. Delete them once you've checked the clips."
        );
    }

    #[test]
    fn only_backsight_recording_names_are_ours() {
        let ours = |n: &str| owned_name(Path::new(n));
        assert_eq!(
            ours("Backsight 2026-11-10 14.03.07.mp4.recording.part"),
            Some(("Backsight 2026-11-10 14.03.07".into(), "mp4".into()))
        );
        assert!(ours("Backsight 2026-11-10 14.03.07 (2).mkv.recording.part").is_some());
        assert_eq!(ours("Backsight 2026-11-10 14.03.07.mp4.part"), None, "a clip or another app's .part");
        assert_eq!(ours("holiday.mp4.recording.part"), None, "not a name Backsight gives");
        assert_eq!(
            ours("Tomb Raider 2026-11-10 14.03.07 (12).mkv.recording.part"),
            Some(("Tomb Raider 2026-11-10 14.03.07 (12)".into(), "mkv".into())),
            "named after the game"
        );
        assert_eq!(ours("2026-11-10 14.03.07.mp4.recording.part"), None, "no name before the time");
        assert_eq!(ours("Game 2026-11-10 14.03.mp4.recording.part"), None, "not a whole time");
        assert_eq!(ours("Game 2026-11-10 14.03.07 ().mp4.recording.part"), None, "not a number");
        let mut names =
            ["Zelda 2026-11-10 14.03.07.mp4", "Apex 2026-11-10 15.00.00.mp4", "Apex 2026-11-10 14.03.07 (2).mp4"];
        names.sort_by(|a, b| stamp_key(a).cmp(&stamp_key(b)));
        assert_eq!(
            names,
            ["Zelda 2026-11-10 14.03.07.mp4", "Apex 2026-11-10 14.03.07 (2).mp4", "Apex 2026-11-10 15.00.00.mp4"]
        );
        assert_eq!(ours("Backsight 2026-11-10 14.03.07.txt.recording.part"), None, "not a video format");
        assert_eq!(ours("Backsight 2026-11-10 14.03.07.mp4.recovered.part"), None, "already recovered");
    }

    #[test]
    fn only_marked_backsight_mkvs_are_unfinished_conversions() {
        let d = tempfile::tempdir().unwrap();
        let game = d.path().join("Apex");
        fs::create_dir(&game).unwrap();
        let touch = |p: PathBuf| {
            fs::write(&p, b"x").unwrap();
            p
        };
        let cut = touch(d.path().join("Backsight 2026-11-10 14.03.07.mkv"));
        mark_conversion(&cut);
        let half = touch(d.path().join("Backsight 2026-11-10 14.03.07.mp4.part"));
        let half_2 = touch(d.path().join("Backsight 2026-11-10 14.03.07 (2).mp4.part")); // a clip owned .mp4
        let clip = touch(d.path().join("Backsight 2026-11-10 14.03.07.mp4"));
        let in_game = touch(game.join("Apex 2026-11-10 13.00.00.mkv"));
        mark_conversion(&in_game);
        let unmarked = touch(d.path().join("Backsight 2026-11-10 15.00.00.mkv")); // converted, or never asked to
        let unmarked_part = touch(d.path().join("Backsight 2026-11-10 15.00.00.mp4.part"));
        let other = touch(d.path().join("Backsight 2026-11-10 14.03.07 extra.mp4.part"));
        touch(d.path().join("holiday.mkv"));
        mark_conversion(&d.path().join("holiday.mkv")); // not a name Backsight gives
        assert_eq!(unfinished_conversions(d.path()), [in_game.clone(), cut.clone()]);
        for kept in [&clip, &unmarked, &unmarked_part, &other, &cut, &half, &half_2] {
            assert_eq!(fs::read(kept).unwrap(), b"x", "{} is left alone", kept.display());
        }

        // The test MKV isn't a video, so the conversion fails: the mark goes afterwards, the MKV
        // and the file already named `<stem>.mp4` stay as they were. The `.part` the mark names
        // (left by an attempt cut off by quitting) is removed; the other one stays.
        fs::write(conversion_mark(&cut), half.to_str().unwrap()).unwrap();
        assert!(finish_conversion(&cut).is_err());
        assert!(!conversion_mark(&cut).exists());
        assert_eq!(fs::read(&cut).unwrap(), b"x");
        assert_eq!(fs::read(&clip).unwrap(), b"x");
        assert!(!half.exists(), "the cut-off attempt's leftover goes");
        assert_eq!(fs::read(&half_2).unwrap(), b"x");

        // A mark naming a file in another folder, or not a `.part`, removes nothing.
        fs::write(conversion_mark(&in_game), half_2.to_str().unwrap()).unwrap();
        assert!(finish_conversion(&in_game).is_err());
        fs::write(conversion_mark(&cut), clip.to_str().unwrap()).unwrap();
        assert!(finish_conversion(&cut).is_err());
        assert_eq!(fs::read(&half_2).unwrap(), b"x");
        assert_eq!(fs::read(&clip).unwrap(), b"x");
    }
}
