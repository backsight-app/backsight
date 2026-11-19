//! One-shot screenshots of the monitor the game is on, of the focused window, or of an area picked
//! on a frozen frame, saved first and optionally copied as an image.
mod hdr_capture;

use crate::{
    capture::{d3d::Gpu, foreground, wgc},
    game,
    session::user_error,
    system,
};
use backsight_config::Settings;
use backsight_encode::{Catalog, catalog::ScreenshotFormat};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

const NO_GPU: &str =
    "Backsight couldn't use your graphics card for the screenshot. Update your graphics driver, then try again.";
const NO_CAPTURE: &str =
    "Backsight couldn't capture your screen. Unlock your desktop and try again; if it keeps failing, restart your PC.";
const NO_WINDOW: &str = "Backsight couldn't capture that window. It may have closed or be minimized. Try again, or use the whole-screen screenshot.";
const NO_ENCODE: &str =
    "Backsight couldn't turn the screenshot into an image file. Try another format on Backsight's Screenshots page.";

pub struct Saved {
    pub path: PathBuf,
    /// A clipboard failure must never hide or remove the successfully saved file.
    pub clipboard_error: Option<String>,
}

/// The monitor under the mouse, captured once so [`take_region`] crops the frame the picker shows.
pub struct Frozen {
    /// Top-down opaque BGRA for SDR or linear scRGB RGBA half-floats for HDR.
    pixels: Vec<u8>,
    hdr: bool,
    pub width: u32,
    pub height: u32,
    /// The monitor's top-left on the virtual desktop in physical pixels, where the picker goes.
    pub origin: (i32, i32),
    /// The game in front when the frame was taken (the picker takes focus after).
    game: Option<String>,
}

impl Frozen {
    /// The frame as an SDR BMP for the picker window.
    pub fn bmp(&self) -> Vec<u8> {
        let preview;
        let pixels = if self.hdr {
            preview = sdr_preview(&self.pixels);
            &preview
        } else {
            &self.pixels
        };
        let len = 14 + 40 + pixels.len();
        let mut out = Vec::with_capacity(len);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(len as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes()); // pixels start after both headers
        // Appended in place: a second copy of a 4K frame would be another 33 MB.
        dib_into(out, pixels, self.width, self.height)
    }
}

/// The whole monitor the focused window is on.
pub fn take(settings: &Settings) -> Result<Saved, String> {
    shoot(settings, Target::Monitor(wgc::foreground_monitor()))
}

/// The whole monitor under the mouse: Home's Screenshot tile, pressed with Backsight's window hidden.
pub fn take_under_mouse(settings: &Settings) -> Result<Saved, String> {
    shoot(settings, Target::Monitor(wgc::cursor_monitor()))
}

/// Only the focused window, title bar included. Falls back to the whole monitor when no ordinary
/// window has focus (the desktop, the taskbar, a minimized window or Backsight itself).
pub fn take_window(settings: &Settings) -> Result<Saved, String> {
    shoot(settings, wgc::foreground_window().map_or_else(|| Target::Monitor(wgc::foreground_monitor()), Target::Window))
}

/// Captures the monitor under the mouse for the region picker. Checks the format and folder
/// first, so a settings problem shows before the user drags.
pub fn freeze(settings: &Settings) -> Result<Frozen, String> {
    let (format, _) = destination(settings)?;
    let monitor = wgc::cursor_monitor();
    let origin = wgc::monitor_origin(monitor).map_err(|e| user_error(NO_CAPTURE, &e))?;
    let game = game_in_front();
    let (pixels, width, height) = capture(settings, Target::Monitor(monitor), format.hdr)?;
    Ok(Frozen { pixels, hdr: format.hdr, width, height, origin, game })
}

/// Saves (and copies) the part of `frozen` the user dragged over in the picker: two corners in
/// either order in the picker's CSS pixels, and the picker's size in the same units. `Ok(None)`
/// when the drag has no area, which cancels.
pub fn take_region(
    settings: &Settings,
    frozen: &Frozen,
    start: [f64; 2],
    end: [f64; 2],
    view: [f64; 2],
) -> Result<Option<Saved>, String> {
    let Some(area) = area(start, end, view, (frozen.width, frozen.height)) else { return Ok(None) };
    let destination = destination(settings)?;
    if destination.0.hdr != frozen.hdr {
        return Err("The screenshot format changed. Start a new region screenshot.".into());
    }
    output(settings, destination, frozen.game.as_deref(), &crop(frozen, &area), area.width, area.height).map(Some)
}

fn crop(frozen: &Frozen, area: &Area) -> Vec<u8> {
    let pixel_bytes = if frozen.hdr { 8 } else { 4 };
    let mut pixels = Vec::with_capacity(area.width as usize * area.height as usize * pixel_bytes);
    for line in
        frozen.pixels.chunks_exact(frozen.width as usize * pixel_bytes).skip(area.y as usize).take(area.height as usize)
    {
        pixels.extend_from_slice(&line[area.x as usize * pixel_bytes..(area.x + area.width) as usize * pixel_bytes]);
    }
    pixels
}

/// A rectangle of the frozen image, in its pixels.
#[derive(Debug, PartialEq, Eq)]
struct Area {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Maps a picker drag to image pixels, clamped to the image; `None` for no area. The picker
/// stretches the whole image over its whole view, so the scale is measured (image ÷ view) rather
/// than taken from Windows: it stays right whatever the monitor's DPI, the text size setting, or a
/// picker a pixel off its monitor. Picker coordinates start at its own top-left, so a monitor at
/// negative desktop coordinates (left of or above the primary) needs nothing extra.
fn area(start: [f64; 2], end: [f64; 2], view: [f64; 2], image: (u32, u32)) -> Option<Area> {
    // Values come from the WebView: NaN, infinities and an empty view give no area.
    let axis = |a: f64, b: f64, view: f64, size: u32| {
        if !(view > 0.0 && a.is_finite() && b.is_finite()) {
            return None;
        }
        let px = |v: f64| (v / view * f64::from(size)).round().clamp(0.0, f64::from(size)) as u32;
        let (lo, hi) = (px(a.min(b)), px(a.max(b)));
        (hi > lo).then_some((lo, hi - lo))
    };
    let (x, width) = axis(start[0], end[0], view[0], image.0)?;
    let (y, height) = axis(start[1], end[1], view[1], image.1)?;
    Some(Area { x, y, width, height })
}

enum Target {
    Monitor(windows::Win32::Graphics::Gdi::HMONITOR),
    Window(windows::Win32::Foundation::HWND),
}

fn shoot(settings: &Settings, target: Target) -> Result<Saved, String> {
    let destination = destination(settings)?;
    let game = game_in_front();
    let (pixels, width, height) = capture(settings, target, destination.0.hdr)?;
    output(settings, destination, game.as_deref(), &pixels, width, height)
}

/// A screenshot is named after the program in front at that moment, unless it's a game launcher.
fn game_in_front() -> Option<String> {
    foreground::program_now().filter(|p| !game::is_launcher(&p.exe)).as_ref().and_then(game::program_name)
}

/// The chosen format and the screenshot folder.
fn destination(settings: &Settings) -> Result<(&'static ScreenshotFormat, PathBuf), String> {
    let format = Catalog::get()
        .screenshot_formats
        .iter()
        .find(|f| f.id == settings.screenshot.format)
        .ok_or("Choose a screenshot format on Backsight's Screenshots page.")?;
    let folder = settings.general.screenshot_dir().ok_or("Choose a screenshot folder on Backsight's General page.")?;
    Ok((format, folder))
}

/// One frame of `target` as opaque BGRA or scRGB.
fn capture(settings: &Settings, target: Target, hdr: bool) -> Result<(Vec<u8>, u32, u32), String> {
    // Like ReplaySession, let windows-core keep the MTA alive for its cached WinRT factories.
    // shortcut: a fresh device on the high-performance GPU per shot (the dGPU on a hybrid laptop); reuse the replay device if shots feel slow.
    let gpu = Arc::new(Gpu::open("cpu").map_err(|e| user_error(NO_GPU, &e))?);
    let no_capture = if matches!(target, Target::Window(_)) { NO_WINDOW } else { NO_CAPTURE };
    let cursor = settings.screenshot.capture_cursor;
    if hdr {
        return hdr_capture::capture(gpu, target, cursor).map_err(|e| user_error(no_capture, &e));
    }
    tracing::debug!("screenshot: open capture");
    let (send, receive) = mpsc::sync_channel(1);
    let device = gpu.clone();
    let mut first = true;
    let on_frame = move |frame: windows::core::Result<wgc::Frame>| {
        if !first {
            return;
        }
        first = false;
        let pixels = frame.and_then(|f| wgc::read_pixels(&device, f));
        let _ = send.try_send(pixels);
    };
    let capture = match target {
        Target::Window(window) => wgc::Capture::window(&gpu, window, cursor, None, on_frame),
        Target::Monitor(monitor) => wgc::Capture::monitor(&gpu, monitor, cursor, None, on_frame),
    }
    .map_err(|e| user_error(no_capture, &e))?;
    let result = receive.recv_timeout(Duration::from_secs(5));
    drop(capture);
    tracing::debug!("screenshot: capture stopped");
    result.map_err(|_| user_error(no_capture, &"no frame within 5 s"))?.map_err(|e| user_error(no_capture, &e))
}

/// Encodes and saves the image, then copies it when the setting is on.
fn output(
    settings: &Settings,
    (format, root): (&ScreenshotFormat, PathBuf),
    game: Option<&str>,
    pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<Saved, String> {
    let bytes = if format.hdr {
        backsight_encode::screenshot::encode_jxr(pixels, width, height).map_err(|e| user_error(NO_ENCODE, &e))?
    } else {
        backsight_encode::screenshot::encode(pixels, width, height, format, settings.screenshot.quality)
            .map_err(|e| user_error(NO_ENCODE, &e))?
    };
    let (folder, name) = game::place(&root, game, &settings.general);
    let path = save(&folder, &name, &format.ext, &bytes)?;
    tracing::debug!("screenshot: file saved");
    let clipboard_error = if settings.screenshot.copy_to_clipboard {
        let preview;
        let pixels = if format.hdr {
            preview = sdr_preview(pixels);
            &preview
        } else {
            pixels
        };
        system::copy_dib(&dib(pixels, width, height)).err().map(|e| {
            tracing::warn!("Screenshot saved but clipboard failed: {e}");
            "Saved to your screenshot folder, but couldn't copy the image. Close any clipboard dialog and try again."
                .into()
        })
    } else {
        None
    };
    Ok(Saved { path, clipboard_error })
}

fn save(folder: &Path, name: &str, ext: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let cant_save = format!(
        "Backsight couldn't save the screenshot in {}. Check that the drive has free space, or pick another folder on Backsight's General page.",
        folder.display()
    );
    fs::create_dir_all(folder).map_err(|e| user_error(&cant_save, &e))?;
    let stamp = system::local_time_stamp();
    for n in 0.. {
        let suffix = if n == 0 { String::new() } else { format!(" ({n})") };
        let path = folder.join(format!("{name} {stamp}{suffix}.{ext}"));
        let temp = path.with_extension(format!("{ext}.part"));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(user_error(&cant_save, &e)),
        };
        let write = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        let result = write.and_then(|()| backsight_encode::fsutil::rename_no_replace(&temp, &path));
        let _ = fs::remove_file(&temp);
        match result {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(user_error(&cant_save, &e)),
        }
    }
    unreachable!()
}

// shortcut: fixed highlight roll-off for the SDR picker and clipboard. Use display-aware tone
// mapping if these previews need to match the brightness of a particular monitor.
pub(crate) fn sdr_preview(rgba: &[u8]) -> Vec<u8> {
    let channel = |bytes: [u8; 2]| {
        let linear = half::f16::from_le_bytes(bytes).to_f32();
        if linear.is_finite() { linear.max(0.0) } else { 0.0 }
    };
    let srgb = |linear: f32| {
        let gamma = if linear <= 0.0031308 { linear * 12.92 } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
        (gamma * 255.0).round() as u8
    };
    rgba.as_chunks::<8>()
        .0
        .iter()
        .flat_map(|&[r0, r1, g0, g1, b0, b1, _, _]| {
            let (r, g, b) = (channel([r0, r1]), channel([g0, g1]), channel([b0, b1]));
            let peak = r.max(g).max(b);
            // Match the slope at the knee and approach white without clipping bright colors.
            let scale = if peak > 0.75 { (1.0 - 0.0625 / (peak - 0.5)) / peak } else { 1.0 };
            [srgb(b * scale), srgb(g * scale), srgb(r * scale), 255]
        })
        .collect()
}

/// CF_DIB: 40-byte header, then bottom-up opaque 32-bit BGR pixels, without a BMP file header.
fn dib(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    dib_into(Vec::with_capacity(40 + pixels.len()), pixels, width, height)
}

/// Appends the DIB (header and bottom-up rows) to `out`.
fn dib_into(mut out: Vec<u8>, pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let at = out.len();
    out.resize(at + 40, 0);
    let head = &mut out[at..];
    head[0..4].copy_from_slice(&40u32.to_le_bytes());
    head[4..8].copy_from_slice(&width.to_le_bytes());
    head[8..12].copy_from_slice(&height.to_le_bytes());
    head[12..14].copy_from_slice(&1u16.to_le_bytes());
    head[14..16].copy_from_slice(&32u16.to_le_bytes());
    for row in pixels.chunks_exact(width as usize * 4).rev() {
        out.extend_from_slice(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dib_preserves_colours_and_reverses_rows() {
        let pixels = [1, 2, 3, 255, 4, 5, 6, 255];
        let data = dib(&pixels, 1, 2);
        assert_eq!(&data[40..], &[4, 5, 6, 255, 1, 2, 3, 255]);
        assert_eq!(&data[4..12], &[1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(&data[12..16], &[1, 0, 32, 0]);
    }
    const SCREEN: (u32, u32) = (2560, 1440);

    fn picked(start: [f64; 2], end: [f64; 2], view: [f64; 2]) -> Option<(u32, u32, u32, u32)> {
        area(start, end, view, SCREEN).map(|a| (a.x, a.y, a.width, a.height))
    }

    #[test]
    fn a_drag_in_any_direction_gives_the_same_area() {
        let view = [2560.0, 1440.0];
        let want = Some((100, 50, 300, 200));
        assert_eq!(picked([100.0, 50.0], [400.0, 250.0], view), want);
        assert_eq!(picked([400.0, 250.0], [100.0, 50.0], view), want);
        assert_eq!(picked([400.0, 50.0], [100.0, 250.0], view), want);
        assert_eq!(picked([100.0, 250.0], [400.0, 50.0], view), want);
    }

    #[test]
    fn a_drag_without_area_cancels() {
        let view = [2560.0, 1440.0];
        assert_eq!(picked([10.0, 10.0], [10.0, 10.0], view), None, "a click");
        assert_eq!(picked([10.0, 10.0], [500.0, 10.0], view), None, "a flat line");
        assert_eq!(picked([10.0, 10.0], [10.2, 10.2], view), None, "under half a pixel");
        assert_eq!(picked([-50.0, 10.0], [-1.0, 90.0], view), None, "wholly off the image");
    }

    #[test]
    fn css_pixels_map_to_image_pixels_at_any_scale() {
        // 150 %: the 2560x1440 monitor is 1706.67x960 CSS px.
        let view = [2560.0 / 1.5, 960.0];
        assert_eq!(picked([100.0, 100.0], [200.0, 300.0], view), Some((150, 150, 150, 300)));
        // 125 % on 1920x1080, scaled up to this image's size: fractions are what count.
        assert_eq!(picked([0.0, 0.0], [768.0, 432.0], [1536.0, 864.0]), Some((0, 0, 1280, 720)));
    }

    #[test]
    fn a_drag_past_the_edges_is_clamped_to_the_image() {
        // The pointer can leave the picker on a monitor at negative desktop coordinates too; the
        // picker reports it relative to its own top-left, so only the image bounds matter.
        let view = [2560.0, 1440.0];
        assert_eq!(picked([-300.0, -20.0], [9000.0, 700.0], view), Some((0, 0, 2560, 700)));
        assert_eq!(picked([2500.0, 1400.0], [2600.0, 1500.0], view), Some((2500, 1400, 60, 40)));
    }

    #[test]
    fn values_the_webview_should_never_send_give_no_area() {
        assert_eq!(picked([0.0, 0.0], [10.0, 10.0], [0.0, 1440.0]), None);
        assert_eq!(picked([0.0, 0.0], [10.0, 10.0], [-2560.0, 1440.0]), None);
        assert_eq!(picked([f64::NAN, 0.0], [10.0, 10.0], [2560.0, 1440.0]), None);
        assert_eq!(picked([0.0, 0.0], [f64::INFINITY, 10.0], [2560.0, 1440.0]), None);
        assert_eq!(picked([0.0, 0.0], [10.0, 10.0], [f64::NAN, 1440.0]), None);
    }

    #[test]
    fn crop_takes_exactly_the_picked_pixels() {
        // 3x2 image whose pixel bytes are (x, y, 0, 255).
        let pixels = (0..2u8).flat_map(|y| (0..3u8).flat_map(move |x| [x, y, 0, 255])).collect();
        let frozen = Frozen { pixels, hdr: false, width: 3, height: 2, origin: (-1920, 0), game: None };
        let a = Area { x: 1, y: 1, width: 2, height: 1 };
        assert_eq!(crop(&frozen, &a), [1, 1, 0, 255, 2, 1, 0, 255]);
        let bmp = frozen.bmp();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize, bmp.len());
        assert_eq!(u32::from_le_bytes(bmp[10..14].try_into().unwrap()), 54);
        assert_eq!(&bmp[14..], dib(&frozen.pixels, 3, 2));
    }

    #[test]
    fn hdr_region_saves_the_selected_half_floats_and_previews_in_sdr() {
        let pixels: Vec<u8> =
            [0u16, 0, 0, 0x3c00, 0x4000, 0, 0x3800, 0x3c00].into_iter().flat_map(u16::to_le_bytes).collect();
        let frozen = Frozen { pixels, hdr: true, width: 2, height: 1, origin: (0, 0), game: None };
        let preview = frozen.bmp();
        assert_eq!(&preview[54..58], &[0, 0, 0, 255]);
        assert!(preview[58] > 0 && preview[58] < preview[60] && preview[60] < 255);
        assert_eq!(preview[59], 0);
        assert_eq!(preview[61], 255);

        let folder = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.general.screenshot_folder = folder.path().to_string_lossy().into_owned();
        settings.screenshot.copy_to_clipboard = false;
        let pick = |settings: &Settings| take_region(settings, &frozen, [1.0, 0.0], [2.0, 1.0], [2.0, 1.0]);
        assert!(pick(&settings).is_err(), "an SDR format cannot use an HDR frozen frame");
        settings.screenshot.format = "jxr".into();
        let saved = pick(&settings).unwrap().unwrap();
        assert_eq!(saved.path.extension().unwrap(), "jxr");
        let expected = backsight_encode::screenshot::encode_jxr(&frozen.pixels[8..], 1, 1).unwrap();
        assert_eq!(fs::read(saved.path).unwrap(), expected);
    }

    #[test]
    fn saving_twice_never_replaces_the_first_image() {
        let folder = std::env::temp_dir().join(format!("backsight-screenshot-{}", std::process::id()));
        let first = save(&folder, "Tomb Raider", "png", b"first").unwrap();
        let second = save(&folder, "Tomb Raider", "png", b"second").unwrap();
        assert!(first.file_name().unwrap().to_str().unwrap().starts_with("Tomb Raider 20"));
        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");
        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
        fs::remove_dir(folder).unwrap();
    }
}
