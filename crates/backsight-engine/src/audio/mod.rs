//! Audio capture: WASAPI loopback ([`wasapi`]), the microphone ([`microphone`]) and one
//! process tree ([`process_loopback`]), each kept continuous on the QPC clock
//! ([`timeline`]), then mixed into the recorded tracks ([`mixer`]). See
//! `docs/architecture/audio.md`.

pub mod devices;
pub mod microphone;
pub mod mixer;
pub mod process_loopback;
pub(crate) mod session_audio;
pub mod timeline;
pub mod wasapi;
