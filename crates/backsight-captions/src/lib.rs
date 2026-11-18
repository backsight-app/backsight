//! Captions: on-device speech to text for the microphone track of saved clips (ADR 0015).
//!
//! After a clip is saved, [`Queue`] runs `backsight.exe --captions …` (one clip at a time) in a
//! separate process at background priority on at most two threads, so the game and the capture
//! keep the CPU and a crash in whisper.cpp can't touch the replay buffer. That process
//! ([`child_main`]) reads the track, finds the speech (Silero VAD), transcribes it (Whisper base),
//! and writes `<clip>.srt` next to the clip through `.srt.part`. The clip itself is never touched.

pub mod cues;
pub mod model;
pub mod queue;
mod whisper;

pub use queue::Queue;

use std::{
    fs, io,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The command-line switch of the captions process.
pub const CHILD_FLAG: &str = "--captions";

/// One clip to caption.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub clip: PathBuf,
    /// The microphone's audio track, counting audio tracks only; `None` for the last one.
    pub track: Option<usize>,
    /// A Whisper language code (`en`, `de`…) or `auto`.
    pub language: String,
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Captions written to this file.
    Written(PathBuf),
    /// Nobody talked: no file.
    NoSpeech,
    /// A file of that name was already there and was kept.
    AlreadyThere,
    /// A plain-language reason for the user.
    Failed(String),
}

/// `clip.mp4` → `clip.srt`: the name players look for next to a video.
pub fn captions_path(clip: &Path) -> PathBuf {
    clip.with_extension("srt")
}

/// The CPU features the build expects (`GGML_NATIVE=OFF` builds for AVX2, FMA and F16C). Without
/// them whisper.cpp would crash; most CPUs from 2015 on have them.
pub fn cpu_supported() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("fma")
        && std::arch::is_x86_feature_detected!("f16c")
        && std::arch::is_x86_feature_detected!("bmi2")
}

/// Threads for Whisper on a CPU with `logical` threads: a quarter of them, at most two.
pub fn threads_for(logical: usize) -> usize {
    (logical / 4).clamp(1, 2)
}

const OLD_CPU: &str = "Captions need a newer processor (most from 2015 on), so they are off on this PC.";
const NOT_DOWNLOADED: &str = "Captions aren't downloaded. Press Download captions on the Audio page.";
const DAMAGED: &str = "The captions download is damaged. Remove it on the Audio page and download it again.";

/// What `--captions <clip> <track or "last"> <language>` prints on its last line.
impl Outcome {
    fn line(&self) -> String {
        match self {
            Outcome::Written(_) => "ok".into(),
            Outcome::NoSpeech => "no-speech".into(),
            Outcome::AlreadyThere => "exists".into(),
            Outcome::Failed(why) => format!("error: {why}"),
        }
    }

    fn parse(output: &str, clip: &Path) -> Outcome {
        match output.lines().last().unwrap_or("").trim() {
            "ok" => Outcome::Written(captions_path(clip)),
            "no-speech" => Outcome::NoSpeech,
            "exists" => Outcome::AlreadyThere,
            line => match line.strip_prefix("error: ") {
                Some(why) => Outcome::Failed(why.into()),
                // Ended without an answer: a crash.
                None => Outcome::Failed(format!(
                    "Backsight couldn't make captions for {}.",
                    clip.file_name().unwrap_or_default().to_string_lossy()
                )),
            },
        }
    }
}

/// The captions process for `job`: no window, below-normal priority from the start, its answer
/// on stdout.
pub fn command(exe: &Path, job: &Job) -> Command {
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut c = Command::new(exe);
    c.arg(CHILD_FLAG)
        .arg(&job.clip)
        .arg(job.track.map_or("last".into(), |t| t.to_string()))
        .arg(&job.language)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW);
    c
}

/// `backsight.exe --captions …`: logs and the answer go to stdout, which the queue reads.
pub fn child_process() -> i32 {
    tracing_subscriber::fmt().with_writer(io::stdout).with_ansi(false).without_time().with_target(false).init();
    let args: Vec<String> = std::env::args().skip(2).collect();
    child_main(&args, io::stdout().lock())
}

/// The captions process: `args` are what follows [`CHILD_FLAG`]. Prints the outcome line on
/// `out`; returns the exit code.
pub fn child_main(args: &[String], mut out: impl io::Write) -> i32 {
    let outcome = match args {
        [clip, track, language] => {
            let job = Job { clip: clip.into(), track: track.parse().ok(), language: language.clone() };
            match model::dir() {
                Ok(dir) => run(&job, &dir),
                Err(e) => Outcome::Failed(e),
            }
        }
        _ => Outcome::Failed(format!("usage: {CHILD_FLAG} <clip> <track|last> <language>")),
    };
    let _ = writeln!(out, "{}", outcome.line());
    i32::from(matches!(outcome, Outcome::Failed(_)))
}

/// Captions `job` in this process with the models in `models`.
pub fn run(job: &Job, models: &Path) -> Outcome {
    let name = job.clip.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if !cpu_supported() {
        return Outcome::Failed(OLD_CPU.into());
    }
    if !model::installed(models) {
        return Outcome::Failed(NOT_DOWNLOADED.into());
    }
    let audio = match backsight_encode::media::read_speech(&job.clip, job.track) {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("captions: can't read the sound of {}: {e}", job.clip.display());
            return Outcome::Failed(format!("Backsight couldn't read the sound of {name}, so it has no captions."));
        }
    };
    let logical = std::thread::available_parallelism().map_or(1, |n| n.get());
    let segments = whisper::transcribe(
        &audio,
        &models.join(model::WHISPER.name),
        &models.join(model::VAD.name),
        &job.language,
        threads_for(logical),
        background,
    );
    let segments = match segments {
        Ok(s) => s,
        Err(whisper::Error::Model(e)) => {
            tracing::warn!("captions: {e}");
            return Outcome::Failed(DAMAGED.into());
        }
        Err(whisper::Error::Run(e)) => {
            tracing::warn!("captions: {e}");
            return Outcome::Failed(format!("Backsight couldn't make captions for {name}."));
        }
    };
    let cues = cues::cues(&segments);
    if cues.is_empty() {
        return Outcome::NoSpeech;
    }
    match write(&captions_path(&job.clip), &cues::srt(&cues)) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("captions: can't write next to {}: {e}", job.clip.display());
            Outcome::Failed(format!(
                "Backsight couldn't save the captions next to {name}. Check that the drive has free space."
            ))
        }
    }
}

/// Writes `text` to `path` through `<path>.part`; never replaces a file already there.
fn write(path: &Path, text: &str) -> io::Result<Outcome> {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    fs::write(&part, text)?;
    match backsight_encode::fsutil::rename_no_replace(&part, path) {
        Ok(()) => Ok(Outcome::Written(path.to_path_buf())),
        Err(e) => {
            let _ = fs::remove_file(&part);
            if e.kind() == io::ErrorKind::AlreadyExists { Ok(Outcome::AlreadyThere) } else { Err(e) }
        }
    }
}

/// Puts this whole process (every whisper.cpp thread too) in background mode: lowest CPU, disk
/// and memory priority. Called after the models are read, which background disk priority can
/// slow from a fraction of a second to a minute on a busy drive.
fn background() {
    use windows::Win32::System::Threading::{GetCurrentProcess, PROCESS_MODE_BACKGROUND_BEGIN, SetPriorityClass};
    // shortcut: no EcoQoS (SetProcessInformation power throttling) yet, add it if captions still
    // show in game frame times on hybrid Intel CPUs.
    // SAFETY: the pseudo handle of this process is always valid.
    if let Err(e) = unsafe { SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN) } {
        tracing::warn!("captions: couldn't enter background mode: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captions_sit_next_to_the_clip() {
        assert_eq!(captions_path(Path::new(r"C:\v\Game 2026.mp4")), Path::new(r"C:\v\Game 2026.srt"));
        assert_eq!(captions_path(Path::new(r"C:\v\a.b.mkv")), Path::new(r"C:\v\a.b.srt"));
    }

    #[test]
    fn at_most_two_threads_and_never_most_of_the_cpu() {
        assert_eq!([2, 4, 8, 12, 16, 32].map(threads_for), [1, 1, 2, 2, 2, 2]);
    }

    #[test]
    fn outcome_lines_round_trip() {
        let clip = Path::new(r"C:\v\clip.mp4");
        for o in [
            Outcome::Written(captions_path(clip)),
            Outcome::NoSpeech,
            Outcome::AlreadyThere,
            Outcome::Failed("Try again.".into()),
        ] {
            assert_eq!(Outcome::parse(&format!("noise\n{}\n", o.line()), clip), o);
        }
        assert_eq!(Outcome::parse("", clip), Outcome::Failed("Backsight couldn't make captions for clip.mp4.".into()));
    }

    #[test]
    fn the_process_answers_bad_arguments_plainly() {
        let mut out = Vec::new();
        assert_eq!(child_main(&["a".into()], &mut out), 1);
        assert!(String::from_utf8(out).unwrap().starts_with("error: usage"));
    }

    #[test]
    fn writing_never_replaces_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.srt");
        assert_eq!(write(&path, "one").unwrap(), Outcome::Written(path.clone()));
        assert_eq!(write(&path, "two").unwrap(), Outcome::AlreadyThere);
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1, "no .part left behind");
    }

    #[test]
    fn missing_models_are_reported_before_any_work() {
        let dir = tempfile::tempdir().unwrap();
        let job = Job { clip: dir.path().join("none.mp4"), track: None, language: "auto".into() };
        let expected = if cpu_supported() { NOT_DOWNLOADED } else { OLD_CPU };
        assert_eq!(run(&job, dir.path()), Outcome::Failed(expected.into()));
    }

    /// An MP4 clip (x264 + AAC) with the sound of `wav`: 16-bit PCM, mono, 16 kHz.
    fn clip_from_wav(wav: &Path, dir: &Path) -> PathBuf {
        use backsight_encode::{
            Catalog,
            catalog::resolve,
            mux::{Muxer, Track, needs_global_header},
            session::{AudioSession, VideoSession},
        };
        let bytes = fs::read(wav).unwrap();
        let data = bytes.windows(4).position(|w| w == b"data").expect("a WAV data chunk") + 8;
        let pcm: Vec<f32> =
            bytes[data..].as_chunks::<2>().0.iter().map(|s| f32::from(i16::from_le_bytes(*s)) / 32768.0).collect();
        let c = Catalog::get();
        let container = c.container("mp4").unwrap();
        let gh = needs_global_header(container);
        let venc = c.video_encoder("x264").unwrap();
        let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, 25, gh).unwrap();
        let aenc = c.audio_encoder("aac").unwrap();
        let mut audio = AudioSession::open(aenc, &resolve(&aenc.options, None), 16_000, 1, gh).unwrap();
        let mut mux = Muxer::create(&dir.join("speech.mp4"), container, &video.params(), &[&audio.params()]).unwrap();
        for (i, chunk) in pcm.chunks(640).enumerate() {
            for (data, _) in video.planes_mut().unwrap().into_iter().flatten() {
                data.fill(128);
            }
            video.send(i as i64, &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
            audio.push(chunk, &mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
        }
        video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
        audio.finish(&mut |p| mux.write(Track::Audio(0), &p).unwrap()).unwrap();
        mux.finish().unwrap()
    }

    /// A real run: the models in the folder `BACKSIGHT_CAPTIONS_MODELS`, speech from the WAV
    /// `BACKSIGHT_CAPTIONS_WAV` (16-bit, mono, 16 kHz). Prints the captions and the time taken.
    #[test]
    #[ignore = "needs the downloaded models and a WAV with speech"]
    fn real_model_captions_a_clip() {
        let models = PathBuf::from(std::env::var("BACKSIGHT_CAPTIONS_MODELS").expect("models folder"));
        let wav = PathBuf::from(std::env::var("BACKSIGHT_CAPTIONS_WAV").expect("a WAV"));
        let dir = tempfile::tempdir().unwrap();
        let clip = clip_from_wav(&wav, dir.path());
        let srt = captions_path(&clip);
        let started = std::time::Instant::now();
        let outcome = run(&Job { clip, track: None, language: "auto".into() }, &models);
        println!("{outcome:?} in {:.1?}", started.elapsed());
        assert_eq!(outcome, Outcome::Written(srt.clone()));
        let text = fs::read_to_string(&srt).unwrap();
        println!("{text}");
        assert!(text.contains(" --> "));
    }
}
