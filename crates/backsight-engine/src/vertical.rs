//! Vertical clips after a save (ADR 0015): which files, the background job, and picking up jobs
//! cut off by quitting or a crash. The picture work is `backsight_encode::vertical`.

use crate::{recovery, system};
use backsight_config::Settings;
use backsight_encode::{
    Catalog,
    catalog::resolve,
    fsutil::{publish, rename_no_replace},
    vertical::make_vertical,
};
use std::{
    fs,
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

fn source_part(wide: &Path) -> PathBuf {
    let mut name = vertical_path(wide).into_os_string();
    name.push(SOURCE_PART);
    PathBuf::from(name)
}

/// Making the vertical clip of one wide clip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    wide: PathBuf,
    /// Both: the wide clip stays. Vertical: it waits under its [`source_part`] name.
    keep_wide: bool,
}

impl Job {
    /// The job for a clip just saved at `wide`; `None` for Wide. With Vertical the wide clip is moved
    /// out of the Library's sight first (renamed, never deleted), so only the vertical one shows.
    pub fn after_save(wide: &Path, shape: Shape) -> Option<Job> {
        let keep_wide = match shape {
            Shape::Wide => return None,
            Shape::Both => true,
            Shape::Vertical => false,
        };
        if !keep_wide && let Err(e) = rename_no_replace(wide, &source_part(wide)) {
            tracing::warn!("{} stays as it is, the vertical clip comes next to it: {e}", wide.display());
            return Some(Job { wide: wide.to_path_buf(), keep_wide: true });
        }
        Some(Job { wide: wide.to_path_buf(), keep_wide })
    }

    fn source(&self) -> PathBuf {
        if self.keep_wide { self.wide.clone() } else { source_part(&self.wide) }
    }

    /// Makes the vertical clip with the video encoder in `settings` and returns it. Jobs run one at a
    /// time, in background mode, and take as long as they take. With Vertical, the waiting wide clip
    /// is removed once the vertical one is saved, or put back under its own name if that failed.
    /// The error is a message for the user.
    pub fn run(&self, settings: &Settings) -> Result<PathBuf, String> {
        static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let _background = system::background_mode();
        let (started, source) = (Instant::now(), self.source());
        match make(&source, &vertical_path(&self.wide), settings) {
            Ok(made) => {
                tracing::info!("made {} in {:.1?}", made.display(), started.elapsed());
                if !self.keep_wide
                    && let Err(e) = fs::remove_file(&source)
                {
                    tracing::warn!("couldn't remove {}: {e}", source.display());
                }
                Ok(made)
            }
            Err(e) if self.keep_wide => {
                tracing::warn!("no vertical clip of {}: {e}", self.wide.display());
                Err(ALSO_WIDE.into())
            }
            Err(e) => {
                tracing::warn!("no vertical clip of {}: {e}", self.wide.display());
                match publish(&source, &self.wide) {
                    Ok(_) => Err(KEPT_WIDE.into()),
                    Err(e) => {
                        tracing::warn!("couldn't rename {} back: {e}", source.display());
                        let name = source.file_name().unwrap_or_default().to_string_lossy();
                        Err(format!(
                            "Backsight couldn't make the vertical clip. Your clip is kept as \"{name}\": remove \"{SOURCE_PART}\" from its name to play it."
                        ))
                    }
                }
            }
        }
    }
}

fn make(source: &Path, vertical: &Path, settings: &Settings) -> Result<PathBuf, String> {
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
    make_vertical(source, vertical, &Catalog::clip_container(container), venc, &values)
}

/// The jobs cut off by quitting or a crash, in `folder` and its game folders, found by what they
/// left: a waiting wide clip, or an unfinished vertical clip next to its wide one. Call it at
/// start, before any job runs. An unfinished vertical clip whose wide clip is gone is removed.
pub fn unfinished(folder: &Path) -> Vec<Job> {
    recovery::files(folder)
        .into_iter()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if let Some(vertical) = name.strip_suffix(SOURCE_PART) {
                return wide_of(&path.with_file_name(vertical)).map(|wide| Job { wide, keep_wide: false });
            }
            let wide = wide_of(&path.with_file_name(name.strip_suffix(".part")?))?;
            if source_part(&wide).exists() {
                return None; // found as a waiting wide clip
            }
            if wide.is_file() {
                return Some(Job { wide, keep_wide: true });
            }
            let _ = fs::remove_file(&path);
            None
        })
        .collect()
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
        let c = Catalog::get();
        let container = Catalog::clip_container(c.container("mp4").unwrap());
        let venc = c.video_encoder("x264").unwrap();
        let gh = needs_global_header(&container);
        let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 640, 360, 30, gh).unwrap();
        let mut mux = Muxer::create(path, &container, &video.params(), &[]).unwrap();
        for i in 0..30 {
            for (data, _) in video.planes_mut().unwrap().into_iter().flatten() {
                data.fill((i * 8) as u8);
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
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_vertical_clip_replaces_the_waiting_wide_one() {
        let d = dir("made");
        let wide = d.join("Game 2026-11-17 14.03.07.mp4");
        clip(&wide);
        let made = Job::after_save(&wide, Shape::Vertical).unwrap().run(&x264()).unwrap();
        assert_eq!(made, d.join("Game 2026-11-17 14.03.07 (vertical).mp4"));
        let left: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(left, vec![made]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_job_never_loses_the_clip() {
        let d = dir("failed");
        let wide = d.join("a.mp4");
        fs::write(&wide, b"not a video").unwrap();
        let job = Job::after_save(&wide, Shape::Vertical).unwrap();
        assert_eq!(job.run(&x264()).unwrap_err(), KEPT_WIDE);
        assert_eq!(fs::read(&wide).unwrap(), b"not a video", "put back under its own name");
        assert_eq!(Job::after_save(&wide, Shape::Both).unwrap().run(&x264()).unwrap_err(), ALSO_WIDE);
        let left: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(left, vec![wide]);
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

        let mut jobs = unfinished(&d);
        jobs.sort_by(|x, y| x.wide.file_name().cmp(&y.wide.file_name()));
        assert_eq!(
            jobs,
            vec![Job { wide: d.join("a.mp4"), keep_wide: false }, Job { wide: game.join("b.mp4"), keep_wide: true }]
        );
        assert!(!d.join("c (vertical).mp4.part").exists());
        assert!(d.join("d.mp4.part").exists() && d.join("e (vertical).mp4").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
