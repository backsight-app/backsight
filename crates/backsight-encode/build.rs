//! Copies the FFmpeg DLLs into this build script's `OUT_DIR` and declares it a native search path.
//! Cargo puts search paths inside the target directory on `PATH` when it runs tests, benches and
//! `cargo run` for this crate and everything depending on it, wherever the test binaries live
//! (Rust 1.100 moved each one into its own folder). The app itself gets its DLLs from Tauri:
//! `bundle.resources` in `app/src-tauri/tauri.conf.json` puts them beside `backsight.exe` for
//! `tauri dev`, `tauri build` and the installer.

use std::{env, fs, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let bin = manifest.join("../../third_party/ffmpeg/bin");
    println!("cargo:rerun-if-changed={}", bin.display());
    let Ok(entries) = fs::read_dir(&bin) else {
        println!("cargo:warning=FFmpeg DLLs not found in {}. Run scripts/fetch-ffmpeg.ps1.", bin.display());
        return;
    };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    for dll in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "dll")) {
        fs::copy(&dll, out.join(dll.file_name().unwrap())).unwrap();
    }
    println!("cargo:rustc-link-search=native={}", out.display());
}
