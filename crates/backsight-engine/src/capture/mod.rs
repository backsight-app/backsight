//! Screen capture on the GPU: one shared D3D11 device ([`d3d`]), Windows.Graphics.Capture
//! ([`wgc`]) and BGRA to NV12 conversion ([`convert`]). See `docs/architecture/capture-pipeline.md`.

pub mod convert;
pub mod d3d;
pub mod foreground;
pub(crate) mod hdr;
pub mod target;
pub mod wgc;
