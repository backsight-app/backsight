//! A file that stops accepting writes mid-save (testing.md, tier 1), as a full disk or an
//! unplugged drive does: another handle locks the whole `.part`, so FFmpeg's writes fail. The
//! save must fail with a plain error and leave nothing behind, never panic (release builds abort,
//! losing the replay buffer).

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::VideoSession,
};
use std::fs::File;

const FPS: u32 = 30;

/// Writes 3 s of video into an MKV clip whose `.part` is locked after its header, then finishes
/// it (or just drops it). Returns the finish error, if it was finished.
fn save_locked(name: &str, finish: bool) -> Option<String> {
    let c = Catalog::get();
    let container = c.container("mkv").unwrap();
    let venc = c.video_encoder("x264").unwrap();
    let gh = needs_global_header(container);
    let mut video = VideoSession::open(venc, &resolve(&c.video_options(venc), None), 256, 144, FPS, gh).unwrap();
    let path = std::env::temp_dir().join(format!("backsight-write-failure-{}-{name}.mkv", std::process::id()));
    let mut mux = Muxer::create(&path, container, &video.params(), &[]).unwrap();
    let part = mux.part().to_path_buf();
    let lock = File::open(&part).unwrap();
    lock.lock().unwrap();

    let mut failed = false;
    for i in 0..3 * FPS {
        for (n, (data, stride)) in video.planes_mut().unwrap().into_iter().flatten().enumerate() {
            for (row, line) in data.chunks_mut(stride).enumerate() {
                line.iter_mut()
                    .enumerate()
                    .for_each(|(x, p)| *p = if n == 0 { (x + row) as u8 ^ i as u8 } else { 128 });
            }
        }
        video.send(i.into(), &mut |p| failed |= mux.write(Track::Video, &p).is_err()).unwrap();
    }
    let result = if finish {
        Some(mux.finish().expect_err("a save whose writes fail must fail"))
    } else {
        drop(mux); // closing must not panic either
        None
    };
    drop(lock);
    assert!(!path.exists(), "{name}: nothing published");
    assert!(!part.exists(), "{name}: the failed clip's .part is removed");
    println!("{name}: a write failed before finish: {failed}");
    result
}

#[test]
fn a_save_whose_writes_fail_reports_it_and_leaves_nothing() {
    let e = save_locked("finished", true).unwrap();
    assert!(!e.is_empty());
    save_locked("dropped", false);
}
