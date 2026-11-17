//! Colour and range (testing.md): a known pattern (100% colour bars, grey steps including the
//! full-range edges 0 and 255, and a 0–255 grey ramp) is encoded, decoded, and must read back as
//! BT.709 limited range, the colour every Backsight file is tagged with.
//!
//! The check is calibrated with controlled samples: the same pattern written in each of the
//! usual mistakes (BT.601 matrix, full range, limited range squeezed twice) goes through the same
//! encoder, and each must be caught and named, while the correct one passes. The x264 half runs
//! in CI (tier 1); the NVENC half (tier 2) adds the app's real GPU path, the D3D11 video
//! processor converting a 1440p BGRA texture to 1080p NV12 straight into NVENC:
//! `cargo test --release -p backsight-engine --test colour -- --ignored --nocapture`.
//! Receipts are synthetic: the pattern starts as a texture, not a WGC capture of a screen.

use backsight_encode::{
    Catalog,
    catalog::resolve,
    mux::{Muxer, Track, needs_global_header},
    session::VideoSession,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The output: Backsight's default 1080p recording size.
const W: usize = 1920;
const H: usize = 1080;
const FPS: u32 = 60;
/// A still picture: enough frames for the encoder's first keyframe to be followed by P/B-frames.
const FRAMES: u32 = 12;

/// Largest colour error of any bar or grey step, as CIE76 ΔE between the source sRGB and the
/// decoded picture read as BT.709 limited range. Calibrated with x264:
/// correct samples read ΔE ≤ 0.55 (the GPU path 0.39), the mildest wrong one (full range) 6.95.
/// About 2.3 is one just-noticeable difference.
const MAX_DELTA_E: f64 = 3.0;
/// The 0–255 ramp must decode as `Y = 16 + 219/255·v` (limited range); full range is slope 1.0,
/// squeezed twice 0.74. These bounds sit well inside both.
const RAMP_SLOPE: (f64, f64) = (0.83, 0.89);
const RAMP_BLACK: (f64, f64) = (13.0, 19.0);

const BARS: [[u8; 3]; 8] =
    [[255, 255, 255], [255, 255, 0], [0, 255, 255], [0, 255, 0], [255, 0, 255], [255, 0, 0], [0, 0, 255], [0, 0, 0]];
/// Includes the full-range edges: 0..16 and 235..255 must stay distinct, not clip.
const STEPS: [u8; 8] = [0, 16, 32, 64, 128, 192, 235, 255];

/// The pattern at a normalised position: bars on top, grey steps, then a ramp along the bottom.
fn pattern(x: f64, y: f64) -> [u8; 3] {
    let col = ((x * 8.0) as usize).min(7);
    if y < 0.4 {
        BARS[col]
    } else if y < 0.6 {
        [STEPS[col]; 3]
    } else {
        [ramp(x).round() as u8; 3]
    }
}

fn ramp(x: f64) -> f64 {
    (x * 255.0).clamp(0.0, 255.0)
}

/// How RGB became YCbCr: the correct way and the usual mistakes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Encoding {
    Bt709Limited,
    Bt601Limited,
    Bt709Full,
    /// RGB already squeezed to 16–235, then squeezed again: the classic washed-out picture.
    LimitedTwice,
}

const ENCODINGS: [Encoding; 4] =
    [Encoding::Bt709Limited, Encoding::Bt601Limited, Encoding::Bt709Full, Encoding::LimitedTwice];

impl Encoding {
    fn yuv(self, rgb: [u8; 3]) -> [f64; 3] {
        let (kr, kb) = if self == Encoding::Bt601Limited { (0.299, 0.114) } else { (0.2126, 0.0722) };
        let mut c = rgb.map(|v| f64::from(v) / 255.0);
        if self == Encoding::LimitedTwice {
            c = c.map(|v| (16.0 + 219.0 * v) / 255.0);
        }
        let y = kr * c[0] + (1.0 - kr - kb) * c[1] + kb * c[2];
        let (cb, cr) = ((c[2] - y) / (2.0 * (1.0 - kb)), (c[0] - y) / (2.0 * (1.0 - kr)));
        let yuv = if self == Encoding::Bt709Full {
            [255.0 * y, 128.0 + 255.0 * cb, 128.0 + 255.0 * cr]
        } else {
            [16.0 + 219.0 * y, 128.0 + 224.0 * cb, 128.0 + 224.0 * cr]
        };
        yuv.map(|v| v.clamp(0.0, 255.0))
    }
}

/// BT.709 limited YCbCr back to sRGB (0..1), as a player reads a file tagged like ours.
fn bt709_limited_to_rgb([y, cb, cr]: [f64; 3]) -> [f64; 3] {
    let (kr, kb) = (0.2126, 0.0722);
    let (y, cb, cr) = ((y - 16.0) / 219.0, (cb - 128.0) / 224.0, (cr - 128.0) / 224.0);
    let (r, b) = (y + 2.0 * (1.0 - kr) * cr, y + 2.0 * (1.0 - kb) * cb);
    let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
    [r, g, b].map(|v| v.clamp(0.0, 1.0))
}

/// CIE76 ΔE between two sRGB colours (0..1), D65.
fn delta_e(a: [f64; 3], b: [f64; 3]) -> f64 {
    fn lab(rgb: [f64; 3]) -> [f64; 3] {
        let [r, g, b] = rgb.map(|v| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) });
        let xyz = [
            (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047,
            0.2126 * r + 0.7152 * g + 0.0722 * b,
            (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883,
        ];
        let f = xyz.map(|t| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 });
        [116.0 * f[1] - 16.0, 500.0 * (f[0] - f[1]), 200.0 * (f[1] - f[2])]
    }
    let (a, b) = (lab(a), lab(b));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// What came back out of a file.
struct Reading {
    /// Each bar and step: its source colour and the decoded mean (Y, Cb, Cr) of its centre.
    patches: Vec<([u8; 3], [f64; 3])>,
    /// Least-squares fit of decoded Y along the ramp: (Y at v = 0, slope per RGB step).
    ramp: (f64, f64),
}

impl Reading {
    fn max_delta_e(&self) -> f64 {
        let src = |rgb: [u8; 3]| rgb.map(|v| f64::from(v) / 255.0);
        self.patches.iter().map(|(rgb, yuv)| delta_e(src(*rgb), bt709_limited_to_rgb(*yuv))).fold(0.0, f64::max)
    }

    /// Root-mean-square distance in code values from what `e` would have produced.
    fn rms(&self, e: Encoding) -> f64 {
        let sum: f64 = self
            .patches
            .iter()
            .map(|(rgb, got)| e.yuv(*rgb).iter().zip(got).map(|(w, g)| (w - g).powi(2)).sum::<f64>())
            .sum();
        (sum / (3 * self.patches.len()) as f64).sqrt()
    }

    /// Which encoding the decoded picture is closest to.
    fn nearest(&self) -> Encoding {
        ENCODINGS.into_iter().min_by(|a, b| self.rms(*a).total_cmp(&self.rms(*b))).unwrap()
    }

    /// `Ok` when the file would look right in a player; otherwise what went wrong, in words.
    fn verdict(&self) -> Result<(), String> {
        let (de, (black, slope), nearest) = (self.max_delta_e(), self.ramp, self.nearest());
        let in_range = |v: f64, (lo, hi): (f64, f64)| (lo..=hi).contains(&v);
        if de <= MAX_DELTA_E && in_range(slope, RAMP_SLOPE) && in_range(black, RAMP_BLACK) {
            return Ok(());
        }
        Err(format!(
            "colours are off (largest ΔE {de:.1}, ramp black {black:.1}, slope {slope:.3}); the picture looks like {nearest:?}"
        ))
    }

    fn receipt(&self) -> Value {
        json!({
            "max_delta_e": round2(self.max_delta_e()),
            "ramp_black": round2(self.ramp.0),
            "ramp_slope": (self.ramp.1 * 1000.0).round() / 1000.0,
            "nearest": format!("{:?}", self.nearest()),
            "rms": ENCODINGS.iter().map(|e| (format!("{e:?}"), json!(round2(self.rms(*e))))).collect::<serde_json::Map<_, _>>(),
        })
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn ffmpeg_tool(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ffmpeg/bin").join(name)
}

fn run(name: &str, args: &[&str]) -> Vec<u8> {
    let out = Command::new(ffmpeg_tool(name)).args(["-v", "error"]).args(args).output().unwrap();
    assert!(out.status.success(), "{name} failed: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// Checks the file says BT.709 limited range, then decodes its last frame and measures it.
fn read_back(path: &Path) -> Reading {
    let file = path.to_str().unwrap();
    let info: Value = serde_json::from_slice(&run(
        "ffprobe.exe",
        &[
            "-of",
            "json",
            "-show_entries",
            "stream=pix_fmt,color_range,color_space,color_transfer,color_primaries",
            file,
        ],
    ))
    .unwrap();
    let s = &info["streams"][0];
    let tags = [&s["pix_fmt"], &s["color_range"], &s["color_space"], &s["color_transfer"], &s["color_primaries"]];
    assert_eq!(tags, ["yuv420p", "tv", "bt709", "bt709", "bt709"], "{} is tagged {s}", path.display());

    // Already yuv420p and limited range, so FFmpeg hands the decoded planes over unconverted.
    let select = format!("select=eq(n\\,{})", FRAMES - 1);
    let raw = run(
        "ffmpeg.exe",
        &["-i", file, "-vf", &select, "-frames:v", "1", "-pix_fmt", "yuv420p", "-f", "rawvideo", "-"],
    );
    assert_eq!(raw.len(), W * H * 3 / 2, "one decoded frame");
    let (y, uv) = raw.split_at(W * H);
    let (u, v) = uv.split_at(W * H / 4);

    // A patch's centre half, away from edges and the chroma that bleeds across them.
    let mean = |plane: &[u8], stride: usize, x: (f64, f64), y: (f64, f64)| {
        let rows = plane.len() / stride;
        let (x0, x1) = ((x.0 * stride as f64) as usize, (x.1 * stride as f64) as usize);
        let (y0, y1) = ((y.0 * rows as f64) as usize, (y.1 * rows as f64) as usize);
        let sum: u64 = (y0..y1).flat_map(|r| &plane[r * stride + x0..r * stride + x1]).map(|&p| u64::from(p)).sum();
        sum as f64 / ((x1 - x0) * (y1 - y0)) as f64
    };
    let mut patches = Vec::new();
    for (row, band) in [(0.05, 0.35), (0.425, 0.575)].into_iter().enumerate() {
        for col in 0..8 {
            let x = ((col as f64 + 0.25) / 8.0, (col as f64 + 0.75) / 8.0);
            let rgb = if row == 0 { BARS[col] } else { [STEPS[col]; 3] };
            patches.push((rgb, [mean(y, W, x, band), mean(u, W / 2, x, band), mean(v, W / 2, x, band)]));
        }
    }
    // Ramp: column means of Y against the source value, ends left out (scaling smears them).
    let points: Vec<(f64, f64)> = (W / 20..W - W / 20)
        .map(|c| {
            let rows = H * 65 / 100..H * 95 / 100;
            let column = rows.clone().map(|r| f64::from(y[r * W + c])).sum::<f64>() / rows.len() as f64;
            (ramp((c as f64 + 0.5) / W as f64), column)
        })
        .collect();
    let n = points.len() as f64;
    let (mx, my) = (points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n);
    let slope = points.iter().map(|(x, y)| (x - mx) * (y - my)).sum::<f64>()
        / points.iter().map(|(x, _)| (x - mx).powi(2)).sum::<f64>();
    Reading { patches, ramp: (my - slope * mx, slope) }
}

/// Encodes the pattern, converted to YCbCr in software the way `encoding` says, with `encoder`
/// at its catalog defaults (system-memory frames, as the encoder probe feeds NVENC).
fn encode_controlled(dir: &Path, encoder: &str, encoding: Encoding) -> PathBuf {
    let c = Catalog::get();
    let enc = c.video_encoder(encoder).unwrap();
    let container = c.container("mp4").unwrap();
    let values = resolve(&c.video_options(enc), None);
    let mut video = VideoSession::open(enc, &values, W as u32, H as u32, FPS, needs_global_header(container)).unwrap();
    let path = dir.join(format!("{encoder}-{encoding:?}.mp4"));
    let mut mux = Muxer::create(&path, container, &video.params(), &[]).unwrap();

    let px = |x: usize, y: usize| encoding.yuv(pattern((x as f64 + 0.5) / W as f64, (y as f64 + 0.5) / H as f64));
    let luma: Vec<u8> = (0..W * H).map(|i| px(i % W, i / W)[0].round() as u8).collect();
    // 4:2:0: each chroma sample is the mean of its 2×2 pixels.
    let chroma: Vec<[u8; 2]> = (0..W * H / 4)
        .map(|i| {
            let (x, y) = (i % (W / 2) * 2, i / (W / 2) * 2);
            let s = [px(x, y), px(x + 1, y), px(x, y + 1), px(x + 1, y + 1)];
            [1, 2].map(|k| (s.iter().map(|p| p[k]).sum::<f64>() / 4.0).round() as u8)
        })
        .collect();
    for pts in 0..FRAMES {
        let nv12 = video.is_nv12();
        let [y, u, v] = video.planes_mut().unwrap();
        let (y, stride) = y.unwrap();
        for row in 0..H {
            y[row * stride..][..W].copy_from_slice(&luma[row * W..][..W]);
        }
        let cw = W / 2;
        match (u, v) {
            (Some((uv, s)), None) if nv12 => {
                for row in 0..H / 2 {
                    for x in 0..cw {
                        uv[row * s + 2 * x..][..2].copy_from_slice(&chroma[row * cw + x]);
                    }
                }
            }
            (Some((u, us)), Some((v, vs))) => {
                for row in 0..H / 2 {
                    for x in 0..cw {
                        [u[row * us + x], v[row * vs + x]] = chroma[row * cw + x];
                    }
                }
            }
            _ => unreachable!("NV12 or yuv420p planes"),
        }
        video.send(i64::from(pts), &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    }
    video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    mux.finish().unwrap()
}

/// Each controlled sample must decode as the encoding it was written in, and only the correct
/// one may pass: proof the thresholds sit between right and wrong for this encoder.
fn check_controls(dir: &Path, encoder: &str) -> Value {
    let mut receipts = serde_json::Map::new();
    for encoding in ENCODINGS {
        let path = encode_controlled(dir, encoder, encoding);
        let reading = read_back(&path);
        let receipt = reading.receipt();
        println!("{encoder} controlled {encoding:?}: {receipt}");
        assert_eq!(reading.nearest(), encoding, "{encoder}: the {encoding:?} sample wasn't recognised");
        let verdict = reading.verdict();
        if encoding == Encoding::Bt709Limited {
            assert_eq!(verdict, Ok(()), "{encoder}: the correct sample fails");
        } else {
            assert!(verdict.is_err(), "{encoder}: the {encoding:?} sample passes ({receipt})");
        }
        receipts.insert(format!("{encoding:?}"), receipt);
    }
    receipts.into()
}

#[test]
fn x264_colour_controls_are_told_apart() {
    let dir = tempfile::Builder::new().prefix("backsight-colour-").tempdir().unwrap();
    let receipts = check_controls(dir.path(), "x264");
    println!("BACKSIGHT_COLOUR {}", json!({"receipt": "synthetic", "encoder": "x264", "controls": receipts}));
}

#[test]
#[ignore = "needs an NVIDIA GPU"]
fn the_gpu_path_keeps_bt709_limited_colour() {
    use backsight_engine::capture::{convert::Converter, d3d::Gpu};
    use windows::Win32::Graphics::{Direct3D11::*, Dxgi::Common::*};

    let dir = tempfile::Builder::new().prefix("backsight-colour-").tempdir().unwrap();
    let controls = check_controls(dir.path(), "nvenc-h264");

    let gpu = Gpu::open("nvidia").unwrap();
    assert_eq!(gpu.vendor_id, 0x10DE, "needs an NVIDIA GPU, got {}", gpu.name);
    // The common case: a 1440p desktop recorded at 1080p, scaled in the same conversion pass.
    let input = (2560u32, 1440u32);
    let (iw, ih) = (input.0 as usize, input.1 as usize);
    let bgra: Vec<u8> = (0..iw * ih)
        .flat_map(|i| {
            let [r, g, b] = pattern(((i % iw) as f64 + 0.5) / iw as f64, ((i / iw) as f64 + 0.5) / ih as f64);
            [b, g, r, 255]
        })
        .collect();
    let desc = D3D11_TEXTURE2D_DESC {
        Width: input.0,
        Height: input.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        // As WGC's pool textures.
        BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
        ..Default::default()
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: bgra.as_ptr().cast(), SysMemPitch: input.0 * 4, SysMemSlicePitch: 0 };
    let mut src = None;
    // SAFETY: `bgra` outlives the call and matches `desc`.
    unsafe { gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut src)) }.unwrap();
    let src = src.unwrap();

    let c = Catalog::get();
    let enc = c.video_encoder("nvenc-h264").unwrap();
    let container = c.container("mp4").unwrap();
    let values = resolve(&c.video_options(enc), None);
    let output = (W as u32, H as u32);
    let mut conv = Converter::new(&gpu, input, output, FPS).unwrap();
    let mut video =
        VideoSession::open_d3d11(enc, &values, output, FPS, needs_global_header(container), gpu.for_ffmpeg()).unwrap();
    let path = dir.path().join("gpu.mp4");
    let mut mux = Muxer::create(&path, container, &video.params(), &[]).unwrap();
    for pts in 0..FRAMES {
        assert!(conv.convert_from(&gpu, &src, input).unwrap());
        conv.copy_to_encoder(&gpu, video.next_gpu_frame().unwrap());
        let _lock = gpu.lock(); // NVENC uses our device inside send (platform-notes.md)
        video.send(i64::from(pts), &mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    }
    {
        let _lock = gpu.lock();
        video.finish(&mut |p| mux.write(Track::Video, &p).unwrap()).unwrap();
    }
    let saved = mux.finish().unwrap();
    let reading = read_back(&saved);
    let receipt = reading.receipt();
    println!(
        "BACKSIGHT_COLOUR {}",
        json!({
            "receipt": "synthetic pattern through the real GPU converter and NVENC (no WGC)",
            "gpu": gpu.name, "input": [input.0, input.1], "output": [output.0, output.1],
            "gpu_path": receipt, "nvenc_controls": controls,
        })
    );
    assert_eq!(reading.verdict(), Ok(()), "GPU path: {receipt}");
}
