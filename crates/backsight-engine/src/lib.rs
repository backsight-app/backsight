//! The Backsight engine: the always-running core behind the tray icon.
//!
//! A [`session::ReplaySession`] runs Instant Replay end to end: [`capture`] (Windows.Graphics.Capture
//! and GPU conversion) and [`audio`] (WASAPI loopback) feed the encoders, whose packets go into
//! the [`replay`] buffer, paced by the frame [`pacer`]. [`input`] turns Raw Input into
//! [`hotkeys`] actions, [`library`] lists saved clips and screenshots, [`recovery`] finds and
//! recovers interrupted recordings, and [`logging`] writes the log. See
//! `docs/architecture/overview.md`.

pub mod audio;
pub mod autostart;
pub mod capture;
pub mod crash;
pub mod game;
pub mod hotkeys;
pub mod input;
mod job;
pub mod library;
pub mod logging;
pub mod overlays;
pub mod overload;
pub mod pacer;
pub mod recording;
pub mod recovery;
pub mod replay;
pub mod screenshot;
pub mod session;
pub mod setup;
pub mod shutdown;
pub mod sounds;
pub mod stack;
pub mod system;
pub mod vertical;
pub mod watchdog;
