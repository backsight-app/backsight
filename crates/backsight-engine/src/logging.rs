//! `tracing` logs to `%APPDATA%\Backsight\logs\backsight.log`. Each start begins a new file and
//! keeps the previous run's as `backsight.old.log`, so the folder never grows past two runs.
//! Lines are scrubbed like crash reports ([`crate::crash::scrub`]), so the log can be shared.

use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::Mutex,
};

/// Sends `tracing` output (info and above) to the log file. Call once at startup, after
/// [`crate::crash::install`] (its rules scrub the lines); returns the log folder.
pub fn init() -> io::Result<PathBuf> {
    let dir = backsight_config::app_dir().map_err(io::Error::other)?.join("logs");
    fs::create_dir_all(&dir)?;
    let path = dir.join("backsight.log");
    let _ = fs::rename(&path, dir.join("backsight.old.log")); // none on the first run
    let file = fs::File::create(&path)?;
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(Scrubbed(file)))
        .with_max_level(tracing::Level::INFO)
        .try_init()
        .map_err(io::Error::other)?;
    Ok(dir)
}

/// Scrubs each write; `tracing` writes one whole event at a time.
struct Scrubbed<W>(W);

impl<W: Write> Write for Scrubbed<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(crate::crash::scrub_installed(&String::from_utf8_lossy(buf)).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
