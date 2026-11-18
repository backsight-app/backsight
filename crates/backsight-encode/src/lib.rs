//! Encoding for Backsight.
//!
//! The [`catalog`] holds every container, encoder and option the app offers, loaded
//! from `content/encoders.json`; [`options`] turns chosen values into FFmpeg options.
//! [`session`] encodes, [`mux`] writes files and [`probe`] finds the encoders that work;
//! [`media`] reads saved files back for the library (metadata, thumbnails, hover previews).
//! The FFmpeg encode and mux pipeline is described in `docs/architecture/capture-pipeline.md`.

pub mod catalog;
pub mod ffmpeg;
pub mod fsutil;
pub mod media;
pub mod mux;
pub mod options;
pub mod probe;
pub mod remux;
pub mod screenshot;
pub mod session;
pub mod vertical;

pub use catalog::{Catalog, OptionDef};
