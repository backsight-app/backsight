//! The speech model files: where they live, the opt-in download from Backsight's own GitHub
//! release (the only network use besides updates, `docs/privacy.md`), and checking them.

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering::Relaxed},
};

/// One model file as published.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub name: &'static str,
    pub size: u64,
    /// SHA-256 of the file, lowercase hex.
    pub sha256: &'static str,
}

/// The release the files are attached to.
pub const RELEASE: &str = "https://github.com/backsight-app/backsight/releases/download/captions-model-1";

// The files as published by whisper.cpp (huggingface.co/ggerganov/whisper.cpp, ggml-org/whisper-vad),
// attached unchanged to the release; a new upload needs new hashes and sizes here.
/// Whisper base, multilingual, 5-bit (MIT).
pub const WHISPER: ModelFile = ModelFile {
    name: "ggml-base-q5_1.bin",
    size: 59_707_625,
    sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
};
/// Silero voice activity detection v6.2.0 (MIT): finds the parts with speech.
pub const VAD: ModelFile = ModelFile {
    name: "ggml-silero-v6.2.0.bin",
    size: 885_098,
    sha256: "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
};
pub const FILES: [ModelFile; 2] = [WHISPER, VAD];

/// `%APPDATA%\Backsight\models`.
pub fn dir() -> Result<PathBuf, String> {
    backsight_config::app_dir()
        .map(|d| d.join("models"))
        .map_err(|e| format!("Backsight couldn't find its settings folder. ({e})"))
}

/// Bytes the download fetches.
pub fn download_size() -> u64 {
    FILES.iter().map(|f| f.size).sum()
}

/// Every file is there at its size. (The hash is checked when it is downloaded.)
pub fn installed(dir: &Path) -> bool {
    FILES.iter().all(|f| fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.size))
}

/// Deletes the model files (and any unfinished download), nothing else in `dir`.
pub fn remove(dir: &Path) -> io::Result<()> {
    for f in FILES {
        for path in [dir.join(f.name), part(dir, &f)] {
            match fs::remove_file(path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    Ok(())
}

fn part(dir: &Path, f: &ModelFile) -> PathBuf {
    dir.join(format!("{}.part", f.name))
}

const NO_NETWORK: &str = "Backsight couldn't download captions. Check your internet connection and try again.";

/// Downloads the missing files into `dir`, calling `progress(done, total)` in bytes. Stops with
/// an error when `cancel` is set. Each file is written to `.part`, checked, then renamed.
pub fn download(dir: &Path, mut progress: impl FnMut(u64, u64), cancel: &AtomicBool) -> Result<(), String> {
    // The same TLS setup as the updater: rustls with ring (a second install is a harmless no-op).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("Backsight/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| plain(NO_NETWORK, &e))?;
    let total = download_size();
    let mut done = 0;
    for f in FILES {
        if fs::metadata(dir.join(f.name)).is_ok_and(|m| m.len() == f.size) {
            done += f.size;
            continue;
        }
        let url = format!("{RELEASE}/{}", f.name);
        let response = client.get(&url).send().map_err(|e| plain(NO_NETWORK, &e))?;
        if !response.status().is_success() {
            tracing::warn!("captions download: {url} answered {}", response.status());
            return Err("The captions download isn't available right now. Try again later.".into());
        }
        let start = done;
        install(&f, response, dir, &mut |n| progress(start + n, total), cancel)?;
        done += f.size;
    }
    progress(total, total);
    Ok(())
}

fn plain(message: &str, e: &dyn std::fmt::Display) -> String {
    tracing::warn!("captions download: {e}");
    message.into()
}

/// Writes `from` to `dir` as `file`, through `<name>.part`, checking size and SHA-256 before the
/// rename. `progress` gets the bytes written so far.
pub fn install(
    file: &ModelFile,
    mut from: impl Read,
    dir: &Path,
    progress: &mut dyn FnMut(u64),
    cancel: &AtomicBool,
) -> Result<(), String> {
    let cant_save = |e: io::Error| {
        tracing::warn!("captions download: can't write in {}: {e}", dir.display());
        format!(
            "Backsight couldn't save captions in {}. Check that the drive has free space and try again.",
            dir.display()
        )
    };
    fs::create_dir_all(dir).map_err(cant_save)?;
    let tmp = part(dir, file);
    let result = (|| {
        let mut out = fs::File::create(&tmp).map_err(cant_save)?;
        let mut hash = Sha256::new();
        let mut buf = vec![0; 256 * 1024];
        let mut written = 0u64;
        loop {
            if cancel.load(Relaxed) {
                return Err("The captions download was stopped.".to_string());
            }
            let n = match from.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(plain(NO_NETWORK, &e)),
            };
            written += n as u64;
            if written > file.size {
                break; // not the file we expect; the check below says so
            }
            hash.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(cant_save)?;
            progress(written);
        }
        out.sync_all().map_err(cant_save)?;
        let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if written != file.size || hex != file.sha256 {
            tracing::warn!("captions download: {} is {written} bytes, SHA-256 {hex}", file.name);
            return Err("The captions download arrived damaged. Try again.".to_string());
        }
        Ok(())
    })();
    match result {
        Ok(()) => fs::rename(&tmp, dir.join(file.name)).map_err(cant_save),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const HELLO: ModelFile = ModelFile {
        name: "hello.bin",
        size: 5,
        // SHA-256 of "hello"
        sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    };

    #[test]
    fn install_checks_size_and_hash_before_publishing() {
        let dir = tempfile::tempdir().unwrap();
        let no = AtomicBool::new(false);
        let mut seen = 0;
        install(&HELLO, Cursor::new(b"hello"), dir.path(), &mut |n| seen = n, &no).unwrap();
        assert_eq!(fs::read(dir.path().join("hello.bin")).unwrap(), b"hello");
        assert_eq!(seen, 5);

        for bad in [&b"hellO"[..], b"hell", b"hello world"] {
            let err = install(&HELLO, Cursor::new(bad), dir.path(), &mut |_| {}, &no).unwrap_err();
            assert!(err.contains("damaged"), "{err}");
        }
        assert_eq!(fs::read(dir.path().join("hello.bin")).unwrap(), b"hello", "the good copy stays");
        assert!(!part(dir.path(), &HELLO).exists(), "no .part left behind");
    }

    #[test]
    fn a_stopped_download_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = install(&HELLO, Cursor::new(b"hello"), dir.path(), &mut |_| {}, &AtomicBool::new(true)).unwrap_err();
        assert!(err.contains("stopped"));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn installed_needs_every_file_and_remove_keeps_other_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!installed(dir.path()));
        for f in FILES {
            fs::File::create(dir.path().join(f.name)).unwrap().set_len(f.size).unwrap();
        }
        assert!(installed(dir.path()));
        fs::write(dir.path().join("mine.txt"), "keep").unwrap();
        fs::write(part(dir.path(), &WHISPER), "half").unwrap();
        remove(dir.path()).unwrap();
        assert!(!installed(dir.path()));
        let left: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["mine.txt"]);
        remove(dir.path()).unwrap();
    }

    /// Hands out `steps` one read at a time: bytes, or an error of that kind.
    struct Steps(Vec<Result<&'static [u8], io::ErrorKind>>);

    impl Read for Steps {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() {
                return Ok(0);
            }
            let bytes = self.0.remove(0)?;
            buf[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        }
    }

    #[test]
    fn a_download_arriving_in_pieces_survives_interruptions_only() {
        let dir = tempfile::tempdir().unwrap();
        let no = AtomicBool::new(false);
        let mut seen = vec![];
        let pieces = Steps(vec![Ok(b"he"), Err(io::ErrorKind::Interrupted), Ok(b"l"), Ok(b"lo")]);
        install(&HELLO, pieces, dir.path(), &mut |n| seen.push(n), &no).unwrap();
        assert_eq!(seen, [2, 3, 5]);
        let broken = Steps(vec![Ok(b"he"), Err(io::ErrorKind::ConnectionReset), Ok(b"llo")]);
        let err = install(&HELLO, broken, dir.path(), &mut |_| {}, &no).unwrap_err();
        assert_eq!(err, NO_NETWORK);
        assert_eq!(plain("words", &"detail"), "words");
    }

    #[test]
    fn files_already_there_are_not_downloaded_again() {
        let dir = tempfile::tempdir().unwrap();
        for f in FILES {
            fs::File::create(dir.path().join(f.name)).unwrap().set_len(f.size).unwrap();
        }
        let mut calls = vec![];
        download(dir.path(), |done, total| calls.push((done, total)), &AtomicBool::new(false)).unwrap();
        assert_eq!(calls, [(download_size(), download_size())]);
    }

    #[test]
    fn remove_reports_what_it_couldnt_delete() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(WHISPER.name)).unwrap();
        assert!(remove(dir.path()).is_err(), "a folder in the model's place");
        assert!(super::dir().unwrap().ends_with("models"));
    }

    #[test]
    fn release_files_are_pinned() {
        for f in FILES {
            assert_eq!(f.sha256.len(), 64);
            assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        }
        assert!((60_000_000..62_000_000).contains(&download_size()), "about 61 MB, as the UI says");
    }

    /// Downloads the real files from the release into a temporary folder.
    #[test]
    #[ignore = "network: downloads about 61 MB; run by hand"]
    fn the_release_serves_the_pinned_files() {
        let dir = tempfile::tempdir().unwrap();
        download(dir.path(), |_, _| {}, &AtomicBool::new(false)).unwrap();
        assert!(installed(dir.path()));
    }
}
