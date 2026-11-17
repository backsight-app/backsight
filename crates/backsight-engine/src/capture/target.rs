//! What video capture records: which monitor, or the game's window. Pure choice and fallback
//! logic; the Windows calls are in `wgc.rs`.

/// The monitor chosen in settings is not connected: the main one is recorded instead.
pub const MONITOR_GONE: &str =
    "The monitor picked on Backsight's Video page isn't connected, so Backsight records your main monitor.";
/// HDR video is on, but Windows shows the recorded monitor in standard dynamic range.
pub const SDR_MONITOR: &str = "This monitor isn't showing HDR, so Backsight records standard video. To record HDR, turn on HDR for it in Windows display settings, then turn Instant Replay off and on.";
/// HDR video is on, but Windows didn't say whether the monitor shows HDR.
pub const HDR_UNKNOWN: &str = "Backsight couldn't check whether this monitor shows HDR, so it records standard video. Turn Instant Replay off and on to check again.";
/// [`MONITOR_GONE`] and [`SDR_MONITOR`] together.
pub const MONITOR_GONE_SDR: &str = "The monitor picked on Backsight's Video page isn't connected, so Backsight records your main monitor. That one isn't showing HDR, so the video is standard.";
/// [`MONITOR_GONE`] and [`HDR_UNKNOWN`] together.
pub const MONITOR_GONE_HDR_UNKNOWN: &str = "The monitor picked on Backsight's Video page isn't connected, so Backsight records your main monitor in standard video: it couldn't check whether that one shows HDR.";

/// HDR video is on, but the encoder or file format can't hold it.
pub const HDR_SETTINGS: &str = "These video settings can't record HDR, so Backsight records standard video. Turn HDR video off and on again on Backsight's Video page to fix them.";
/// HDR video is on and the settings fit, but the encoder didn't start in HDR.
pub const HDR_ENCODER: &str = "HDR video couldn't start, so Backsight records standard video. Update your graphics driver, then turn Instant Replay off and on.";
/// HDR video is on in window mode, but the game's monitor shows standard dynamic range.
pub const HDR_WINDOW: &str = "Your game's monitor isn't showing HDR, so Backsight records standard video. To record HDR, turn on HDR for it in Windows display settings, then turn Instant Replay off and on.";
/// Window mode: the game moved to a monitor showing HDR during a recording of standard video.
pub const HDR_AFTER_RECORDING: &str =
    "Your game is on a monitor showing HDR now. Backsight switches to HDR video when this recording ends.";
/// Window mode: the game moved to a monitor not showing HDR during a recording of HDR video.
pub const SDR_AFTER_RECORDING: &str = "Your game is on a monitor that isn't showing HDR now. Backsight switches to standard video when this recording ends.";
/// [`MONITOR_GONE`] and [`HDR_SETTINGS`] together.
pub const MONITOR_GONE_HDR_SETTINGS: &str = "The monitor picked on Backsight's Video page isn't connected, so Backsight records your main monitor. These video settings can't record HDR, so the video is standard.";
/// [`MONITOR_GONE`] and [`HDR_ENCODER`] together.
pub const MONITOR_GONE_HDR_ENCODER: &str = "The monitor picked on Backsight's Video page isn't connected, so Backsight records your main monitor. HDR video couldn't start, so the video is standard.";

/// Why HDR video records standard video instead. HDR never stops capture from starting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HdrFallback {
    /// HDR video is off, or it's on and everything fits.
    NotNeeded,
    /// The monitor shows standard dynamic range.
    SdrMonitor,
    /// Windows didn't say (the raw reason is logged).
    Unknown,
    /// The encoder or file format can't hold HDR (a hand-edited file, or a choice made after HDR was turned on).
    Settings,
    /// The encoder didn't start in HDR (the raw reason is logged).
    Encoder,
}

/// What the saved settings allow, before the monitor or the encoder is asked. `encoder_hdr` is the
/// catalog's flag and `ext` the recording file's extension.
pub fn hdr_settings(hdr: bool, encoder_hdr: bool, ext: &str) -> HdrFallback {
    match hdr {
        false => HdrFallback::NotNeeded,
        true if encoder_hdr && matches!(ext, "mp4" | "mkv") => HdrFallback::NotNeeded,
        true => HdrFallback::Settings,
    }
}

/// Window mode: whether a session started with `fallback` (and HDR video `wanted`) should restart
/// because the game's window sits on a monitor that does (`window_hdr`) or doesn't show HDR,
/// unlike the one checked at start. Only a monitor reason flips; settings or an encoder that
/// can't do HDR never do, so a restart can't repeat for them.
pub fn window_hdr_restart(wanted: bool, fallback: HdrFallback, window_hdr: bool) -> bool {
    match fallback {
        _ if !wanted => false,
        HdrFallback::NotNeeded => !window_hdr,
        HdrFallback::SdrMonitor | HdrFallback::Unknown => window_hdr,
        HdrFallback::Settings | HdrFallback::Encoder => false,
    }
}

/// Window mode's HDR line on Home once the game is captured (see [`window_notice`]).
/// `window_hdr`: what the game's monitor said when checked (`None`: Windows didn't say); it
/// decides over the start's check, which may have been another monitor.
pub fn window_hdr_notice(wanted: bool, fallback: HdrFallback, window_hdr: Option<bool>) -> Option<&'static str> {
    match fallback {
        _ if !wanted => None,
        HdrFallback::SdrMonitor | HdrFallback::Unknown if window_hdr == Some(false) => Some(HDR_WINDOW),
        HdrFallback::SdrMonitor | HdrFallback::Unknown => Some(HDR_UNKNOWN),
        other => start_notice(false, false, other),
    }
}

/// Window mode's line while a recording holds off the restart for the game's monitor
/// (`waiting`); `hdr_now`: the video being recorded is HDR.
pub fn hdr_after_recording(waiting: bool, hdr_now: bool) -> Option<&'static str> {
    waiting.then_some(if hdr_now { SDR_AFTER_RECORDING } else { HDR_AFTER_RECORDING })
}

/// What Home says when capture starts. Window mode's notices come from [`window_notice`].
pub fn start_notice(window_mode: bool, monitor_gone: bool, hdr: HdrFallback) -> Option<&'static str> {
    match (window_mode, monitor_gone, hdr) {
        (true, ..) => Some(WAITING_FOR_WINDOW),
        (false, false, HdrFallback::NotNeeded) => None,
        (false, true, HdrFallback::NotNeeded) => Some(MONITOR_GONE),
        (false, false, HdrFallback::SdrMonitor) => Some(SDR_MONITOR),
        (false, true, HdrFallback::SdrMonitor) => Some(MONITOR_GONE_SDR),
        (false, false, HdrFallback::Unknown) => Some(HDR_UNKNOWN),
        (false, true, HdrFallback::Unknown) => Some(MONITOR_GONE_HDR_UNKNOWN),
        (false, false, HdrFallback::Settings) => Some(HDR_SETTINGS),
        (false, true, HdrFallback::Settings) => Some(MONITOR_GONE_HDR_SETTINGS),
        (false, false, HdrFallback::Encoder) => Some(HDR_ENCODER),
        (false, true, HdrFallback::Encoder) => Some(MONITOR_GONE_HDR_ENCODER),
    }
}
/// Window mode before a game has kept focus.
pub const WAITING_FOR_WINDOW: &str = "No game window found yet, so the video is black. Keep your game in front for a few seconds and Backsight records just that window.";
/// Window mode after the captured window closed.
pub const WINDOW_CLOSED: &str = "The game window closed. Backsight records the next program you keep in front for a few seconds, so open your game.";
/// Window mode while the captured window is minimized.
pub const WINDOW_MINIMIZED: &str =
    "The game window is minimized, so the video shows its last picture until you bring it back.";

/// Which of `monitors` (`(id, primary)` pairs, in any order) to capture for the saved id: that
/// one, or the primary one when the id is empty or the monitor is gone. The flag says it fell
/// back from a saved monitor. `None` only for an empty list.
pub fn pick_monitor(saved: &str, monitors: &[(&str, bool)]) -> Option<(usize, bool)> {
    let primary = monitors.iter().position(|&(_, p)| p).or((!monitors.is_empty()).then_some(0))?;
    if saved.is_empty() {
        return Some((primary, false));
    }
    Some(monitors.iter().position(|&(id, _)| id.eq_ignore_ascii_case(saved)).map_or((primary, true), |i| (i, false)))
}

/// Whether capture of monitor `captured` should restart on the one [`pick_monitor`] now picks:
/// the saved monitor came back after a fallback, the captured one is gone (in case Windows
/// doesn't close its capture), or (none saved) the main monitor changed.
pub fn should_move(captured: &str, saved: &str, monitors: &[(&str, bool)]) -> bool {
    pick_monitor(saved, monitors).is_some_and(|(i, _)| !monitors[i].0.eq_ignore_ascii_case(captured))
}

/// Window mode's choice of window. Once a game's window is captured it stays (a new window of the
/// same program is followed) until it closes; another program only takes over when its window
/// covers its whole monitor, as full-screen and borderless games do, or (`launched_by_captured`)
/// the captured program started it: a windowed game launched from a launcher or chat captured
/// first. So Alt+Tab to a chat, or a browser opened mid-game, never puts that window in the
/// video (ADR 0012). `captured` and `candidate` are `(pid, window)`.
pub fn should_capture(
    captured: Option<(u32, isize)>,
    candidate: (u32, isize),
    covers_monitor: bool,
    launched_by_captured: bool,
) -> bool {
    match captured {
        None => true,
        Some(c) if c == candidate => false,
        Some((pid, _)) if pid == candidate.0 => true,
        Some(_) => covers_monitor || launched_by_captured,
    }
}

/// Whether a window rectangle `(left, top, right, bottom)` covers the monitor's: full screen or
/// borderless. A maximized ordinary window leaves the taskbar uncovered, so it doesn't count.
pub fn covers(window: (i32, i32, i32, i32), monitor: (i32, i32, i32, i32)) -> bool {
    window.0 <= monitor.0 && window.1 <= monitor.1 && window.2 >= monitor.2 && window.3 >= monitor.3
}

/// What Home says in window mode. `hdr` ([`window_hdr_notice`]) is said while nothing else needs saying.
pub fn window_notice(capturing: bool, lost: bool, minimized: bool, hdr: Option<&'static str>) -> Option<&'static str> {
    match (capturing, lost) {
        (false, true) => Some(WINDOW_CLOSED),
        (false, false) => Some(WAITING_FOR_WINDOW),
        (true, _) if minimized => Some(WINDOW_MINIMIZED),
        (true, _) => hdr,
    }
}

/// A new capture size is used only once it has held this long (100 ns ticks): dragging a window's
/// edge changes the size on nearly every frame, and each change rebuilds the GPU converter.
pub const SIZE_SETTLE: u64 = 1_500_000;

/// Debounces capture size changes. `observe` gets each frame's size and time and says whether to
/// convert it at that size (`true`), or skip it while the size is still changing (the last
/// picture is repeated meanwhile).
// shortcut: a still window resized once sends no frame after the settle time, so the video keeps
// its old size and picture until it redraws; apply the pending frame from the video thread if
// paused games that change resolution show this.
#[derive(Debug, Default)]
pub struct SizeSettle {
    pending: Option<((u32, u32), u64)>,
}

impl SizeSettle {
    pub fn observe(&mut self, current: (u32, u32), size: (u32, u32), time: u64) -> bool {
        if size == current {
            self.pending = None;
            return true;
        }
        match self.pending {
            Some((s, since)) if s == size => time.saturating_sub(since) >= SIZE_SETTLE,
            _ => {
                self.pending = Some((size, time));
                false
            }
        }
    }
}

/// Where a `source`-sized picture goes inside an `output`-sized frame: as large as fits with its
/// shape kept, centred, black bars on the other sides. `(left, top, right, bottom)`, on even
/// pixels for NV12.
pub fn fit(source: (u32, u32), output: (u32, u32)) -> (u32, u32, u32, u32) {
    let (sw, sh) = (u64::from(source.0.max(1)), u64::from(source.1.max(1)));
    let (ow, oh) = (u64::from(output.0), u64::from(output.1));
    // Width-limited if the source is wider than the output, compared without dividing.
    let (w, h) = if sw * oh >= sh * ow { (ow, sh * ow / sw) } else { (sw * oh / sh, oh) };
    let (w, h) = ((w as u32).max(2) & !1, (h as u32).max(2) & !1);
    let (left, top) = (((output.0 - w.min(output.0)) / 2) & !1, ((output.1 - h.min(output.1)) / 2) & !1);
    (left, top, left + w, top + h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: [(&str, bool); 3] = [("left", false), ("main", true), ("right", false)];

    #[test]
    fn start_notice_says_every_fallback() {
        use HdrFallback::*;
        assert_eq!(start_notice(true, false, NotNeeded), Some(WAITING_FOR_WINDOW));
        assert_eq!(start_notice(false, false, NotNeeded), None);
        assert_eq!(start_notice(false, true, NotNeeded), Some(MONITOR_GONE));
        assert_eq!(start_notice(false, false, SdrMonitor), Some(SDR_MONITOR));
        assert_eq!(start_notice(false, true, SdrMonitor), Some(MONITOR_GONE_SDR), "both facts");
        assert_eq!(start_notice(false, false, Unknown), Some(HDR_UNKNOWN));
        assert_eq!(start_notice(false, true, Unknown), Some(MONITOR_GONE_HDR_UNKNOWN), "both facts");
        assert_eq!(start_notice(false, false, Settings), Some(HDR_SETTINGS));
        assert_eq!(start_notice(false, true, Settings), Some(MONITOR_GONE_HDR_SETTINGS), "both facts");
        assert_eq!(start_notice(false, false, Encoder), Some(HDR_ENCODER));
        assert_eq!(start_notice(false, true, Encoder), Some(MONITOR_GONE_HDR_ENCODER), "both facts");
        assert_eq!(start_notice(true, false, SdrMonitor), Some(WAITING_FOR_WINDOW), "window mode waits first");
        // Only a monitor that was checked and found SDR is blamed on Windows' HDR setting.
        assert!(!HDR_UNKNOWN.contains("isn't showing HDR") && !MONITOR_GONE_HDR_UNKNOWN.contains("isn't showing"));
    }

    #[test]
    fn hdr_never_refuses_it_falls_back_to_standard_video() {
        use HdrFallback::*;
        assert_eq!(hdr_settings(false, false, "mov"), NotNeeded, "HDR off: nothing to check");
        assert_eq!(hdr_settings(true, true, "mp4"), NotNeeded);
        assert_eq!(hdr_settings(true, true, "mkv"), NotNeeded);
        assert_eq!(hdr_settings(true, false, "mp4"), Settings, "an encoder without HDR");
        assert_eq!(hdr_settings(true, true, "mov"), Settings, "a file format without HDR");
        // Every fallback has something to say on Home, once, instead of an error per key press.
        for fallback in [SdrMonitor, Unknown, Settings, Encoder] {
            assert!(start_notice(false, false, fallback).is_some_and(|n| n.contains("standard video")), "{fallback:?}");
        }
        let hdr = window_hdr_notice(true, SdrMonitor, Some(false));
        assert_eq!(hdr, Some(HDR_WINDOW), "the game's monitor, not the Video page's, is blamed");
        assert_eq!(window_notice(true, false, false, hdr), Some(HDR_WINDOW));
        assert_eq!(window_notice(true, false, false, None), None);
        assert_eq!(window_notice(true, false, true, hdr), Some(WINDOW_MINIMIZED), "the nearer fact wins");
        assert_eq!(window_notice(false, false, false, hdr), Some(WAITING_FOR_WINDOW));
        assert_eq!(window_hdr_notice(true, Settings, Some(false)), Some(HDR_SETTINGS));
        assert_eq!(window_hdr_notice(true, Encoder, None), Some(HDR_ENCODER), "the standard retry still says why");
        assert_eq!(window_hdr_notice(true, NotNeeded, Some(true)), None, "recording HDR");
        assert_eq!(window_hdr_notice(false, SdrMonitor, Some(false)), None, "HDR video off");
        assert_eq!(window_hdr_notice(true, Unknown, None), Some(HDR_UNKNOWN), "the game's monitor couldn't be checked");
        assert_eq!(window_hdr_notice(true, SdrMonitor, None), Some(HDR_UNKNOWN), "not blamed: Windows didn't say");
        assert_eq!(window_hdr_notice(true, Unknown, Some(false)), Some(HDR_WINDOW), "the game's monitor checked fine");
    }

    #[test]
    fn window_mode_restarts_when_the_game_is_on_the_other_kind_of_monitor() {
        use HdrFallback::*;
        assert!(!window_hdr_restart(true, NotNeeded, true), "HDR session, HDR monitor");
        assert!(window_hdr_restart(true, NotNeeded, false), "HDR session, game on an SDR monitor");
        assert!(window_hdr_restart(true, SdrMonitor, true), "started on an SDR monitor, game on HDR");
        assert!(window_hdr_restart(true, Unknown, true));
        assert!(!window_hdr_restart(true, SdrMonitor, false));
        assert!(!window_hdr_restart(true, Settings, true), "settings can't do HDR anywhere");
        assert!(!window_hdr_restart(true, Encoder, true), "an encoder that failed would fail again");
        assert!(!window_hdr_restart(false, NotNeeded, false), "HDR video off");
        // A recording holds the restart off: Home says what happens when it ends.
        assert_eq!(hdr_after_recording(true, false), Some(HDR_AFTER_RECORDING));
        assert_eq!(hdr_after_recording(true, true), Some(SDR_AFTER_RECORDING));
        assert_eq!(hdr_after_recording(false, false), None, "no recording: the restart comes soon");
    }

    #[test]
    fn the_saved_monitor_or_the_main_one() {
        assert_eq!(pick_monitor("", &LIST), Some((1, false)), "fresh install: the main monitor");
        assert_eq!(pick_monitor("right", &LIST), Some((2, false)));
        assert_eq!(pick_monitor("RIGHT", &LIST), Some((2, false)), "device paths ignore case");
        assert_eq!(pick_monitor("unplugged", &LIST), Some((1, true)), "gone: main, with a notice");
        assert_eq!(pick_monitor("x", &[("a", false)]), Some((0, true)), "no primary flag: the first");
        assert_eq!(pick_monitor("", &[]), None);
    }

    #[test]
    fn capture_moves_back_when_the_monitor_does() {
        let main_only = [("main", true)];
        // The saved monitor unplugged: capture fell back to the main one and stays there meanwhile.
        assert!(!should_move("main", "right", &main_only));
        assert!(should_move("main", "right", &LIST), "plugged back in: back to it");
        assert!(should_move("main", "RIGHT", &LIST), "device paths ignore case");
        assert!(!should_move("right", "right", &LIST), "already on it");
        assert!(should_move("right", "right", &main_only), "gone without its capture closing: the main one");
        // Nothing saved: the main monitor, which changes when the main one is unplugged and back.
        assert!(!should_move("main", "", &LIST));
        assert!(should_move("left", "", &LIST), "the main monitor came back");
        assert!(!should_move("main", "right", &[]), "no monitors at all: nothing to move to");
    }

    #[test]
    fn window_mode_sticks_with_the_game() {
        let game = (100, 0x10);
        assert!(should_capture(None, game, false, false), "nothing yet: the program kept in front");
        assert!(!should_capture(Some(game), game, false, true), "already captured");
        assert!(should_capture(Some(game), (100, 0x20), false, false), "the game's new window");
        assert!(!should_capture(Some(game), (200, 0x30), false, false), "Alt+Tab to a chat stays out");
        assert!(should_capture(Some(game), (200, 0x30), true, false), "another full-screen game takes over");
        // A chat or an unlisted launcher captured first: the windowed game it starts takes over.
        assert!(should_capture(Some(game), (200, 0x30), false, true), "a program the captured one started");
    }

    #[test]
    fn only_full_screen_windows_cover_the_monitor() {
        let monitor = (0, 0, 1920, 1080);
        assert!(covers((0, 0, 1920, 1080), monitor), "borderless");
        assert!(covers((-8, -8, 1928, 1088), monitor), "a frame hanging over the edges");
        assert!(!covers((-8, -8, 1928, 1040), monitor), "maximized: the taskbar shows");
        assert!(!covers((1920, 0, 3840, 1080), monitor), "on the next monitor");
    }

    #[test]
    fn window_notices() {
        assert_eq!(window_notice(false, false, false, None), Some(WAITING_FOR_WINDOW));
        assert_eq!(window_notice(false, true, false, None), Some(WINDOW_CLOSED));
        assert_eq!(window_notice(true, false, true, None), Some(WINDOW_MINIMIZED));
        assert_eq!(window_notice(true, true, false, None), None, "a new window after one closed");
    }

    #[test]
    fn size_changes_wait_until_the_drag_stops() {
        let mut settle = SizeSettle::default();
        let now = (800, 600);
        assert!(settle.observe(now, now, 0));
        assert!(!settle.observe(now, (810, 600), 10));
        assert!(!settle.observe(now, (820, 600), 20), "still dragging");
        assert!(!settle.observe(now, (820, 600), 20 + SIZE_SETTLE - 1));
        assert!(settle.observe(now, (820, 600), 20 + SIZE_SETTLE), "held long enough");
        assert!(settle.observe((820, 600), (820, 600), 20 + SIZE_SETTLE + 1));
    }

    #[test]
    fn pictures_keep_their_shape_inside_the_frame() {
        assert_eq!(fit((1920, 1080), (1920, 1080)), (0, 0, 1920, 1080), "same shape fills");
        assert_eq!(fit((3840, 2160), (1920, 1080)), (0, 0, 1920, 1080), "scaled down");
        assert_eq!(fit((800, 600), (1920, 1080)), (240, 0, 1680, 1080), "4:3 gets bars left and right");
        assert_eq!(fit((1920, 400), (1920, 1080)), (0, 340, 1920, 740), "wide gets bars top and bottom");
        let (l, t, r, b) = fit((1001, 777), (1280, 720));
        assert!([l, t, r, b].iter().all(|v| v % 2 == 0), "even for NV12");
        assert!(r <= 1280 && b <= 720 && r - l <= 1280);
        assert!(fit((0, 0), (64, 36)).2 <= 64, "an empty window doesn't divide by zero");
    }
}
