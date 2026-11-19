//! The media library: every saved video and screenshot, newest first, with the metadata,
//! thumbnails and hover previews the Library page shows. See `docs/architecture/library.md`.
//!
//! File IO and bookkeeping only; reading the files themselves is `backsight_encode::media`.

use backsight_encode::{Catalog, media};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Screenshot,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LibraryItem {
    pub path: PathBuf,
    pub name: String,
    pub kind: MediaKind,
    /// Lowercase, no dot.
    pub ext: String,
    pub bytes: u64,
    /// When the file was created (recorded or saved), unix ms.
    pub created_ms: u64,
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// The catalog's label, e.g. "H.264".
    pub video_codec: Option<String>,
    pub audio_tracks: Option<u32>,
    /// An image the WebView can show: the file itself for screenshots it can show, else a cached JPEG.
    pub thumbnail: Option<PathBuf>,
    /// A video with its captions file next to it (`clip.srt` for `clip.mp4`, ADR 0016).
    pub captions: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Library {
    pub items: Vec<LibraryItem>,
    pub video_dir: Option<PathBuf>,
    pub screenshot_dir: Option<PathBuf>,
}

/// What `cache/library.json` remembers about a file while its size and modified time hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cached {
    bytes: u64,
    modified_ms: u64,
    /// `None` when FFmpeg couldn't read the file; not retried until the file changes, except
    /// while it is new (see [`SETTLING_MS`]).
    info: Option<media::MediaInfo>,
    thumbnail_failed: bool,
}

/// A file saved this recently that couldn't be read or thumbnailed is tried again at every scan:
/// right after a save, antivirus or the save itself may still hold it.
const SETTLING_MS: u64 = 2 * 60 * 1000;

impl Cached {
    /// Whether this cache entry stands for `f` at `now_ms`, or the file must be read again.
    fn holds_for(&self, f: &Found, now_ms: u64) -> bool {
        let failed = self.thumbnail_failed || (self.info.is_none() && readable(f));
        self.bytes == f.bytes && self.modified_ms == f.modified_ms && !(failed && now_ms < f.modified_ms + SETTLING_MS)
    }
}

/// Whether FFmpeg should be able to read `f` (an HDR screenshot is not meant to be).
fn readable(f: &Found) -> bool {
    f.kind == MediaKind::Video || webview_shows(&f.ext)
}

/// One media file found on disk.
struct Found {
    path: PathBuf,
    kind: MediaKind,
    ext: String,
    bytes: u64,
    modified_ms: u64,
    created_ms: u64,
}

/// Thumbnails and previews together stay under this; previews go first, oldest first.
const CACHE_CAP_BYTES: u64 = 500 << 20;
const PREVIEW_FAILED: &str =
    "Backsight couldn't make a preview of this video. It may be damaged, or still being saved.";

/// One scan at a time: scans write the same cache file.
static SCAN: Mutex<()> = Mutex::new(());
/// One preview at a time: two requests for the same video would write the same file.
static PREVIEW: Mutex<()> = Mutex::new(());

fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn kind_of(ext: &str) -> Option<MediaKind> {
    let c = Catalog::get();
    if c.containers.iter().any(|k| k.ext == ext) {
        Some(MediaKind::Video)
    } else if c.screenshot_formats.iter().any(|f| f.ext == ext) {
        Some(MediaKind::Screenshot)
    } else {
        None
    }
}

/// Resolve a WebView-supplied path before opening or copying a Library file.
pub fn resolve_file(path: &Path, roots: &[Option<PathBuf>]) -> Result<PathBuf, String> {
    let real = fs::canonicalize(path).map_err(|_| "That file is gone. It may have been moved or deleted.")?;
    let ext = real.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if !real.is_file()
        || kind_of(&ext).is_none()
        || !roots.iter().flatten().filter_map(|d| fs::canonicalize(d).ok()).any(|d| real.starts_with(d))
    {
        return Err("That file isn't in your library.".into());
    }
    Ok(real)
}

// shortcut: treats the HDR format (JPEG XR) as the one a WebView can't show; give the catalog's
// screenshot formats their own flag when a format breaks that rule.
fn webview_shows(ext: &str) -> bool {
    Catalog::get().screenshot_formats.iter().any(|f| f.ext == ext && !f.hdr)
}

/// Every media file under `dir`, recursively. `.part` files (saves in progress) and anything
/// unreadable are skipped; symlinked folders are not followed.
fn walk(dir: &Path, seen: &mut HashSet<PathBuf>, out: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            walk(&path, seen, out);
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let (Some(kind), Ok(meta)) = (kind_of(&ext), entry.metadata()) else { continue };
        // The UI gets paths as strings, and the folders may overlap.
        if !meta.is_file() || path.to_str().is_none() || !seen.insert(path.clone()) {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        out.push(Found {
            path,
            kind,
            ext,
            bytes: meta.len(),
            modified_ms: unix_ms(modified),
            created_ms: unix_ms(meta.created().unwrap_or(modified)),
        });
    }
}

/// Name of a file's thumbnail and preview in the cache, changing when the file does.
fn cache_key(path: &Path, bytes: u64, modified_ms: u64) -> String {
    // FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let text = format!("{}|{bytes}|{modified_ms}", path.display());
    for b in text.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Probes a file and makes its thumbnail, unless the cache already has them.
fn refresh(f: &Found, old: Option<Cached>, thumb: &Path) -> Cached {
    let mut c = old.unwrap_or_else(|| {
        let info = readable(f)
            .then(|| media::probe(&f.path))
            .and_then(|r| r.inspect_err(|e| tracing::debug!("library: can't read {}: {e}", f.path.display())).ok());
        Cached { bytes: f.bytes, modified_ms: f.modified_ms, info, thumbnail_failed: false }
    });
    let made = match f.kind {
        MediaKind::Video if c.info.is_some() => Some(media::thumbnail as fn(&Path, &Path) -> Result<(), String>),
        MediaKind::Screenshot if !webview_shows(&f.ext) => Some(hdr_thumbnail as _),
        _ => None,
    };
    if let Some(make) = made
        && !c.thumbnail_failed
        && !thumb.exists()
        && let Err(e) = make(&f.path, thumb)
    {
        tracing::warn!("library: no thumbnail for {}: {e}", f.path.display());
        c.thumbnail_failed = true;
    }
    c
}

/// A JPEG of an HDR (JPEG XR) screenshot, which the WebView can't show, tone-mapped the way
/// the screenshot picker and clipboard show it.
fn hdr_thumbnail(src: &Path, dst: &Path) -> Result<(), String> {
    // As big as video thumbnails.
    let (half, w, h) = backsight_encode::screenshot::decode_jxr(src, (480, 480)).map_err(|e| e.to_string())?;
    let jpeg = Catalog::get().screenshot_formats.iter().find(|f| f.id == "jpeg").ok_or("no JPEG format")?;
    let bytes = backsight_encode::screenshot::encode(&crate::screenshot::sdr_preview(&half), w, h, jpeg, 80)?;
    let tmp = dst.with_extension("tmp");
    fs::write(&tmp, bytes)
        .and_then(|()| fs::rename(&tmp, dst))
        .map_err(|e| format!("can't write {}: {e}", dst.display()))
}

fn item(f: Found, c: &Cached, thumb: PathBuf) -> LibraryItem {
    let info = c.info.clone().unwrap_or_default();
    let video = f.kind == MediaKind::Video;
    let codec = info.video_codec.filter(|_| video).map(|name| {
        let known = Catalog::get().video_codecs.iter().find(|v| v.id == name);
        known.map_or_else(|| name.to_uppercase(), |v| v.label.clone())
    });
    let thumbnail = match f.kind {
        MediaKind::Screenshot if webview_shows(&f.ext) => Some(f.path.clone()),
        _ => thumb.exists().then_some(thumb),
    };
    LibraryItem {
        name: f.path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        kind: f.kind,
        bytes: f.bytes,
        created_ms: f.created_ms,
        duration_ms: info.duration_ms.filter(|_| video),
        width: info.width,
        height: info.height,
        fps: info.fps.filter(|_| video),
        video_codec: codec,
        audio_tracks: video.then_some(info.audio_tracks).filter(|_| c.info.is_some()),
        thumbnail,
        captions: video && f.path.with_extension("srt").is_file(),
        ext: f.ext,
        path: f.path,
    }
}

/// Lists every video and screenshot under the two folders, newest first. A missing folder
/// gives no items and leaves the cache untouched (it may be an unplugged drive). Metadata and thumbnails are cached under `cache_dir`, so a rescan only
/// reads files that are new or changed.
pub fn scan(video_dir: Option<&Path>, screenshot_dir: Option<&Path>, cache_dir: &Path) -> Library {
    let _guard = SCAN.lock().unwrap_or_else(|e| e.into_inner());
    let video_dir = video_dir.and_then(|d| std::path::absolute(d).ok());
    let screenshot_dir = screenshot_dir.and_then(|d| std::path::absolute(d).ok());
    let (mut found, mut seen) = (Vec::new(), HashSet::new());
    for dir in [&video_dir, &screenshot_dir].into_iter().flatten() {
        walk(dir, &mut seen, &mut found);
    }

    let index = cache_dir.join("library.json");
    let media_dir = cache_dir.join("library");
    let _ = fs::create_dir_all(&media_dir);
    // A missing or broken cache is rebuilt.
    let old_text = fs::read_to_string(&index).unwrap_or_default();
    let mut old: BTreeMap<PathBuf, Cached> = serde_json::from_str(&old_text).unwrap_or_default();
    let now_ms = unix_ms(SystemTime::now());
    let jobs: Vec<(Found, Option<Cached>, PathBuf)> = found
        .into_iter()
        .map(|f| {
            let cached = old.remove(&f.path).filter(|c| c.holds_for(&f, now_ms));
            let thumb = media_dir.join(format!("{}.jpg", cache_key(&f.path, f.bytes, f.modified_ms)));
            (f, cached, thumb)
        })
        .collect();

    // New files are probed in parallel; each FFmpeg decoder uses a few threads of its own.
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 4);
    let mut done: Vec<Cached> = Vec::with_capacity(jobs.len());
    std::thread::scope(|s| {
        let chunk = jobs.len().div_ceil(workers).max(1);
        let handles: Vec<_> = jobs
            .chunks(chunk)
            .map(|part| s.spawn(move || part.iter().map(|(f, c, t)| refresh(f, c.clone(), t)).collect::<Vec<_>>()))
            .collect();
        for h in handles {
            done.extend(h.join().expect("library worker panicked"));
        }
    });

    let mut cache = BTreeMap::new();
    let mut keep = HashSet::new();
    let mut items = Vec::with_capacity(jobs.len());
    for ((f, _, thumb), c) in jobs.into_iter().zip(done) {
        if f.kind == MediaKind::Video || !webview_shows(&f.ext) {
            keep.insert(cache_key(&f.path, f.bytes, f.modified_ms));
        }
        cache.insert(f.path.clone(), c.clone());
        items.push(item(f, &c, thumb));
    }
    // An offline drive looks like an empty folder: keep its cache for when it comes back.
    let offline = [&video_dir, &screenshot_dir].into_iter().flatten().any(|d| !d.is_dir());
    let text = serde_json::to_string(&cache).expect("plain data");
    if offline {
        tracing::info!("library: a folder is missing, keeping the cache as it is");
    } else if text != old_text {
        // Write then rename: a crash never leaves half a cache (which would only cost a rescan).
        let tmp = index.with_extension("json.tmp");
        if fs::write(&tmp, &text).and_then(|()| fs::rename(&tmp, &index)).is_err() {
            tracing::warn!("library: can't write {}", index.display());
        }
    }
    if !offline {
        prune(&media_dir, &keep);
    }

    items.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then_with(|| a.path.cmp(&b.path)));
    Library { items, video_dir, screenshot_dir }
}

/// Deletes thumbnails and previews of files that are gone or changed, then the oldest previews
/// (and after them thumbnails) until the folder is under [`CACHE_CAP_BYTES`].
fn prune(media_dir: &Path, keep: &HashSet<String>) {
    let Ok(entries) = fs::read_dir(media_dir) else { return };
    // (is a thumbnail, modified, size, path): previews sort first, oldest first.
    let mut kept = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        // Only finished files: a preview being written is `<key>.mp4.part`.
        let (Some(ext), Some(stem)) = (p.extension().and_then(|x| x.to_str()), p.file_stem().and_then(|x| x.to_str()))
        else {
            continue;
        };
        if ext == "part" || ext == "tmp" {
            // Left by a crash. One being written right now is minutes old at most.
            let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if age.is_some_and(|a| a > Duration::from_secs(3600)) {
                let _ = fs::remove_file(&p);
            }
            continue;
        }
        if ext != "jpg" && ext != "mp4" {
            continue;
        }
        if !keep.contains(stem) {
            let _ = fs::remove_file(&p);
        } else if let Ok(m) = e.metadata() {
            kept.push((ext == "jpg", m.modified().unwrap_or(UNIX_EPOCH), m.len(), p));
        }
    }
    kept.sort();
    let mut total: u64 = kept.iter().map(|k| k.2).sum();
    for (_, _, len, p) in kept {
        if total <= CACHE_CAP_BYTES {
            break;
        }
        if fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

/// The hover preview of a video, made on first request (a second or two) and cached.
pub fn preview(path: &Path, resolved: &Path, cache_dir: &Path) -> Result<PathBuf, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if kind_of(&ext) != Some(MediaKind::Video) || path.components().any(|c| c == Component::ParentDir) {
        return Err("That file isn't a video in your library.".into());
    }
    let meta = fs::metadata(resolved).map_err(|e| {
        tracing::warn!("library: preview of {}: {e}", path.display());
        "That video is gone. It may have been moved or deleted.".to_string()
    })?;
    let key = cache_key(path, meta.len(), unix_ms(meta.modified().unwrap_or(UNIX_EPOCH)));
    let dst = cache_dir.join("library").join(format!("{key}.mp4"));
    let _guard = PREVIEW.lock().unwrap_or_else(|e| e.into_inner());
    if !dst.exists() {
        let _ = fs::create_dir_all(cache_dir.join("library"));
        let started = std::time::Instant::now();
        media::preview(resolved, &dst).map_err(|e| {
            tracing::warn!("library: no preview for {}: {e}", path.display());
            PREVIEW_FAILED.to_string()
        })?;
        tracing::debug!("library: preview of {} took {:?}", path.display(), started.elapsed());
    }
    Ok(dst)
}

/// `%APPDATA%\Backsight\cache`.
pub fn cache_dir() -> Result<PathBuf, String> {
    backsight_config::app_dir().map(|d| d.join("cache")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_changes_with_the_file() {
        let p = Path::new(r"C:\Videos\a.mp4");
        assert_eq!(cache_key(p, 1, 2), cache_key(p, 1, 2));
        assert_ne!(cache_key(p, 1, 2), cache_key(p, 1, 3));
        assert_ne!(cache_key(p, 1, 2), cache_key(Path::new(r"C:\Videos\b.mp4"), 1, 2));
        assert_eq!(cache_key(p, 1, 2).len(), 16);
    }

    #[test]
    fn a_new_file_that_failed_is_read_again() {
        let f = |ext: &str, kind| Found {
            path: PathBuf::from(format!(r"C:\Videos.{ext}")),
            kind,
            ext: ext.into(),
            bytes: 10,
            modified_ms: 1_000_000,
            created_ms: 1_000_000,
        };
        let (video, hdr_shot) = (f("mp4", MediaKind::Video), f("jxr", MediaKind::Screenshot));
        let read =
            Cached { bytes: 10, modified_ms: 1_000_000, info: Some(Default::default()), thumbnail_failed: false };
        let unreadable = Cached { info: None, ..read.clone() };
        let no_thumb = Cached { thumbnail_failed: true, ..read.clone() };
        let (new, old) = (1_000_000 + 5_000, 1_000_000 + SETTLING_MS);
        assert!(read.holds_for(&video, new), "a good entry is kept");
        assert!(!unreadable.holds_for(&video, new) && !no_thumb.holds_for(&video, new), "retried while new");
        assert!(unreadable.holds_for(&video, old) && no_thumb.holds_for(&video, old), "then given up on");
        assert!(unreadable.holds_for(&hdr_shot, new), "not meant to be read");
        assert!(!Cached { bytes: 11, ..read.clone() }.holds_for(&video, old), "the file changed");
    }

    #[test]
    fn an_hdr_screenshot_gets_a_jpeg_thumbnail() {
        let root = tempfile::tempdir().unwrap();
        let (shots, cache) = (root.path().join("shots"), root.path().join("cache"));
        fs::create_dir(&shots).unwrap();
        // 4.0 is brighter than SDR white: the roll-off must keep it from turning grey or failing.
        let bright: Vec<u8> = [0x4400u16, 0x3c00, 0, 0x3c00].into_iter().flat_map(u16::to_le_bytes).collect();
        let jxr = backsight_encode::screenshot::encode_jxr(&bright.repeat(64 * 32), 64, 32).unwrap();
        fs::write(shots.join("Game 2026-11-18 12.00.00.jxr"), jxr).unwrap();
        let lib = scan(None, Some(&shots), &cache);
        let thumb = lib.items[0].thumbnail.clone().expect("a thumbnail");
        assert_eq!(thumb.extension().unwrap(), "jpg");
        assert!(fs::read(&thumb).unwrap().starts_with(&[0xff, 0xd8, 0xff]), "a JPEG");
        scan(None, Some(&shots), &cache);
        assert!(thumb.exists(), "kept by the next scan");
    }

    #[test]
    fn extensions_come_from_the_catalog() {
        assert_eq!(kind_of("mkv"), Some(MediaKind::Video));
        assert_eq!(kind_of("png"), Some(MediaKind::Screenshot));
        assert_eq!(kind_of("part"), None);
        assert!(webview_shows("jpg") && !webview_shows("jxr"));
    }

    #[test]
    fn file_commands_stay_inside_the_library_and_require_media() {
        let root = tempfile::tempdir().unwrap();
        let clips = root.path().join("clips");
        fs::create_dir(&clips).unwrap();
        let roots = [Some(clips.clone())];
        for path in [clips.join("clip.mp4"), clips.join("other.txt"), root.path().join("outside.mp4")] {
            fs::write(path, []).unwrap();
        }
        assert!(resolve_file(&clips.join("clip.mp4"), &roots).is_ok());
        assert!(resolve_file(&clips.join("../outside.mp4"), &roots).is_err());
        assert!(resolve_file(&clips.join("other.txt"), &roots).is_err());
        assert!(resolve_file(&clips.join("gone.mp4"), &roots).is_err());
    }

    #[test]
    fn a_missing_folder_keeps_the_cache() {
        let cache = std::env::temp_dir().join(format!("backsight-lib-offline-{}", std::process::id()));
        let media = cache.join("library");
        fs::create_dir_all(&media).unwrap();
        let thumb = media.join("0123456789abcdef.jpg");
        fs::write(&thumb, b"jpg").unwrap();
        fs::write(cache.join("library.json"), r#"{"X:\\gone.mp4":null}"#).unwrap();
        let offline = cache.join("unplugged-drive");
        let lib = scan(Some(&offline), None, &cache);
        assert!(lib.items.is_empty());
        assert!(thumb.exists(), "thumbnail kept");
        assert_eq!(fs::read_to_string(cache.join("library.json")).unwrap(), r#"{"X:\\gone.mp4":null}"#);
        // Back online (here: an empty folder that exists), the stale entries are cleaned up.
        fs::create_dir_all(&offline).unwrap();
        scan(Some(&offline), None, &cache);
        assert!(!thumb.exists(), "pruned once the folder is there");
        let _ = fs::remove_dir_all(&cache);
    }

    #[test]
    fn preview_refuses_files_outside_the_library() {
        let cache = std::env::temp_dir();
        for path in [Path::new(r"C:\Windows\win.ini"), Path::new(r"C:\Videos\..\secret.mp4")] {
            assert!(preview(path, path, &cache).is_err());
        }
    }
}
