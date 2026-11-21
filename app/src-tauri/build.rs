/// `2026-11-12 14:03, abc1234 commit subject (plus uncommitted changes)`: the commit this build
/// was made from, in the shape of `build.txt`. Empty in a debug build (and with no git), which
/// the app calls a development build.
fn build_stamp() -> String {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    if std::env::var("PROFILE").is_ok_and(|p| p != "release") {
        return String::new();
    }
    let Some(commit) = git(&["log", "-1", "--format=%cd, %h %s", "--date=format:%Y-%m-%d %H:%M"]) else {
        return String::new();
    };
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    // Rebuilt after a new commit, a staged change or a source edit.
    for path in ["HEAD", "logs/HEAD", "index"] {
        if let Some(p) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    // Unstaged edits anywhere in what the app is built from mark the build as changed too.
    for dir in ["src", "../src", "../../crates", "../../content"] {
        println!("cargo:rerun-if-changed={dir}");
    }
    format!("{commit}{}", if dirty { " (plus uncommitted changes)" } else { "" })
}

fn main() {
    println!("cargo:rustc-env=BACKSIGHT_BUILD={}", build_stamp());
    // Listing the commands makes each one a permission, so every window gets only the commands its
    // capability names (`capabilities/`). Keep in step with `generate_handler!` in main.rs.
    let commands = &[
        "get_settings",
        "save_settings",
        "get_status",
        "replay_menu_action",
        "get_diagnostics",
        "list_microphones",
        "captions_status",
        "download_captions",
        "stop_captions_download",
        "remove_captions",
        "list_monitors",
        "list_library",
        "library_preview",
        "library_action",
        "recovered_originals",
        "region_image",
        "region_show",
        "region_pick",
        "hud_poll",
        "hud_idle",
        "hud_drag",
        "hud_arrange",
        "local_builds",
        "install_local_build",
        "check_update",
        "install_update",
        "encoder_check",
        "setup_check",
        "overload_status",
        "lighter_video",
        "tune_check",
        "auto_change",
        "auto_change_dismiss",
        "overlay_scan",
        "crash_notice",
        "clips_space",
        "press_action",
        "crash_dismiss",
        "crash_report_text",
        "crash_report_send",
        "crash_open_folder",
        "open_logs_folder",
        "about_open",
        "pause_hotkeys",
        "save_problem_report",
        "ui_log",
        "ui_heartbeat",
    ];
    let attributes = tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(commands));
    tauri_build::try_build(attributes).expect("failed to run the Tauri build script");
}
