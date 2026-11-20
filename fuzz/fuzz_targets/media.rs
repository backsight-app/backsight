#![no_main]
//! Damaged media: a half-written recording (`….recording.part`) after a crash, or any file in the
//! clips folder the library reads. Recovery must never lose the original's bytes; reading must
//! never panic, hang or allocate without bound. The first byte picks the container.
use backsight_encode::media;
use backsight_engine::recovery;
use libfuzzer_sys::fuzz_target;
use std::{fs, path::PathBuf, sync::OnceLock};

const EXTS: [&str; 5] = ["mp4", "mkv", "ts", "webm", "mov"];

fn dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("backsight-fuzz-media-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&pick, bytes)) = data.split_first() else { return };
    let d = dir();
    let ext = EXTS[usize::from(pick) % EXTS.len()];
    let file = d.join(format!("in.{ext}"));
    fs::write(&file, bytes).unwrap();
    let _ = media::probe(&file);
    if pick & 0x10 != 0 {
        let _ = media::read_speech(&file, None);
    }
    if pick & 0x20 != 0 {
        let _ = media::thumbnail(&file, &d.join("thumb.jpg"));
    }
    if pick & 0x40 != 0 {
        let _ = media::preview(&file, &d.join("preview.mp4"));
    }

    // Recovery keeps the original byte for byte, under one of three names.
    let stem = "Game 2026-11-19 12.00.00";
    let part = d.join(format!("{stem}.{ext}.recording.part"));
    fs::write(&part, bytes).unwrap();
    let result = recovery::recover(&part, None);
    let kept = [
        part.clone(),
        d.join(format!("{stem}.{ext}{}", recovery::RECOVERED_PART)),
        d.join(format!("{stem}.{ext}{}", recovery::UNRECOVERABLE_PART)),
    ];
    let found: Vec<_> = kept.iter().filter(|p| p.exists()).collect();
    assert_eq!(found.len(), 1, "{result:?}: {found:?}");
    assert!(fs::read(found[0]).unwrap() == bytes, "{result:?}: the original changed");
    for e in fs::read_dir(d).unwrap().flatten() {
        let _ = fs::remove_file(e.path());
    }
});
