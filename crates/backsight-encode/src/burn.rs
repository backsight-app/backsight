//! Captions burned into a vertical clip's picture (ADR 0015): the clip's `.srt` is read once, each
//! caption is drawn with DirectWrite when it first shows (white, black outline, bottom centre),
//! then blended into the encoder's frames. FFmpeg's `subtitles` filter would need libass, which
//! the slim build doesn't have.

use crate::{screenshot::Apartment, session::Planes};
use std::{f32::consts::TAU, path::Path};
use windows::{
    Win32::{
        Graphics::{
            Direct2D::{
                Common::{D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT},
                D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_RENDER_TARGET_PROPERTIES,
                D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Factory,
                ID2D1RenderTarget, ID2D1SolidColorBrush,
            },
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_FAR,
                DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory,
                IDWriteTextFormat,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Imaging::{
                CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory,
                WICBitmapCacheOnLoad,
            },
        },
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::w,
};

/// One caption, shown from `start_us` up to `end_us` (clip time).
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start_us: i64,
    pub end_us: i64,
    pub text: String,
}

/// The cues of a SubRip file, lines of one caption joined (DirectWrite wraps them to the width).
/// Blocks that don't parse are skipped.
pub fn parse_srt(srt: &str) -> Vec<Cue> {
    let srt = srt.replace("\r\n", "\n");
    let mut cues = Vec::new();
    for block in srt.split("\n\n") {
        let mut lines = block.lines().map(str::trim).filter(|l| !l.is_empty());
        let Some(mut timing) = lines.next() else { continue };
        if !timing.contains("-->") {
            let Some(next) = lines.next() else { continue }; // the cue's number came first
            timing = next;
        }
        let Some((a, b)) = timing.split_once("-->") else { continue };
        let (Some(start_us), Some(end_us)) = (srt_time(a), srt_time(b)) else { continue };
        let text = lines.collect::<Vec<_>>().join(" ");
        if !text.is_empty() && end_us > start_us {
            cues.push(Cue { start_us, end_us, text });
        }
    }
    cues
}

/// `HH:MM:SS,mmm` in microseconds.
fn srt_time(t: &str) -> Option<i64> {
    let (hms, ms) = t.trim().split_once([',', '.'])?;
    let mut parts = hms.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
    Some(((h * 60 + m) * 60 + s) * 1_000_000 + ms.parse::<i64>().ok()? * 1000)
}

/// Sample levels of the encoder's frames (limited range): black, white, neutral chroma.
struct Levels {
    black: u32,
    white: u32,
    neutral: u32,
    /// 16-bit samples (P010), the value in the top 10 bits.
    wide: bool,
}

const SDR: Levels = Levels { black: 16, white: 235, neutral: 128, wide: false };
/// PQ: caption white at about 203 nits (HDR's reference white), not the format's 10,000.
const HDR: Levels = Levels { black: 64, white: 572, neutral: 512, wide: true };

impl Levels {
    fn get(&self, row: &[u8], i: usize) -> u32 {
        if self.wide { u32::from(u16::from_le_bytes([row[2 * i], row[2 * i + 1]])) >> 6 } else { u32::from(row[i]) }
    }

    fn set(&self, row: &mut [u8], i: usize, v: u32) {
        if self.wide {
            row[2 * i..2 * i + 2].copy_from_slice(&((v << 6) as u16).to_le_bytes());
        } else {
            row[i] = v as u8;
        }
    }
}

/// A drawn caption: `[premultiplied grey, alpha]` per pixel for whole frame rows from `top`, and
/// the alpha of each 2×2 block for the half-size chroma planes from `top / 2`.
struct Overlay {
    top: usize,
    width: usize,
    px: Vec<[u8; 2]>,
    chroma: Vec<u8>,
}

impl Overlay {
    fn new(top: usize, width: usize, px: Vec<[u8; 2]>) -> Overlay {
        let rows = px.len() / width.max(1);
        let (cw, ctop) = (width.div_ceil(2), top / 2);
        let crows = (top + rows).div_ceil(2) - ctop;
        let alpha = |x: usize, y: usize| -> u32 {
            let inside = x < width && (top..top + rows).contains(&y);
            if inside { u32::from(px[(y - top) * width + x][1]) } else { 0 }
        };
        let chroma = (0..crows * cw)
            .map(|i| {
                let (x, y) = (2 * (i % cw), 2 * (ctop + i / cw));
                ((alpha(x, y) + alpha(x + 1, y) + alpha(x, y + 1) + alpha(x + 1, y + 1)) / 4) as u8
            })
            .collect();
        Overlay { top, width, px, chroma }
    }

    /// Blends into a frame's planes: Y and interleaved UV (NV12, P010), or Y, U and V (yuv420p).
    fn blend(&self, planes: &mut Planes, l: &Levels) {
        let bytes = if l.wide { 2 } else { 1 };
        if let Some((y, stride)) = &mut planes[0] {
            for (r, px) in self.px.chunks(self.width).enumerate() {
                let Some(row) = y.get_mut((self.top + r) * *stride..) else { break };
                for (x, &[grey, a]) in px.iter().enumerate().filter(|(_, p)| p[1] > 0) {
                    let (a, grey) = (u32::from(a), u32::from(grey));
                    let v = (l.get(row, x) * (255 - a) + l.black * a + grey * (l.white - l.black)) / 255;
                    l.set(row, x, v);
                }
            }
        }
        let cw = self.width.div_ceil(2);
        let interleaved = planes[2].is_none();
        for plane in planes[1..].iter_mut().flatten() {
            let (data, stride) = (&mut *plane.0, plane.1);
            for (r, alphas) in self.chroma.chunks(cw).enumerate() {
                let Some(row) = data.get_mut((self.top / 2 + r) * stride..) else { break };
                for (x, &a) in alphas.iter().enumerate().filter(|(_, a)| **a > 0) {
                    let a = u32::from(a);
                    let samples = if interleaved { 2 * x..2 * x + 2 } else { x..x + 1 };
                    let len = row.len();
                    for i in samples.filter(|i| (i + 1) * bytes <= len) {
                        l.set(row, i, (l.get(row, i) * (255 - a) + l.neutral * a) / 255);
                    }
                }
            }
        }
    }
}

/// Draws captions with Direct2D into a software bitmap the size of the caption band.
struct Painter {
    width: usize,
    /// The band's first frame row and its height: the lower part of the picture, ending 15% above
    /// the bottom so the phone apps' buttons don't cover it.
    top: usize,
    height: usize,
    size: f32,
    target: ID2D1RenderTarget,
    bitmap: IWICBitmap,
    format: IDWriteTextFormat,
    white: ID2D1SolidColorBrush,
    black: ID2D1SolidColorBrush,
    _com: Apartment,
}

impl Painter {
    fn new(width: usize, height: usize) -> windows::core::Result<Painter> {
        let com = Apartment::enter()?;
        let (top, band) = (height * 45 / 100, height * 40 / 100);
        let size = (width as f32 / 16.0).max(12.0);
        // SAFETY: COM is entered on this thread for the objects' lifetime (`_com` drops last);
        // every pointer passed lives through its call.
        unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let bitmap =
                wic.CreateBitmap(width as u32, band as u32, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let target = d2d.CreateWicBitmapRenderTarget(&bitmap, &props)?;
            // Only one colour channel is read (`draw`): no ClearType colour fringes.
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let format = dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_FAR)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let brush = |r, g, b| target.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a: 1.0 }, None);
            let (white, black) = (brush(1.0, 1.0, 1.0)?, brush(0.0, 0.0, 0.0)?);
            Ok(Painter { width, top, height: band, size, target, bitmap, format, white, black, _com: com })
        }
    }

    /// `text` in white with a black outline, centred, its last line at the band's bottom.
    fn draw(&self, text: &str) -> windows::core::Result<Overlay> {
        let text: Vec<u16> = text.encode_utf16().collect();
        let (w, h, outline) = (self.width as f32, self.height as f32, self.size / 10.0);
        let rect = |dx: f32, dy: f32| D2D_RECT_F {
            left: w * 0.05 + dx,
            top: outline + dy,
            right: w * 0.95 + dx,
            bottom: h - outline + dy,
        };
        let mut bgra = vec![0u8; self.width * self.height * 4];
        // SAFETY: drawing between BeginDraw and EndDraw on this thread; `bgra` holds the whole
        // bitmap at a stride of `width * 4`.
        unsafe {
            self.target.BeginDraw();
            self.target.Clear(Some(&D2D1_COLOR_F::default()));
            // The outline: the text in black at 16 points around a circle, the white on top.
            for k in 0..16 {
                let angle = k as f32 * TAU / 16.0;
                let r = rect(outline * angle.cos(), outline * angle.sin());
                let (fmt, black) = (&self.format, &self.black);
                self.target.DrawText(&text, fmt, &r, black, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            }
            let (fmt, white) = (&self.format, &self.white);
            let r = rect(0.0, 0.0);
            self.target.DrawText(&text, fmt, &r, white, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            self.target.EndDraw(None, None)?;
            self.bitmap.CopyPixels(std::ptr::null(), (self.width * 4) as u32, &mut bgra)?;
        }
        // Grey text: blue stands for all three channels. Only the rows with text are kept.
        let px: Vec<[u8; 2]> = bgra.as_chunks::<4>().0.iter().map(|p| [p[0], p[3]]).collect();
        let used = |r: &&[[u8; 2]]| r.iter().any(|p| p[1] > 0);
        let first = px.chunks(self.width).position(|r| used(&r)).unwrap_or(0);
        let last = px.chunks(self.width).rposition(|r| used(&r)).map_or(first, |l| l + 1);
        Ok(Overlay::new(self.top + first, self.width, px[first * self.width..last * self.width].to_vec()))
    }
}

/// The captions of one clip, burned into its vertical clip's frames as they are encoded.
pub struct Captions {
    cues: Vec<Cue>,
    painter: Option<Painter>,
    /// The cue drawn last and its picture.
    shown: Option<(usize, Overlay)>,
    failed: bool,
    burned: bool,
}

impl Captions {
    /// Reads the captions file `srt`; an error if it can't be read or holds no captions.
    pub fn load(srt: &Path) -> Result<Captions, String> {
        let text = std::fs::read_to_string(srt).map_err(|e| format!("can't read {}: {e}", srt.display()))?;
        Captions::new(parse_srt(&text)).ok_or_else(|| format!("{} has no captions", srt.display()))
    }

    fn new(cues: Vec<Cue>) -> Option<Captions> {
        (!cues.is_empty()).then_some(Captions { cues, painter: None, shown: None, failed: false, burned: false })
    }

    /// At least one caption made it into the picture.
    pub fn burned(&self) -> bool {
        self.burned
    }

    /// Blends the caption showing at `t_us` (clip time) into a `width`×`height` frame: of captions
    /// that overlap, the one that started last. If drawing fails, the rest of the clip goes on
    /// without captions (logged once) and none count as burned, so the `.srt` file is kept.
    pub fn burn(&mut self, planes: &mut Planes, width: usize, height: usize, t_us: i64, hdr: bool) {
        let showing = self.cues.iter().enumerate().filter(|(_, c)| (c.start_us..c.end_us).contains(&t_us));
        let Some((index, _)) = showing.max_by_key(|(_, c)| c.start_us) else { return };
        if self.failed {
            return;
        }
        if self.shown.as_ref().is_none_or(|(i, _)| *i != index) {
            let drawn = match &self.painter {
                Some(p) => p.draw(&self.cues[index].text),
                None => Painter::new(width, height).and_then(|p| self.painter.insert(p).draw(&self.cues[index].text)),
            };
            match drawn {
                Ok(o) => self.shown = Some((index, o)),
                Err(e) => {
                    tracing::warn!("vertical clip: captions can't be drawn, the rest goes without: {e}");
                    (self.failed, self.burned) = (true, false);
                    return;
                }
            }
        }
        if let Some((_, overlay)) = &self.shown {
            overlay.blend(planes, if hdr { &HDR } else { &SDR });
            self.burned = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_files_are_read_and_bad_blocks_skipped() {
        let srt = "1\r\n00:00:01,500 --> 00:00:03,000\r\nHello there,\r\nhow are you?\r\n\r\n\
                   2\n00:01:02,000 --> 00:01:04,250\nfine\n\n3\nnot a time\nskipped\n\n\
                   4\n00:00:05,000 --> 00:00:04,000\nbackwards\n\n5\n00:00:06,000 --> 00:00:06,000\nno time\n\n\
                   6\n01:00:00,000 --> 01:00:01,000\nan hour in\n";
        assert_eq!(
            parse_srt(srt),
            vec![
                Cue { start_us: 1_500_000, end_us: 3_000_000, text: "Hello there, how are you?".into() },
                Cue { start_us: 62_000_000, end_us: 64_250_000, text: "fine".into() },
                Cue { start_us: 3_600_000_000, end_us: 3_601_000_000, text: "an hour in".into() },
            ]
        );
        assert!(Captions::new(parse_srt("")).is_none());
    }

    #[test]
    fn text_is_white_on_a_black_outline_and_blends_into_nv12_and_p010() {
        let (w, h) = (594, 1056);
        let mut c =
            Captions::new(vec![Cue { start_us: 0, end_us: 1_000_000, text: "Did you see that?".into() }]).unwrap();
        let mut y = vec![128u8; w * h];
        let mut uv = vec![60u8; w * h / 2];
        let mut planes: Planes = [Some((&mut y, w)), Some((&mut uv, w)), None];
        c.burn(&mut planes, w, h, 2_000_000, false);
        assert!(!c.burned(), "nothing shows after the last caption");
        c.burn(&mut planes, w, h, 500_000, false);
        assert!(c.burned());
        let (top, overlay) = (h * 45 / 100, &c.shown.as_ref().unwrap().1);
        assert!(overlay.top >= top && overlay.top + overlay.px.len() / w <= h * 85 / 100, "inside the caption band");
        assert!(y[..top * w].iter().all(|&v| v == 128), "nothing above the band");
        assert!(y.contains(&235) && y.contains(&16), "white text, black outline");
        assert!(uv.contains(&128), "the text's colour is neutral");
        assert!(uv[..(top / 2) * w].iter().all(|&v| v == 60));

        // P010: the same picture at 10 bits, white at the HDR reference level.
        let mut y = vec![0u8; w * h * 2];
        let mut uv = vec![0u8; w * h];
        let mut planes: Planes = [Some((&mut y, w * 2)), Some((&mut uv, w * 2)), None];
        overlay.blend(&mut planes, &HDR);
        let max = y.chunks(2).map(|s| u16::from_le_bytes([s[0], s[1]]) >> 6).max();
        assert_eq!(max, Some(572));
    }

    #[test]
    fn overlapping_captions_show_the_newest() {
        let cue = |start_us, end_us, text: &str| Cue { start_us, end_us, text: text.into() };
        let mut c = Captions::new(vec![cue(0, 3_000_000, "first"), cue(1_000_000, 2_000_000, "second")]).unwrap();
        let (w, h) = (64, 128);
        let (mut y, mut uv) = (vec![16u8; w * h], vec![128u8; w * h / 2]);
        for (t, shown) in [(500_000, 0), (1_500_000, 1), (2_500_000, 0)] {
            c.burn(&mut [Some((&mut y, w)), Some((&mut uv, w)), None], w, h, t, false);
            assert_eq!(c.shown.as_ref().map(|(i, _)| *i), Some(shown), "at {t}");
        }
    }

    #[test]
    fn blends_into_separate_u_and_v_planes() {
        // A 4×4 caption of full alpha at rows 2..4 of a 4×6 yuv420p frame.
        let (w, h) = (4, 6);
        let overlay = Overlay::new(2, w, vec![[255, 255]; w * 2]);
        let mut y = vec![100u8; w * h];
        let (mut u, mut v) = (vec![60u8; w / 2 * h / 2], vec![200u8; w / 2 * h / 2]);
        overlay.blend(&mut [Some((&mut y, w)), Some((&mut u, w / 2)), Some((&mut v, w / 2))], &SDR);
        assert_eq!(y, [[100; 8], [235; 8], [100; 8]].concat(), "white in its rows only");
        assert_eq!(u, [60, 60, 128, 128, 60, 60], "the chroma row under it is neutral");
        assert_eq!(v, [200, 200, 128, 128, 200, 200]);
    }

    #[test]
    fn ten_bit_samples_sit_in_the_top_bits() {
        let mut row = vec![0u8; 8];
        for (i, v) in [10, 572, 64, 1000].into_iter().enumerate() {
            HDR.set(&mut row, i, v);
        }
        assert_eq!((0..4).map(|i| HDR.get(&row, i)).collect::<Vec<_>>(), [10, 572, 64, 1000]);
        assert_eq!(u16::from_le_bytes([row[2], row[3]]), 572 << 6);
        assert_eq!(SDR.get(&[7, 9], 1), 9);
    }

    #[test]
    fn half_transparent_captions_mix_with_the_picture() {
        // A 4×4 caption of half alpha (dark grey, premultiplied) at rows 2..6 of a 4×6 frame.
        let (w, h) = (4, 6);
        let overlay = Overlay::new(2, w, vec![[64, 128]; w * 4]);
        let mut y = vec![100u8; w * h];
        let (mut u, mut v) = (vec![60u8; w / 2 * h / 2], vec![200u8; w / 2 * h / 2]);
        overlay.blend(&mut [Some((&mut y, w)), Some((&mut u, w / 2)), Some((&mut v, w / 2))], &SDR);
        // (100 × 127 + 16 × 128 + 64 × 219) / 255, and (60 × 127 + 128 × 128) / 255.
        assert_eq!(y, [&[100; 8][..], &[112; 16]].concat());
        assert_eq!(u, [60, 60, 94, 94, 94, 94], "both chroma rows under it");
        assert_eq!(v, [200, 200, 163, 163, 163, 163]);

        // NV12 whose last chroma row is cut short: only the samples there are touched.
        let mut y = vec![100u8; w * h];
        let mut uv = vec![60u8; w * 2 + 3];
        overlay.blend(&mut [Some((&mut y, w)), Some((&mut uv, w)), None], &SDR);
        assert_eq!(uv, [&[60; 4][..], &[94; 4], &[94; 3]].concat());
        // The same at 10 bits.
        let mut y = vec![0u8; w * h * 2];
        let mut uv = vec![0u8; w * 2 * 2 + 6];
        overlay.blend(&mut [Some((&mut y, w * 2)), Some((&mut uv, w * 2)), None], &HDR);
        let samples: Vec<_> = uv.chunks(2).map(|s| u16::from_le_bytes([s[0], s[1]]) >> 6).collect();
        assert_eq!(samples, [&[0; 4][..], &[257; 4], &[257; 3]].concat());
    }

    #[test]
    fn the_outline_rings_the_text_and_the_text_is_centred() {
        let (w, h) = (594, 1056);
        let painter = Painter::new(w, h).unwrap();
        let o = painter.draw("Hey, gg").unwrap();
        let rows = o.px.len() / w;
        // Bounding box (left, right, top, bottom) of the pixels `keep` picks.
        let bbox = |keep: &dyn Fn([u8; 2]) -> bool| {
            let hits: Vec<_> = (0..rows * w).filter(|&i| keep(o.px[i])).map(|i| (i % w, i / w)).collect();
            let (xs, ys) = (hits.iter().map(|p| p.0), hits.iter().map(|p| p.1));
            (xs.clone().min().unwrap(), xs.max().unwrap(), ys.clone().min().unwrap(), ys.max().unwrap())
        };
        let ink = bbox(&|p| p[1] > 0);
        let white = bbox(&|p| p[0] > 200);
        assert_eq!((ink.2, ink.3), (0, rows - 1), "only rows with text are kept");
        // Clipped at the band's bottom, the outline there would be thinner.
        let margins = [white.0 - ink.0, ink.1 - white.1, white.2 - ink.2, ink.3 - white.3];
        assert!(margins.iter().all(|m| (2..=5).contains(m)), "about size / 10 on every side: {margins:?}");
        assert!(ink.0.abs_diff(w - 1 - ink.1) <= 2, "centred: {ink:?}");
        let bottom = o.top + rows - painter.top;
        assert!(bottom > painter.height * 3 / 4, "at the band's bottom: row {bottom} of {}", painter.height);
    }
}
