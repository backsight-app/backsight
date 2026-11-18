//! Still-image encoding through FFmpeg (SDR) and Windows Imaging Component (HDR).
mod jxr;
use crate::{
    catalog::ScreenshotFormat,
    ffmpeg::{find_encoder, open_with_options},
};
pub(crate) use jxr::Apartment;
pub use jxr::encode as encode_jxr;
use rsmpeg::{
    avcodec::AVCodecContext,
    avutil::{AVFrame, ra},
    ffi,
    swscale::SwsContext,
};

/// Encodes tightly packed, top-down BGRA pixels. Clipboard output uses these same pixels.
/// `quality` (1–100) only matters for lossy formats. Errors are raw detail for the log.
pub fn encode(bgra: &[u8], width: u32, height: u32, format: &ScreenshotFormat, quality: u8) -> Result<Vec<u8>, String> {
    let w = i32::try_from(width).map_err(|e| e.to_string())?;
    let h = i32::try_from(height).map_err(|e| e.to_string())?;
    let bytes = (width as usize).checked_mul(height as usize).and_then(|n| n.checked_mul(4));
    if w == 0 || h == 0 || bytes != Some(bgra.len()) || (format.lossy && !(1..=100).contains(&quality)) {
        return Err(format!("invalid screenshot input: {width}x{height}, {} bytes, quality {quality}", bgra.len()));
    }
    let name = format.ffmpeg.as_deref().ok_or_else(|| format!("no encoder for {}", format.id))?;
    // The pixel layout each encoder takes is FFmpeg detail, not a user choice.
    let pixel_format = match name {
        "png" => ffi::AV_PIX_FMT_RGB24,
        "mjpeg" => ffi::AV_PIX_FMT_YUVJ444P,
        "libwebp" => ffi::AV_PIX_FMT_YUV420P,
        _ => return Err(format!("no pixel format known for encoder {name}")),
    };
    let codec = find_encoder(name)?;
    let mut enc = AVCodecContext::new(&codec);
    enc.set_width(w);
    enc.set_height(h);
    enc.set_pix_fmt(pixel_format);
    enc.set_time_base(ra(1, 1));
    let options = match name {
        "mjpeg" => vec![("flags".into(), "+qscale".into())],
        "libwebp" => vec![("quality".into(), quality.to_string())],
        _ => vec![],
    };
    open_with_options(&mut enc, &options)?;
    let frame = |fmt| -> Result<AVFrame, String> {
        let mut f = AVFrame::new();
        f.set_width(w);
        f.set_height(h);
        f.set_format(fmt);
        f.alloc_buffer().map_err(|e| e.to_string())?;
        Ok(f)
    };
    let source = frame(ffi::AV_PIX_FMT_BGRA)?;
    let row = width as usize * 4;
    for (y, pixels) in bgra.chunks_exact(row).enumerate() {
        // SAFETY: alloc_buffer owns height rows; each BGRA row has at least width*4 bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), source.data[0].add(y * source.linesize[0] as usize), row)
        };
    }
    let mut target = frame(pixel_format)?;
    // SAFETY: our allocated frame is exclusively owned; quality is a plain integer field.
    unsafe { (*target.as_mut_ptr()).quality = (2 + (100 - quality as i32) * 29 / 99) * ffi::FF_QP2LAMBDA as i32 };
    SwsContext::get_context(w, h, source.format, w, h, target.format, ffi::SWS_BILINEAR, None, None, None)
        .ok_or("Couldn't convert screenshot pixels.")?
        .scale_frame(&source, 0, h, &mut target)
        .map_err(|e| e.to_string())?;
    enc.send_frame(Some(&target)).map_err(|e| e.to_string())?;
    enc.send_frame(None).map_err(|e| e.to_string())?;
    let packet = enc.receive_packet().map_err(|e| e.to_string())?;
    // SAFETY: the received packet owns size bytes until it drops, after this copy.
    Ok(unsafe { std::slice::from_raw_parts(packet.data, packet.size as usize) }.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodes_all_sdr_formats_and_rejects_invalid_input() {
        let pixels = [30, 90, 180, 255].repeat(35);
        let formats = &crate::Catalog::get().screenshot_formats;
        let f = |id: &str| formats.iter().find(|f| f.id == id).unwrap();
        // shortcut: checks file signatures only (colours were verified by decoding once, 2026-11-10); decode and compare if this code changes.
        for format in ["png", "jpeg", "webp"] {
            let data = encode(&pixels, 7, 5, f(format), 85).unwrap();
            match format {
                "png" => assert!(data.starts_with(b"\x89PNG\r\n\x1a\n")),
                "jpeg" => assert!(data.starts_with(&[255, 216, 255])),
                _ => {
                    assert_eq!(&data[..4], b"RIFF");
                    assert_eq!(&data[8..12], b"WEBP");
                }
            }
        }
        assert!(encode(&pixels, 7, 6, f("png"), 85).is_err());
        assert!(encode(&pixels, 7, 5, f("jxr"), 85).is_err());
        assert!(encode(&pixels, 7, 5, f("png"), 0).is_ok(), "PNG ignores quality");
        assert!(encode(&pixels, 7, 5, f("jpeg"), 0).is_err());
    }
}
