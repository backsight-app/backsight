//! Publishing a finished file without ever replacing another one. Platform edge (one Win32 call).

use std::{
    ffi::OsString,
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW},
    core::PCWSTR,
};

/// Renames `from` to `to`, failing with `AlreadyExists` instead of replacing a file. Never copies
/// across volumes. `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` makes the check and the rename
/// one step, so a file that appears at `to` at any moment is never overwritten. It needs no hard
/// links, so it works on FAT, exFAT and network shares too.
///
/// Antivirus, search indexing or cloud sync often hold a fresh file for a moment, so access-denied
/// (5) and sharing-violation (32) are retried for about a second before giving up. Callers delete
/// the source on failure, so giving up too early would lose a finished clip.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut tries = 0;
    loop {
        // SAFETY: terminated paths live through the call. No flags: never replace or copy across volumes.
        let result = unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), MOVE_FILE_FLAGS(0)) }
            // HRESULT_FROM_WIN32 keeps the Win32 code in the low 16 bits (ERROR_ALREADY_EXISTS = 183).
            .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xffff));
        match result {
            Err(e) if matches!(e.raw_os_error(), Some(5 | 32)) && tries < 10 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            other => return other,
        }
    }
}

/// `path` for `n` = 1, then `name (n).ext` next to it.
fn numbered(path: &Path, n: u32) -> PathBuf {
    if n <= 1 {
        return path.to_path_buf();
    }
    let mut name = OsString::from(path.file_stem().unwrap_or_default());
    name.push(format!(" ({n})"));
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    path.with_file_name(name)
}

/// Moves `from` to `to`, or to `name (2).ext`, `name (3).ext`... when a file already has that name
/// (also when it appears while we run). Returns the name used.
pub fn publish(from: &Path, to: &Path) -> io::Result<PathBuf> {
    for n in 1..10_000 {
        let candidate = numbered(to, n);
        match rename_no_replace(from, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free file name"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("backsight-fsutil-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_destination_that_appears_is_not_overwritten() {
        let d = dir("race");
        let (part, to) = (d.join("a.mp4.part"), d.join("a.mp4"));
        fs::write(&part, "new").unwrap();
        // The file shows up after any existence check would have passed.
        fs::write(&to, "theirs").unwrap();
        let e = rename_no_replace(&part, &to).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&to).unwrap(), "theirs");
        assert_eq!(fs::read_to_string(&part).unwrap(), "new");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn collisions_get_the_next_free_name() {
        let d = dir("next");
        let to = d.join("clip.mp4");
        fs::write(&to, "1").unwrap();
        fs::write(d.join("clip (2).mp4"), "2").unwrap();
        let part = d.join("clip.mp4.part");
        fs::write(&part, "new").unwrap();
        let used = publish(&part, &to).unwrap();
        assert_eq!(used, d.join("clip (3).mp4"));
        assert_eq!(fs::read_to_string(&used).unwrap(), "new");
        assert_eq!(fs::read_to_string(&to).unwrap(), "1");
        assert_eq!(fs::read_to_string(d.join("clip (2).mp4")).unwrap(), "2");
        assert!(!part.exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_file_held_open_for_a_moment_is_still_published() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = dir("held");
        let (part, to) = (d.join("h.mp4.part"), d.join("h.mp4"));
        fs::write(&part, "clip").unwrap();
        // Share mode 0, like a scanner that locks the file while it reads it.
        let held = fs::OpenOptions::new().read(true).share_mode(0).open(&part).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        assert_eq!(publish(&part, &to).unwrap(), to);
        release.join().unwrap();
        assert_eq!(fs::read_to_string(&to).unwrap(), "clip");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_free_name_is_used_as_is() {
        let d = dir("free");
        let part = d.join("x.part");
        fs::write(&part, "v").unwrap();
        assert_eq!(publish(&part, &d.join("x.mkv")).unwrap(), d.join("x.mkv"));
        fs::remove_dir_all(&d).unwrap();
    }
}
