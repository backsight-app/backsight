//! Vertical clips after a save (ADR 0015): which files, the background job, and picking up jobs
//! cut off by quitting or a crash. The picture work is `backsight_encode::vertical`.

use crate::{recovery, system};
use backsight_config::Settings;
use backsight_encode::{
    Catalog,
    burn::Captions,
    catalog::resolve,
    fsutil::{publish, rename_no_replace},
    vertical::make_vertical,
};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::Instant,
};

/// The `replay.clip_shape` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Wide,
    Vertical,
    Both,
}

impl Shape {
    /// Anything unknown (a hand-edited file) is Wide, which makes nothing extra.
    pub fn from_setting(value: &str) -> Self {
        match value {
            "vertical" => Shape::Vertical,
            "both" => Shape::Both,
            _ => Shape::Wide,
        }
    }
}

const SUFFIX: &str = " (vertical)";
/// With Vertical, the wide clip waits under `<vertical name>.source.part` until the job is done.
const SOURCE_PART: &str = ".source.part";
const KEPT_WIDE: &str = "Backsight couldn't make the vertical clip, so it kept the clip wide. Try another encoder on Backsight's Video page.";
const ALSO_WIDE: &str =
    "Backsight couldn't make the vertical clip. The wide clip is saved. Try another encoder on Backsight's Video page.";

/// `<stem> (vertical).<ext>` next to the wide clip.
pub fn vertical_path(wide: &Path) -> PathBuf {
    let mut name = wide.file_stem().unwrap_or_default().to_owned();
    name.push(SUFFIX);
    if let Some(ext) = wide.extension() {
        name.push(".");
        name.push(ext);
    }
    wide.with_file_name(name)
}

/// The wide clip a vertical clip's name was made from.
fn wide_of(vertical: &Path) -> Option<PathBuf> {
    let stem = vertical.file_stem()?.to_str()?.strip_suffix(SUFFIX)?;
    let ext = vertical.extension()?.to_str()?;
    Some(vertical.with_file_name(format!("{stem}.{ext}")))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn source_part(wide: &Path) -> PathBuf {
    with_suffix(&vertical_path(wide), SOURCE_PART)
}

/// One clip whatever the case of its name or its separators: Windows file names ignore them.
fn same(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect::<Vec<_>>();
    key(a) == key(b)
}

/// A clip's captions file, as `backsight_captions` names it.
fn srt_of(clip: &Path) -> PathBuf {
    clip.with_extension("srt")
}

/// One job in the record of jobs not finished yet. `tries`: how often it was started and Backsight
/// then ended without quitting (a crash, maybe inside FFmpeg on this very clip). `made`: the
/// vertical clip saved, and whether captions are burned into it, written down before tidying up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Entry {
    wide: PathBuf,
    keep_wide: bool,
    tries: u32,
    #[serde(default)]
    made: Option<(PathBuf, bool)>,
}

/// `%APPDATA%\Backsight\vertical-jobs.json`. Tests share one file of their own.
fn record_path() -> Option<PathBuf> {
    if cfg!(test) {
        return Some(std::env::temp_dir().join(format!("backsight-vertical-record-{}.json", std::process::id())));
    }
    backsight_config::app_dir().ok().map(|d| d.join("vertical-jobs.json"))
}

/// Changes the record of jobs not finished yet, which finds them again at the next start wherever
/// their clip is (the video folder may have been changed since). A missing record is empty. One
/// that can't be read or is damaged is logged and left as it is (`change` gets an empty one), and
/// one that can't be saved is logged: the folder scan still finds most jobs.
fn edit<T>(change: impl FnOnce(&mut Vec<Entry>) -> T) -> T {
    edit_at(record_path(), change)
}

fn edit_at<T>(mut path: Option<PathBuf>, change: impl FnOnce(&mut Vec<Entry>) -> T) -> T {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let read = path.as_deref().map(|p| match fs::read(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        read => {
            read.and_then(|b| serde_json::from_slice(&b).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)))
        }
    });
    let mut entries = match read {
        Some(Ok(entries)) => entries,
        // A damaged record is set aside and a new one begun, so crash counting keeps working; one
        // that merely can't be read now (antivirus holding it) is left alone.
        Some(Err(e)) if e.kind() == io::ErrorKind::InvalidData => {
            let p = path.as_deref().unwrap_or(Path::new(""));
            let aside = p.with_extension("json.damaged");
            tracing::warn!("{} is damaged ({e}); kept as {} and started afresh", p.display(), aside.display());
            if fs::rename(p, &aside).is_err() {
                path = None;
            }
            Vec::new()
        }
        Some(Err(e)) => {
            let left = path.take().unwrap_or_default(); // not inside warn!, which may not run its arguments
            tracing::warn!("{} left as it is, it can't be read: {e}", left.display());
            Vec::new()
        }
        None => Vec::new(),
    };
    let before = entries.clone();
    let out = change(&mut entries);
    if entries != before
        && let Some(path) = path
    {
        // Flushed to disk before the rename, so a power cut never leaves an empty record.
        let part = with_suffix(&path, ".part");
        let saved = serde_json::to_vec(&entries)
            .map_err(io::Error::other)
            .and_then(|bytes| {
                let mut file = fs::File::create(&part)?;
                io::Write::write_all(&mut file, &bytes)?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&part, &path));
        if let Err(e) = saved {
            tracing::warn!("couldn't save {}: {e}", path.display());
        }
    }
    out
}

fn forget(wide: &Path) {
    edit(|entries| entries.retain(|e| !same(&e.wide, wide)));
}

/// The wide clip of the job being made right now, and whether Backsight is quitting.
struct Running {
    wide: Option<PathBuf>,
    stopping: bool,
}

static RUNNING: Mutex<Running> = Mutex::new(Running { wide: None, stopping: false });

fn running() -> std::sync::MutexGuard<'static, Running> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Backsight is quitting or Windows is shutting down: the job being made is cut off on purpose,
/// so the next start makes it again instead of giving up on it, and no other job starts.
pub fn stopping() {
    let mut running = running();
    running.stopping = true;
    if let Some(wide) = running.wide.take() {
        not_cut_off(&wide);
    }
}

/// A job that would start while Backsight is quitting: it stays recorded for the next start.
const LATER: &str = "Backsight is closing. It makes the vertical clip when it starts again.";

fn not_cut_off(wide: &Path) {
    edit(|entries| entries.iter_mut().filter(|e| same(&e.wide, wide)).for_each(|e| e.tries = 0));
}

/// Making the vertical clip of one wide clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    wide: PathBuf,
    /// Both: the wide clip stays. Vertical: it waits under its [`source_part`] name.
    keep_wide: bool,
    /// Found at start, left by quitting or a crash.
    resumed: bool,
    /// It was being made when Backsight ended without quitting: it isn't tried again.
    cut_off: bool,
    /// It was saved just before Backsight ended (see [`Entry`]): only the tidying up is left.
    made: Option<(PathBuf, bool)>,
}

impl Job {
    fn new(wide: &Path, keep_wide: bool) -> Job {
        Job { wide: wide.to_path_buf(), keep_wide, resumed: false, cut_off: false, made: None }
    }

    /// The job for a clip just saved at `wide`; `None` for Wide. With Vertical the wide clip is moved
    /// out of the Library's sight first (renamed, never deleted), so only the vertical one shows.
    pub fn after_save(wide: &Path, shape: Shape) -> Option<Job> {
        let keep_wide = match shape {
            Shape::Wide => return None,
            Shape::Both => true,
            Shape::Vertical => false,
        };
        let job = match (!keep_wide).then(|| rename_no_replace(wide, &source_part(wide))) {
            Some(Err(e)) => {
                tracing::warn!("{} stays as it is, the vertical clip comes next to it: {e}", wide.display());
                Job::new(wide, true)
            }
            _ => Job::new(wide, keep_wide),
        };
        edit(|entries| {
            entries.retain(|e| !same(&e.wide, &job.wide));
            entries.push(Entry { wide: job.wide.clone(), keep_wide: job.keep_wide, tries: 0, made: None });
        });
        Some(job)
    }

    /// The clip the vertical one is made from: the wide clip, or its working file.
    pub fn source(&self) -> PathBuf {
        if self.keep_wide { self.wide.clone() } else { source_part(&self.wide) }
    }

    /// Makes the vertical clip with the video encoder in `settings` and returns it. Jobs run one at a
    /// time, in background mode, and take as long as they take. `captions` gets the source clip
    /// and returns its captions file, if any, to burn into the picture. With Vertical, the waiting
    /// wide clip is removed once the vertical one is saved, or put back under its own name if that
    /// failed. The error is a message for the user.
    pub fn run(&self, settings: &Settings, captions: impl FnOnce(&Path) -> Option<PathBuf>) -> Result<PathBuf, String> {
        if running().stopping {
            return Err(LATER.into());
        }
        let (source, vertical) = (self.source(), vertical_path(&self.wide));
        // Only the file written down counts: one that just has the name may be another clip.
        if let Some((made, burned)) = self.made.as_ref().filter(|(made, _)| self.resumed && made.is_file()) {
            tracing::info!("{} was already made", made.display());
            if self.keep_wide {
                captions(&source); // the wide clip keeps its own captions file
            }
            self.tidy(&source, made, true, *burned);
            return Ok(made.clone());
        }
        let name = self.wide.file_name().unwrap_or_default().to_string_lossy();
        if self.cut_off {
            if self.keep_wide {
                captions(&source);
            }
            tracing::warn!("no vertical clip of {}: Backsight ended while making it last time", self.wide.display());
            let why = format!("Backsight closed unexpectedly while making the vertical clip of \"{name}\" last time");
            let (kept, also) = (format!("{why}, so it kept the clip wide."), format!("{why}. The wide clip is saved."));
            return Err(self.failed(&source, kept, also));
        }
        let srt = captions(&source);
        let mut burn = srt
            .as_deref()
            .and_then(|s| Captions::load(s).map_err(|e| tracing::warn!("the vertical clip has no captions: {e}")).ok());

        static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let _background = system::background_mode();
        {
            // One step with `stopping`: quitting began either before this job (it waits for the
            // next start, untouched) or after it was counted as started (and is uncounted then).
            let mut running = running();
            if running.stopping {
                return Err(LATER.into());
            }
            running.wide = Some(self.wide.clone());
            edit(|entries| match entries.iter_mut().find(|e| same(&e.wide, &self.wide)) {
                Some(e) => e.tries += 1,
                None => {
                    entries.push(Entry { wide: self.wide.clone(), keep_wide: self.keep_wide, tries: 1, made: None })
                }
            });
        }
        let started = Instant::now();
        let made = make(&source, &vertical, settings, burn.as_mut());
        // Counted as running until the outcome is written down, so quitting in between is no crash.
        let done = || running().wide.take();
        match made {
            Ok(made) => {
                tracing::info!("made {} in {:.1?}", made.display(), started.elapsed());
                let burned = burn.as_ref().is_some_and(Captions::burned);
                let written = |e: &mut Entry| e.made = Some((made.clone(), burned));
                edit(|entries| entries.iter_mut().filter(|e| same(&e.wide, &self.wide)).for_each(written));
                done();
                self.tidy(&source, &made, true, burned);
                Ok(made)
            }
            Err(e) => {
                tracing::warn!("no vertical clip of {}: {e}", self.wide.display());
                done();
                Err(self.failed(&source, KEPT_WIDE.into(), ALSO_WIDE.into()))
            }
        }
    }

    /// The job is over: forgets it and, with Vertical, removes the working file once the vertical
    /// clip is `made`. The working file's captions are removed when `burned` into the picture, else
    /// they go with `kept`, the clip that is kept.
    fn tidy(&self, source: &Path, kept: &Path, made: bool, burned: bool) {
        if !self.keep_wide {
            self.tidy_files(source, kept, made, burned);
        }
        // Last: until then the record still says what's left to tidy at the next start.
        forget(&self.wide);
    }

    fn tidy_files(&self, source: &Path, kept: &Path, made: bool, burned: bool) {
        let srt = srt_of(source);
        if srt.is_file() {
            let moved = if burned { fs::remove_file(&srt) } else { rename_no_replace(&srt, &srt_of(kept)) };
            if let Err(e) = moved {
                tracing::warn!("couldn't tidy up {}: {e}", srt.display());
            }
        }
        if made && let Err(e) = fs::remove_file(source) {
            tracing::warn!("couldn't remove {}: {e}", source.display());
        }
    }

    /// No vertical clip: with Vertical the working file gets its own name back, so footage is never
    /// lost. Returns the message for the user: `kept` (Vertical) or `also` (Both).
    fn failed(&self, source: &Path, kept: String, also: String) -> String {
        let _ = fs::remove_file(with_suffix(&vertical_path(&self.wide), ".part"));
        if self.keep_wide {
            forget(&self.wide);
            return also;
        }
        match publish(source, &self.wide) {
            Ok(restored) => {
                self.tidy(source, &restored, false, false);
                kept
            }
            Err(e) => {
                forget(&self.wide);
                tracing::warn!("couldn't rename {} back: {e}", source.display());
                let name = source.file_name().unwrap_or_default().to_string_lossy();
                format!(
                    "Backsight couldn't make the vertical clip. Your clip is kept as \"{name}\": remove \"{SOURCE_PART}\" from its name to play it."
                )
            }
        }
    }
}

fn make(
    source: &Path,
    vertical: &Path,
    settings: &Settings,
    captions: Option<&mut Captions>,
) -> Result<PathBuf, String> {
    let c = Catalog::get();
    let venc = c.video_encoder(&settings.video.encoder).ok_or("unknown video encoder")?;
    let ext = vertical.extension().and_then(|e| e.to_str()).unwrap_or_default();
    // The clip's own format: the chosen one, unless it was changed since (an older clip's job).
    let container = c
        .container(&settings.video.container)
        .filter(|k| k.ext.eq_ignore_ascii_case(ext))
        .or_else(|| c.containers.iter().find(|k| k.ext.eq_ignore_ascii_case(ext)))
        .ok_or("no file format for this clip")?;
    let values = resolve(&c.video_options(venc), settings.video.encoder_options.get(&venc.id));
    // shortcut: x264's own threads run at normal priority; move the encode into a below-normal child process if a slow PC shows stutter.
    make_vertical(source, vertical, &Catalog::clip_container(container), venc, &values, captions)
}

/// The jobs cut off by quitting or a crash: those in the record (wherever their clip is) and those
/// found in `folder` and its game folders by what they left: a waiting wide clip, or an unfinished
/// vertical clip next to its wide one. Call it at start, before any job runs. An unfinished
/// vertical clip whose wide clip is gone is removed.
pub fn unfinished(folder: &Path) -> Vec<Job> {
    let mut jobs: Vec<Job> = recovery::files(folder)
        .into_iter()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if let Some(vertical) = name.strip_suffix(SOURCE_PART) {
                return wide_of(&path.with_file_name(vertical)).map(|wide| Job::new(&wide, false));
            }
            let wide = wide_of(&path.with_file_name(name.strip_suffix(".part")?))?;
            if source_part(&wide).exists() {
                return None; // found as a waiting wide clip
            }
            if wide.is_file() {
                return Some(Job::new(&wide, true));
            }
            let _ = fs::remove_file(&path);
            None
        })
        .collect();
    let there = |e: &Entry| if e.keep_wide { e.wide.is_file() } else { source_part(&e.wide).is_file() };
    edit(|entries| {
        // A clip on a drive that isn't plugged in now stays in the record for a later start.
        entries.retain(|e| {
            let gone = !there(e) && e.wide.parent().is_some_and(Path::is_dir);
            if gone {
                let _ = fs::remove_file(with_suffix(&vertical_path(&e.wide), ".part"));
            }
            !gone
        });
        for e in entries.iter().filter(|e| there(e)) {
            match jobs.iter_mut().find(|j| same(&j.wide, &e.wide)) {
                Some(job) => (job.cut_off, job.made) = (e.tries > 0, e.made.clone()),
                None => jobs.push(Job { cut_off: e.tries > 0, made: e.made.clone(), ..Job::new(&e.wide, e.keep_wide) }),
            }
        }
        for job in &mut jobs {
            job.resumed = true;
            if !entries.iter().any(|e| same(&e.wide, &job.wide)) {
                entries.push(Entry { wide: job.wide.clone(), keep_wide: job.keep_wide, tries: 0, made: None });
            }
        }
    });
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;
    use backsight_encode::{
        mux::{Muxer, Track, needs_global_header},
        session::VideoSession,
    };

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("backsight-vertical-jobs-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A 1 s, 640×360 x264 clip as clips are saved.
    fn clip(path: &Path) {
        clip_of(path, 30);
    }

    /// [`clip`], `frames` long at 30 fps.
    fn clip_of(path: &Path, frames: i64) {
        let c = Catalog::get();
        let container = Catalog::clip_container(c.container("mp4").unwrap());
        let venc = c.video_encoder("x264").unwrap();
        let gh = needs_global_header(&container);
        let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 640, 360, 30, gh).unwrap();
        let mut mux = Muxer::create(path, &container, &video.params(), &[]).unwrap();
        for i in 0..frames {
            for (data, _) in video.planes_mut().unwrap().into_iter().flatten() {
                data.fill((i * 8 % 256) as u8);
            }
            video.send(i, &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        }
        video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        mux.finish().unwrap();
    }

    fn x264() -> Settings {
        let mut s = Settings::default();
        s.video.encoder = "x264".into();
        s
    }

    /// The jobs `unfinished` finds whose clip is in `d` (tests share one record).
    fn unfinished_in(scan: &Path, d: &Path) -> Vec<Job> {
        let mut jobs: Vec<_> = unfinished(scan).into_iter().filter(|j| j.wide.starts_with(d)).collect();
        jobs.sort_by(|x, y| x.wide.file_name().cmp(&y.wide.file_name()));
        jobs
    }

    fn files(d: &Path) -> Vec<PathBuf> {
        let mut left: Vec<_> = fs::read_dir(d).unwrap().flatten().map(|e| e.path()).collect();
        left.sort();
        left
    }

    /// Tests that run jobs take turns: one of them makes Backsight look like it is quitting.
    fn one_run_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        static RUNS: Mutex<()> = Mutex::new(());
        RUNS.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn resumed(wide: PathBuf, keep_wide: bool, cut_off: bool) -> Job {
        Job { wide, keep_wide, resumed: true, cut_off, made: None }
    }

    #[test]
    fn a_damaged_record_is_set_aside_and_a_new_one_begun() {
        let d = dir("record");
        let record = d.join("vertical-jobs.json");
        let entry = Entry { wide: d.join("a.mp4"), keep_wide: true, tries: 0, made: None };
        edit_at(Some(record.clone()), |e| e.push(entry.clone()));
        assert_eq!(edit_at(Some(record.clone()), |e| e.clone()), vec![entry.clone()]);
        assert_eq!(files(&d), vec![record.clone()], "no .part left");
        fs::write(&record, b"{ not json").unwrap();
        edit_at(Some(record.clone()), |e| {
            assert!(e.is_empty());
            e.push(entry.clone());
        });
        assert_eq!(fs::read(d.join("vertical-jobs.json.damaged")).unwrap(), b"{ not json", "kept aside");
        assert_eq!(edit_at(Some(record.clone()), |e| e.clone()), vec![entry], "crash counting goes on");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_record_that_cant_be_read_now_is_left_alone() {
        let d = dir("record-unreadable");
        let record = d.join("vertical-jobs.json");
        fs::create_dir(&record).unwrap(); // reading it fails, but not because it's missing or damaged
        let entry = Entry { wide: d.join("a.mp4"), keep_wide: true, tries: 0, made: None };
        edit_at(Some(record.clone()), |e| e.push(entry));
        assert!(record.is_dir(), "not set aside");
        assert_eq!(files(&d), vec![record], "nothing written over it");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn shapes_and_names() {
        assert_eq!(Shape::from_setting("wide"), Shape::Wide);
        assert_eq!(Shape::from_setting("vertical"), Shape::Vertical);
        assert_eq!(Shape::from_setting("both"), Shape::Both);
        assert_eq!(Shape::from_setting("Tall?"), Shape::Wide);
        let wide = Path::new(r"C:\Videos\Game 2026-11-17 14.03.07 (2).mp4");
        let vertical = vertical_path(wide);
        assert_eq!(vertical, Path::new(r"C:\Videos\Game 2026-11-17 14.03.07 (2) (vertical).mp4"));
        assert_eq!(wide_of(&vertical).as_deref(), Some(wide));
        assert_eq!(wide_of(wide), None);
        assert_eq!(source_part(wide), Path::new(r"C:\Videos\Game 2026-11-17 14.03.07 (2) (vertical).mp4.source.part"));
        assert_eq!(
            srt_of(&source_part(wide)),
            Path::new(r"C:\Videos\Game 2026-11-17 14.03.07 (2) (vertical).mp4.source.srt")
        );
    }

    #[test]
    fn vertical_moves_the_wide_clip_away_and_both_keeps_it() {
        let d = dir("after-save");
        let wide = d.join("a.mp4");
        fs::write(&wide, b"clip").unwrap();
        assert_eq!(Job::after_save(&wide, Shape::Wide), None);
        assert_eq!(Job::after_save(&wide, Shape::Both).unwrap().source(), wide);
        let job = Job::after_save(&wide, Shape::Vertical).unwrap();
        assert!(!wide.exists());
        assert_eq!(fs::read(job.source()).unwrap(), b"clip");
        forget(&wide);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_vertical_clip_replaces_the_waiting_wide_one_with_its_captions_burned_in() {
        let _turn = one_run_at_a_time();
        let d = dir("made");
        let wide = d.join("Game 2026-11-17 14.03.07.mp4");
        clip(&wide);
        let job = Job::after_save(&wide, Shape::Vertical).unwrap();
        let captions = |source: &Path| {
            let srt = srt_of(source);
            fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nNice!\n").unwrap();
            Some(srt)
        };
        let made = job.run(&x264(), captions).unwrap();
        assert_eq!(made, d.join("Game 2026-11-17 14.03.07 (vertical).mp4"));
        assert_eq!(files(&d), vec![made], "the working file and its burned captions are gone");
        assert!(unfinished_in(&d, &d).is_empty(), "forgotten");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_job_never_loses_the_clip_or_its_captions() {
        let _turn = one_run_at_a_time();
        let d = dir("failed");
        let wide = d.join("a.mp4");
        fs::write(&wide, b"not a video").unwrap();
        let job = Job::after_save(&wide, Shape::Vertical).unwrap();
        let captions = |source: &Path| {
            fs::write(srt_of(source), "1\n00:00:00,000 --> 00:00:01,000\nNice!\n").unwrap();
            Some(srt_of(source))
        };
        assert_eq!(job.run(&x264(), captions).unwrap_err(), KEPT_WIDE);
        assert_eq!(fs::read(&wide).unwrap(), b"not a video", "put back under its own name");
        assert!(d.join("a.srt").is_file(), "its captions too");
        assert_eq!(Job::after_save(&wide, Shape::Both).unwrap().run(&x264(), |_| None).unwrap_err(), ALSO_WIDE);
        assert_eq!(files(&d), vec![wide, d.join("a.srt")]);
        assert!(unfinished_in(&d, &d).is_empty(), "forgotten");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cut_off_jobs_are_found_again() {
        let d = dir("unfinished");
        let game = d.join("Game");
        fs::create_dir_all(&game).unwrap();
        // Vertical, cut off: the wide clip waits (and maybe half a vertical one).
        fs::write(d.join("a (vertical).mp4.source.part"), b"").unwrap();
        fs::write(d.join("a (vertical).mp4.part"), b"").unwrap();
        // Both, cut off, in a game folder.
        fs::write(game.join("b.mp4"), b"").unwrap();
        fs::write(game.join("b (vertical).mp4.part"), b"").unwrap();
        // Half a vertical clip whose wide clip was deleted since: removed.
        fs::write(d.join("c (vertical).mp4.part"), b"").unwrap();
        // Not ours to touch.
        fs::write(d.join("d.mp4.part"), b"").unwrap();
        fs::write(d.join("e (vertical).mp4"), b"").unwrap();

        let jobs = unfinished_in(&d, &d);
        assert_eq!(jobs, vec![resumed(d.join("a.mp4"), false, false), resumed(game.join("b.mp4"), true, false)]);
        let recorded = edit(|e| e.iter().filter(|e| e.wide.starts_with(&d)).count());
        assert_eq!(recorded, 2, "jobs found by their files are written down too");
        assert!(!d.join("c (vertical).mp4.part").exists());
        assert!(d.join("d.mp4.part").exists() && d.join("e (vertical).mp4").exists());
        jobs.iter().for_each(|j| forget(&j.wide));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn jobs_are_found_after_the_video_folder_changed() {
        let (d, now) = (dir("moved"), dir("moved-new-folder"));
        let wide = d.join("a.mp4");
        fs::write(&wide, b"clip").unwrap();
        Job::after_save(&wide, Shape::Vertical).unwrap();
        let both = d.join("b.mp4");
        fs::write(&both, b"clip").unwrap();
        Job::after_save(&both, Shape::Both).unwrap();
        assert_eq!(
            unfinished_in(&now, &d),
            vec![resumed(wide.clone(), false, false), resumed(both.clone(), true, false)]
        );
        // The clip was deleted since: forgotten.
        fs::remove_file(&both).unwrap();
        assert_eq!(unfinished_in(&now, &d), vec![resumed(wide.clone(), false, false)]);
        // Its whole folder can't be seen (a drive unplugged): kept for a later start.
        let away = d.with_file_name(format!("{}-unplugged", d.file_name().unwrap().to_string_lossy()));
        fs::rename(&d, &away).unwrap();
        assert!(unfinished_in(&now, &d).is_empty());
        fs::rename(&away, &d).unwrap();
        assert_eq!(unfinished_in(&now, &d), vec![resumed(wide.clone(), false, false)], "and found when it's back");
        forget(&wide);
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&now);
    }

    #[test]
    fn a_recorded_job_found_only_in_the_record_keeps_its_tries_and_what_was_made() {
        let (d, now) = (dir("record-only"), dir("record-only-new-folder"));
        let wide = d.join("a.mp4");
        fs::write(&wide, b"clip").unwrap();
        Job::after_save(&wide, Shape::Both).unwrap();
        let made = d.join("a (vertical).mp4");
        edit(|e| {
            e.iter_mut()
                .filter(|x| same(&x.wide, &wide))
                .for_each(|x| (x.tries, x.made) = (1, Some((made.clone(), true))))
        });
        let [job] = unfinished_in(&now, &d).try_into().unwrap();
        assert_eq!(
            job,
            Job { wide: wide.clone(), keep_wide: true, resumed: true, cut_off: true, made: Some((made, true)) }
        );
        forget(&wide);
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&now);
    }

    #[test]
    fn a_clip_that_cant_be_moved_away_stays_wide_for_its_job() {
        let d = dir("cant-move");
        let wide = d.join("a.mp4"); // not there: moving it fails
        let job = Job::after_save(&wide, Shape::Vertical).unwrap();
        assert_eq!(job.source(), wide, "the vertical clip comes next to it");
        forget(&wide);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_job_is_counted_as_started_while_it_is_made() {
        let _turn = one_run_at_a_time();
        let d = dir("counted");
        let wide = d.join("a.mp4");
        clip_of(&wide, 150);
        let job = Job::after_save(&wide, Shape::Both).unwrap();
        let tries = || edit(|e| e.iter().find(|x| same(&x.wide, &wide)).map(|x| x.tries));
        let seen = std::thread::scope(|s| {
            let run = s.spawn(|| job.run(&x264(), |_| None));
            let mut seen = Vec::new();
            while !run.is_finished() {
                let now = tries();
                if seen.last() != Some(&now) {
                    seen.push(now);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            run.join().unwrap().unwrap();
            seen
        });
        // A crash now would be counted at the next start (`cut_off`).
        assert!(seen.contains(&Some(1)), "{seen:?}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_clip_written_down_as_made_but_gone_is_made_again() {
        let _turn = one_run_at_a_time();
        let d = dir("made-gone");
        let wide = d.join("a.mp4");
        clip(&wide);
        Job::after_save(&wide, Shape::Vertical).unwrap();
        let vertical = d.join("a (vertical).mp4");
        let made = |e: &mut Entry| e.made = Some((vertical.clone(), false));
        edit(|entries| entries.iter_mut().filter(|e| same(&e.wide, &wide)).for_each(made));
        let [job] = unfinished_in(&d, &d).try_into().unwrap();
        assert_eq!(job.run(&x264(), |_| None).unwrap(), vertical);
        assert!(vertical.is_file() && fs::metadata(&vertical).unwrap().len() > 1000, "a real clip");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_clip_is_one_job_whatever_the_case_of_its_folder_name() {
        let d = dir("case");
        let wide = d.join("a.mp4");
        fs::write(&wide, b"clip").unwrap();
        Job::after_save(&wide, Shape::Vertical).unwrap();
        let shouted = PathBuf::from(d.to_string_lossy().to_uppercase());
        let jobs: Vec<_> = unfinished(&shouted).into_iter().filter(|j| same(&j.wide, &wide)).collect();
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        forget(&wide);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_job_saved_just_before_a_crash_is_not_made_twice() {
        let _turn = one_run_at_a_time();
        let d = dir("published");
        let wide = d.join("a.mp4");
        fs::write(&wide, b"clip").unwrap();
        Job::after_save(&wide, Shape::Vertical).unwrap();
        // Published with its captions burned in and written down, then the crash.
        let vertical = d.join("a (vertical).mp4");
        fs::write(&vertical, b"vertical").unwrap();
        fs::write(
            srt_of(&source_part(&wide)),
            "1
00:00:00,000 --> 00:00:01,000
Nice!
",
        )
        .unwrap();
        let made = |e: &mut Entry| e.made = Some((vertical.clone(), true));
        edit(|entries| entries.iter_mut().filter(|e| same(&e.wide, &wide)).for_each(made));
        let [job] = unfinished_in(&d, &d).try_into().unwrap();
        assert_eq!(job.run(&x264(), |_| panic!("not made again")).unwrap(), vertical);
        assert_eq!(files(&d), vec![vertical], "no (2), no working file, no second set of captions");
        assert!(unfinished_in(&d, &d).is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_vertical_clip_that_only_has_the_name_is_not_taken_for_the_job() {
        let _turn = one_run_at_a_time();
        let d = dir("same-name");
        let wide = d.join("a.mp4");
        clip(&wide);
        Job::after_save(&wide, Shape::Vertical).unwrap();
        let other = d.join("a (vertical).mp4");
        fs::write(&other, b"another clip").unwrap();
        let [job] = unfinished_in(&d, &d).try_into().unwrap();
        assert_eq!(job.run(&x264(), |_| None).unwrap(), d.join("a (vertical) (2).mp4"));
        assert_eq!(files(&d), vec![d.join("a (vertical) (2).mp4"), other.clone()]);
        assert_eq!(fs::read(&other).unwrap(), b"another clip");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_clip_put_back_under_a_2_name_keeps_its_captions_beside_it() {
        let _turn = one_run_at_a_time();
        let d = dir("put-back-2");
        let wide = d.join("a.mp4");
        fs::write(&wide, b"not a video").unwrap();
        let job = Job::after_save(&wide, Shape::Vertical).unwrap();
        fs::write(&wide, b"a clip saved since under that name").unwrap();
        let captions = |source: &Path| {
            fs::write(
                srt_of(source),
                "1
00:00:00,000 --> 00:00:01,000
Nice!
",
            )
            .unwrap();
            Some(srt_of(source))
        };
        assert_eq!(job.run(&x264(), captions).unwrap_err(), KEPT_WIDE);
        assert_eq!(files(&d), vec![d.join("a (2).mp4"), d.join("a (2).srt"), wide]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_job_that_would_start_while_quitting_waits_for_the_next_start() {
        let _turn = one_run_at_a_time();
        let d = dir("stopping");
        let wide = d.join("a.mp4");
        fs::write(&wide, b"clip").unwrap();
        let job = Job::after_save(&wide, Shape::Vertical).unwrap();
        // Quitting while its captions are made (they are stopped, and the job wakes after).
        let woke = job.run(&x264(), |_| {
            stopping();
            None
        });
        // A job that gets its turn (or is saved) after quitting began.
        let queued = job.run(&x264(), |_| panic!("no captions while quitting"));
        running().stopping = false;
        assert_eq!((woke.unwrap_err(), queued.unwrap_err()), (LATER.to_owned(), LATER.to_owned()));
        assert_eq!(files(&d), vec![source_part(&wide)], "left as it was");
        assert_eq!(unfinished_in(&d, &d), vec![resumed(wide.clone(), false, false)], "made again, not given up");
        forget(&wide);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_job_that_ended_backsight_is_given_up_but_quitting_resumes_it() {
        let _turn = one_run_at_a_time();
        let d = dir("crashing");
        let wide = d.join("a.mp4");
        fs::write(&wide, b"clip").unwrap();
        Job::after_save(&wide, Shape::Vertical).unwrap();
        let started = |w: &Path| edit(|e| e.iter_mut().filter(|x| same(&x.wide, w)).for_each(|x| x.tries += 1));

        // Started, then Backsight quit: made again.
        started(&wide);
        not_cut_off(&wide); // what `stopping` does for the running job
        assert_eq!(unfinished_in(&d, &d), vec![resumed(wide.clone(), false, false)]);

        // Started, then Backsight crashed: given up, the clip kept wide, and told.
        started(&wide);
        let [job] = unfinished_in(&d, &d).try_into().unwrap();
        assert!(job.cut_off);
        let e = job.run(&x264(), |_| panic!("no captions for a job given up")).unwrap_err();
        assert!(
            e.contains("closed unexpectedly") && e.contains("\"a.mp4\"") && e.contains("kept the clip wide"),
            "{e}"
        );
        assert_eq!(files(&d), vec![wide]);
        assert!(unfinished_in(&d, &d).is_empty(), "not tried again");

        // With Both, the wide clip still gets its captions.
        let both = d.join("b.mp4");
        fs::write(&both, b"clip").unwrap();
        Job::after_save(&both, Shape::Both).unwrap();
        started(&both);
        let [job] = unfinished_in(&d, &d).try_into().unwrap();
        let mut captioned = None;
        let e = job
            .run(&x264(), |clip| {
                captioned = Some(clip.to_path_buf());
                None
            })
            .unwrap_err();
        assert!(e.contains("The wide clip is saved"), "{e}");
        assert_eq!(captioned, Some(both));
        let _ = fs::remove_dir_all(&d);
    }
}
