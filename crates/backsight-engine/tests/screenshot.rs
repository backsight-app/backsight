//! Real WGC and clipboard check; deliberately replaces the clipboard with a desktop image.
use backsight_config::Settings;
use backsight_engine::screenshot;
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};

#[test]
#[ignore = "needs an unlocked desktop with an ordinary focused window"]
fn window_shot_matches_the_focused_window() {
    use windows::Win32::{
        Foundation::RECT,
        Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            WindowsAndMessaging::{GetClientRect, GetForegroundWindow, GetWindowRect},
        },
    };
    let (mut frame, mut outer, mut client) = (RECT::default(), RECT::default(), RECT::default());
    // SAFETY: out-pointers are valid locals sized for a RECT; the window is only queried.
    let window = unsafe {
        // Like the app (Tauri is per-monitor v2), so window rects are in physical pixels.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let window = GetForegroundWindow();
        let size = std::mem::size_of::<RECT>() as u32;
        DwmGetWindowAttribute(window, DWMWA_EXTENDED_FRAME_BOUNDS, (&raw mut frame).cast(), size).unwrap();
        GetWindowRect(window, &mut outer).unwrap();
        GetClientRect(window, &mut client).unwrap();
        window
    };
    let size = |r: RECT| ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
    let mut settings = Settings::default();
    let folder = std::env::temp_dir().join(format!("backsight-window-shot-{}", std::process::id()));
    settings.general.screenshot_folder = folder.to_string_lossy().into_owned();
    settings.screenshot.copy_to_clipboard = false;
    let saved = screenshot::take_window(&settings).unwrap();
    let info = backsight_encode::media::probe(&saved.path).unwrap();
    let image = (info.width.unwrap(), info.height.unwrap());
    println!(
        "window {window:?}: image {image:?}, frame bounds {:?}, window rect {:?}, client {:?}",
        size(frame),
        size(outer),
        size(client)
    );
    std::fs::remove_file(&saved.path).unwrap();
    std::fs::remove_dir(folder).unwrap();
    assert_eq!(image, size(frame), "the image is the window's visible frame, without shadow or other windows");
}

#[test]
#[ignore = "needs an unlocked desktop"]
fn region_shot_crops_the_frozen_monitor() {
    let mut settings = Settings::default();
    let folder = std::env::temp_dir().join(format!("backsight-region-shot-{}", std::process::id()));
    settings.general.screenshot_folder = folder.to_string_lossy().into_owned();
    settings.screenshot.copy_to_clipboard = false;
    let frozen = screenshot::freeze(&settings).unwrap();
    println!("monitor under the mouse: {}x{} at {:?}", frozen.width, frozen.height, frozen.origin);
    // The middle half of a view the size of the image (a 100 % monitor).
    let view = [f64::from(frozen.width), f64::from(frozen.height)];
    let saved = screenshot::take_region(
        &settings,
        &frozen,
        [view[0] * 0.75, view[1] * 0.75],
        [view[0] / 4.0, view[1] / 4.0],
        view,
    )
    .unwrap()
    .expect("an area");
    let info = backsight_encode::media::probe(&saved.path).unwrap();
    std::fs::remove_file(&saved.path).unwrap();
    std::fs::remove_dir(folder).unwrap();
    assert_eq!((info.width.unwrap(), info.height.unwrap()), (frozen.width.div_ceil(2), frozen.height.div_ceil(2)));
    assert!(screenshot::take_region(&settings, &frozen, [5.0, 5.0], [5.0, 5.0], view).unwrap().is_none());
}

#[test]
#[ignore = "needs an unlocked desktop; replaces clipboard contents"]
fn saves_and_copies_an_image_with_replay_off() {
    let _ = tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).with_test_writer().try_init();
    let mut settings = Settings::default();
    let folder = std::env::temp_dir().join(format!("backsight-screenshot-live-{}", std::process::id()));
    settings.general.screenshot_folder = folder.to_string_lossy().into_owned();
    settings.screenshot.copy_to_clipboard = true;
    let saved = screenshot::take(&settings).unwrap();
    assert!(saved.clipboard_error.is_none(), "{:?}", saved.clipboard_error);
    let info = backsight_encode::media::probe(&saved.path).unwrap();
    assert!(info.width.unwrap() > 0 && info.height.unwrap() > 0);
    assert_eq!(info.video_codec.as_deref(), Some("png"));
    // SAFETY: read-only clipboard access; CloseClipboard balances a successful open.
    unsafe {
        OpenClipboard(None).unwrap();
        let image = GetClipboardData(8);
        CloseClipboard().unwrap();
        assert!(image.is_ok(), "clipboard must contain image pixels, not a file path");
    }
    println!(
        "Saved {} ({}x{}), clipboard contains CF_DIB",
        saved.path.display(),
        info.width.unwrap(),
        info.height.unwrap()
    );
    std::fs::remove_file(saved.path).unwrap();

    let (ready, wait_ready) = std::sync::mpsc::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        // SAFETY: hold the clipboard without changing it to simulate another app using it.
        unsafe {
            OpenClipboard(None).unwrap();
        }
        ready.send(()).unwrap();
        wait_release.recv().unwrap();
        // SAFETY: balances the successful OpenClipboard on this thread.
        unsafe {
            CloseClipboard().unwrap();
        }
    });
    wait_ready.recv().unwrap();
    println!("Testing saved file while another thread holds the clipboard");
    let failed_copy = screenshot::take(&settings).unwrap();
    println!("Busy-clipboard capture returned");
    assert!(failed_copy.clipboard_error.is_some());
    assert!(failed_copy.path.is_file(), "clipboard failure must preserve the file");
    settings.screenshot.copy_to_clipboard = false;
    println!("Testing capture with copying disabled");
    let file_only = screenshot::take(&settings).unwrap();
    assert!(file_only.clipboard_error.is_none(), "with copying off, a busy clipboard is no error");
    release.send(()).unwrap();
    holder.join().unwrap();
    std::fs::remove_file(failed_copy.path).unwrap();
    std::fs::remove_file(file_only.path).unwrap();
    std::fs::remove_dir(folder).unwrap();
}
