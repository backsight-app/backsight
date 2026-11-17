//! Encoder quality floors (testing.md, tier 2): a generated 1080p60 reference (moving test chart
//! with text and edges, smooth gradients, film-grain noise) goes through each encoder at its
//! catalog defaults, and PSNR, SSIM and VMAF (when the linked FFmpeg has libvmaf) against the
//! reference must clear fixed floors. Scores, never byte- or bitrate-exact goldens: hardware
//! output changes with every driver.
//!
//! The floors are calibrated with controlled samples run every time: a near-lossless x264 encode
//! must pass, and deliberately bad encodes (1 Mbps, a BT.601 matrix in a file tagged BT.709, a
//! half-resolution picture stretched back up) must each fail at least one floor. Receipts are
//! synthetic: generated pictures, the real encoders.
//! `cargo test --release -p backsight-encode --test quality -- encoders_at_their_defaults_clear_calibrated_quality_floors --exact --ignored --nocapture`

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::VideoSession,
};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const W: usize = 1920;
const H: usize = 1080;
const FPS: u32 = 60;
/// Frames per part of the reference (chart, gradients, grain): 4 s in all.
const PART: u32 = 80;
const FRAMES: u32 = 3 * PART;

/// Floors for this reference only (scores depend on the content). Each sits midway between the
/// best bad control and the worst sample that must pass, measured on an NVIDIA card
/// (testing.md has every receipt): PSNR-Y bad ≤ 34.84 dB (half size), pass ≥ 39.11 (x264
/// default); SSIM bad ≤ 0.9311, pass ≥ 0.9379 (the narrowest gap: grain dominates SSIM); VMAF
/// bad ≤ 85.15, pass ≥ 92.85. The controls rerun every time, so a floor that stops separating
/// them fails the test instead of passing silently.
const PSNR_Y_FLOOR: f64 = 37.0;
const SSIM_FLOOR: f64 = 0.934;
const VMAF_FLOOR: f64 = 89.0;

/// How the encoder's input differs from the reference.
#[derive(Clone, Copy, PartialEq)]
enum Input {
    Same,
    /// Converted with the BT.601 matrix, while the file says BT.709.
    Bt601,
    /// Half the resolution, stretched back to 1080p.
    HalfSize,
}

/// The reference as an FFmpeg filter graph: deterministic, so it is generated again for the
/// comparison instead of being stored. Explicit colour conversion, since swscale's own default
/// for RGB to YUV is the BT.601 matrix, and timestamps renumbered by frame: concat's own pair
/// frames off by one against the decoded file (platform-notes.md).
fn graph(input: Input) -> String {
    let s = format!("size={W}x{H}:rate={FPS}");
    let part = format!("trim=end_frame={PART},format=rgb24");
    let stretch =
        if input == Input::HalfSize { format!("scale={}:{},scale={W}:{H},", W / 2, H / 2) } else { String::new() };
    let matrix = if input == Input::Bt601 { "bt601" } else { "bt709" };
    format!(
        "testsrc2={s},{part}[a];\
         gradients={s}:c0=0x203060:c1=0xe0b070:x0=0:y0=0:x1={x1}:y1={y1}:seed=1:speed=0.02,{part}[b];\
         testsrc2={s},noise=alls=12:allf=t:all_seed=7,{part}[c];\
         [a][b][c]concat=n=3:v=1:a=0,settb=1/{FPS},setpts=N,{stretch}scale=out_color_matrix={matrix}:out_range=tv,format=yuv420p",
        x1 = W - 1,
        y1 = H - 1,
    )
}

fn ffmpeg() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin/ffmpeg.exe")
}

/// Encodes the reference (as changed by `input`) with `encoder` at its catalog defaults plus
/// `options`. `None` when this machine can't open the encoder.
fn encode(dir: &Path, name: &str, encoder: &str, options: &[(&str, toml::Value)], input: Input) -> Option<PathBuf> {
    let c = Catalog::get();
    let enc = c.video_encoder(encoder).unwrap();
    let container = c.container("mp4").unwrap();
    let saved: backsight_config::EncoderOptions = options.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    let values = resolve(&c.video_options(enc), Some(&saved));
    for (k, v) in options {
        assert_eq!(values.get(*k), Some(v), "{encoder} rejects {k} = {v}");
    }
    let mut video = match VideoSession::open(enc, &values, W as u32, H as u32, FPS, needs_global_header(container)) {
        Ok(v) => v,
        Err(e) => {
            println!("{name}: skipped, {encoder} doesn't open here: {e}");
            return None;
        }
    };
    let path = dir.join(format!("{name}.mp4"));
    let mut mux = Muxer::create(&path, container, &video.params(), &[]).unwrap();
    let pix_fmt = if video.is_nv12() { "nv12" } else { "yuv420p" };
    let mut source = Command::new(ffmpeg())
        .args(["-v", "error", "-f", "lavfi", "-i", &graph(input), "-frames:v", &FRAMES.to_string()])
        .args(["-pix_fmt", pix_fmt, "-f", "rawvideo", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = source.stdout.take().unwrap();
    let mut frame = vec![0u8; W * H * 3 / 2];
    for pts in 0..FRAMES {
        stdout.read_exact(&mut frame).unwrap();
        let (y_src, chroma) = frame.split_at(W * H);
        let planes = video.planes_mut().unwrap();
        // NV12: Y, then UV rows as wide as Y. yuv420p: Y, U, V, chroma rows half as wide.
        let sources: [(&[u8], usize, usize); 3] = if pix_fmt == "nv12" {
            [(y_src, W, H), (chroma, W, H / 2), (&[], 0, 0)]
        } else {
            let (u, v) = chroma.split_at(W * H / 4);
            [(y_src, W, H), (u, W / 2, H / 2), (v, W / 2, H / 2)]
        };
        for (plane, (src, width, rows)) in planes.into_iter().zip(sources) {
            let Some((dst, stride)) = plane else { continue };
            for r in 0..rows {
                dst[r * stride..][..width].copy_from_slice(&src[r * width..][..width]);
            }
        }
        video.send(i64::from(pts), &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    }
    assert!(source.wait().unwrap().success(), "the reference generator failed");
    video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    Some(mux.finish().unwrap())
}

struct Scores {
    psnr_y: f64,
    ssim: f64,
    vmaf: Option<f64>,
}

impl Scores {
    /// The floors this sample is under, by name.
    fn failures(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.psnr_y < PSNR_Y_FLOOR {
            out.push("PSNR");
        }
        if self.ssim < SSIM_FLOOR {
            out.push("SSIM");
        }
        if self.vmaf.is_some_and(|v| v < VMAF_FLOOR) {
            out.push("VMAF");
        }
        out
    }
}

/// The number after `key` on the first line of FFmpeg's log that contains `tag`.
fn score(log: &str, tag: &str, key: &str) -> f64 {
    let line = log.lines().find(|l| l.contains(tag)).unwrap_or_else(|| panic!("no {tag} line in:\n{log}"));
    let rest = &line[line.find(key).unwrap_or_else(|| panic!("no {key} in {line}")) + key.len()..];
    rest.split(|c: char| c != '.' && !c.is_ascii_digit()).next().unwrap().parse().unwrap()
}

/// Compares a decoded sample with the reference, frame by frame (both numbered 0, 1, 2…, so
/// frames pair by position). libvmaf takes the distorted video first (FFmpeg filters docs).
fn measure(path: &Path, vmaf: bool) -> Scores {
    let n = if vmaf { 3 } else { 2 };
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut lavfi = format!(
        "[0:v]settb=1/{FPS},setpts=N,split={n}[d0][d1]{d2};[1:v]split={n}[r0][r1]{r2};\
         [d0][r0]psnr[p];[d1][r1]ssim[s]",
        d2 = if vmaf { "[d2]" } else { "" },
        r2 = if vmaf { "[r2]" } else { "" },
    );
    if vmaf {
        lavfi += &format!(";[d2][r2]libvmaf=n_threads={threads}[v]");
    }
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-hide_banner", "-nostats", "-i"]).arg(path).args(["-f", "lavfi", "-i", &graph(Input::Same)]);
    cmd.args(["-lavfi", &lavfi]);
    for out in ["[p]", "[s]", "[v]"].into_iter().take(n) {
        cmd.args(["-map", out, "-f", "null", "-"]);
    }
    let out = cmd.output().unwrap();
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "comparison failed:\n{log}");
    Scores {
        psnr_y: score(&log, "PSNR y:", "y:"),
        ssim: score(&log, "SSIM Y:", "All:"),
        vmaf: vmaf.then(|| score(&log, "VMAF score:", "VMAF score: ")),
    }
}

fn frame_count(path: &Path) -> u64 {
    let exe = ffmpeg().with_file_name("ffprobe.exe");
    let out = Command::new(exe)
        .args(["-v", "error", "-of", "json", "-show_entries", "stream=nb_frames"])
        .arg(path)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["streams"][0]["nb_frames"].as_str().unwrap().parse().unwrap()
}

#[derive(PartialEq)]
enum Role {
    /// Must clear every floor: proof the floors aren't too strict.
    GoodControl,
    /// Must fail at least one floor: proof the floors catch a visible problem.
    BadControl,
    /// What ships: an encoder at its defaults.
    Default,
}

/// Name, role, encoder, options over its defaults, input.
type Sample<'a> = (&'a str, Role, &'a str, Vec<(&'a str, toml::Value)>, Input);

fn check_samples(samples: Vec<Sample<'_>>) {
    let dir = tempfile::Builder::new().prefix("backsight-quality-").tempdir().unwrap();
    let filters = Command::new(ffmpeg()).args(["-hide_banner", "-filters"]).output().unwrap();
    assert!(filters.status.success(), "could not list FFmpeg filters");
    let vmaf = String::from_utf8_lossy(&filters.stdout).lines().any(|l| l.split_whitespace().nth(1) == Some("libvmaf"));
    if !vmaf {
        // Backsight's slim FFmpeg (what ships) has no libvmaf; PSNR and SSIM still gate.
        println!("VMAF floor skipped: the linked FFmpeg has no libvmaf filter. Scoring PSNR and SSIM only.");
    }

    let mut problems = Vec::new();
    for (name, role, encoder, options, input) in samples {
        let Some(path) = encode(dir.path(), name, encoder, &options, input) else {
            assert!(encoder == "nvenc-hevc", "{name} must run");
            continue;
        };
        assert_eq!(frame_count(&path), u64::from(FRAMES), "{name}: frames lost");
        let bytes = std::fs::metadata(&path).unwrap().len();
        let s = measure(&path, vmaf);
        let failures = s.failures();
        println!(
            "BACKSIGHT_QUALITY {}",
            json!({
                "receipt": "synthetic", "sample": name, "encoder": encoder, "mbps": (bytes as f64 * 8.0 / 4.0 / 1e4).round() / 100.0,
                "psnr_y": (s.psnr_y * 100.0).round() / 100.0, "ssim": (s.ssim * 10000.0).round() / 10000.0,
                "vmaf": s.vmaf.map(|v| (v * 100.0).round() / 100.0), "below_floors": failures,
            })
        );
        match role {
            Role::BadControl if failures.is_empty() => problems.push(format!("{name} passes every floor")),
            Role::GoodControl | Role::Default if !failures.is_empty() => {
                problems.push(format!("{name} is under the {failures:?} floor"));
            }
            _ => {}
        }
    }
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
#[ignore = "CPU-only FFmpeg quality comparison; takes a few minutes"]
fn x264_quality_floors_catch_wrong_matrix() {
    check_samples(vec![
        ("x264-default", Role::Default, "x264", vec![], Input::Same),
        ("x264-bt601-matrix", Role::BadControl, "x264", vec![], Input::Bt601),
    ]);
}

#[test]
#[ignore = "needs an NVIDIA GPU; takes a few minutes"]
fn encoders_at_their_defaults_clear_calibrated_quality_floors() {
    let int = |v: i64| toml::Value::Integer(v);
    let str = |v: &str| toml::Value::String(v.into());
    check_samples(vec![
        ("x264-near-lossless", Role::GoodControl, "x264", vec![("rc", str("cq")), ("cq", int(10))], Input::Same),
        ("nvenc-1mbps", Role::BadControl, "nvenc-h264", vec![("rc", str("cbr")), ("bitrate", int(1000))], Input::Same),
        ("nvenc-bt601-matrix", Role::BadControl, "nvenc-h264", vec![], Input::Bt601),
        ("nvenc-half-size", Role::BadControl, "nvenc-h264", vec![], Input::HalfSize),
        ("nvenc-h264-default", Role::Default, "nvenc-h264", vec![], Input::Same),
        ("nvenc-hevc-default", Role::Default, "nvenc-hevc", vec![], Input::Same),
        ("x264-default", Role::Default, "x264", vec![], Input::Same),
    ]);
}
