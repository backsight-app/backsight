//! Real WGC window capture of a window this test makes (no focus taken, nothing else touched):
//! sizes follow resizes, a minimized window sends nothing, and closing it ends capture.
use backsight_engine::capture::{d3d::Gpu, wgc};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::HWND,
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

/// Pumps this thread's messages for `time`, changing the window's text so it keeps repainting
/// (WGC sends frames only when the content changes, and drops the first one at a new size).
fn pump(window: HWND, time: Duration) {
    let end = Instant::now() + time;
    let mut msg = MSG::default();
    let mut n = 0u32;
    while Instant::now() < end {
        n += 1;
        let text = windows::core::HSTRING::from(format!("Backsight window capture test {n}"));
        // SAFETY: `msg` is a valid local; messages are for windows on this thread; a destroyed
        // window only makes SetWindowTextW fail.
        unsafe {
            let _ = SetWindowTextW(window, &text);
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Frame sizes (or "closed") seen in `time`.
fn seen(window: HWND, rx: &mpsc::Receiver<Option<(u32, u32)>>, time: Duration) -> Vec<Option<(u32, u32)>> {
    pump(window, time);
    rx.try_iter().collect()
}

#[test]
#[ignore = "needs an unlocked desktop and a GPU"]
fn window_capture_follows_resize_minimize_and_close() {
    // SAFETY: plain window calls on windows this test owns; strings are static.
    let window = unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let window = CreateWindowExW(
            WS_EX_NOACTIVATE,
            w!("STATIC"),
            w!("Backsight window capture test"),
            WS_OVERLAPPEDWINDOW,
            40,
            40,
            640,
            480,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        window
    };
    pump(window, Duration::from_millis(300));
    let gpu = Gpu::open("cpu").unwrap();
    let (tx, rx) = mpsc::channel();
    let capture = wgc::Capture::window(&gpu, window, false, None, move |frame| {
        let _ = tx.send(frame.ok().map(|f| f.size));
    })
    .unwrap();

    let first = seen(window, &rx, Duration::from_millis(800));
    println!("first frames: {first:?}");
    let (w, h) = first.iter().flatten().next().copied().expect("frames from a shown window");

    // SAFETY: our own window; no activation.
    unsafe { SetWindowPos(window, None, 0, 0, 900, 500, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE).unwrap() };
    let resized = seen(window, &rx, Duration::from_millis(1000));
    println!("after resize: {resized:?}");
    let last = resized.iter().flatten().last().copied().expect("frames after a resize");
    assert!(last.0 > w && last.1 > h, "the frame follows the window: {last:?} after {:?}", (w, h));

    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(window, SW_MINIMIZE);
    }
    pump(window, Duration::from_millis(500)); // the minimize animation may still send frames
    let minimized = seen(window, &rx, Duration::from_millis(1500));
    println!("while minimized: {minimized:?}");

    // SAFETY: our own window, destroyed on the thread that made it.
    unsafe { DestroyWindow(window).unwrap() };
    let closed = seen(window, &rx, Duration::from_millis(1500));
    println!("after close: {closed:?}");
    drop(capture);
    assert!(minimized.iter().all(Option::is_some), "minimizing is not an error");
    assert!(minimized.len() <= 2, "a minimized window sends (almost) no frames: {minimized:?}");
    assert!(closed.contains(&None), "closing the window ends capture with an error");
}
