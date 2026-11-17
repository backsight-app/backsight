//! Shared FFmpeg helpers. With [`crate::session`] and [`crate::mux`] this is the FFmpeg edge,
//! the only code in `backsight-encode` that calls `rsmpeg::ffi`.

use crate::options::FfmpegOptions;
use rsmpeg::{
    avcodec::{AVCodec, AVCodecContext, AVCodecRef},
    avutil::AVDictionary,
    ffi,
};
use std::{
    ffi::CString,
    ptr::{self, NonNull},
};

pub(crate) fn find_encoder(name: &str) -> Result<AVCodecRef<'static>, String> {
    let cname = CString::new(name).map_err(|e| e.to_string())?;
    AVCodec::find_encoder_by_name(&cname).ok_or_else(|| format!("FFmpeg has no encoder {name}"))
}

fn cstrings(k: &str, v: &str) -> Result<(CString, CString), String> {
    Ok((CString::new(k).map_err(|e| e.to_string())?, CString::new(v).map_err(|e| e.to_string())?))
}

/// Sets each option on a fresh, unopened encoder context and reports the first one FFmpeg
/// rejects (unknown name or invalid value). Works without the encoder's hardware.
#[cfg(test)]
pub(crate) fn check_options(encoder: &str, options: &FfmpegOptions) -> Result<(), String> {
    let codec = find_encoder(encoder)?;
    let mut ctx = AVCodecContext::new(&codec);
    for (k, v) in options {
        let (ck, cv) = cstrings(k, v)?;
        // SAFETY: `ctx` owns a valid AVCodecContext (its first field is an AVClass pointer and
        // its private data is a child object) for the whole call; `ck` and `cv` are NUL-terminated.
        let ret = unsafe {
            ffi::av_opt_set(ctx.as_mut_ptr().cast(), ck.as_ptr(), cv.as_ptr(), ffi::AV_OPT_SEARCH_CHILDREN as i32)
        };
        if ret < 0 {
            return Err(format!("{k}={v} rejected ({ret})"));
        }
    }
    Ok(())
}

/// Sets an option on an unopened encoder context if the encoder has it; returns whether it did.
pub(crate) fn set_if_known(ctx: &mut AVCodecContext, key: &str, value: &str) -> bool {
    let Ok((ck, cv)) = cstrings(key, value) else { return false };
    // SAFETY: as in `check_options`: `ctx` is a valid AVCodecContext with its private data as a
    // child object, and both strings are NUL-terminated.
    let ret = unsafe {
        ffi::av_opt_set(ctx.as_mut_ptr().cast(), ck.as_ptr(), cv.as_ptr(), ffi::AV_OPT_SEARCH_CHILDREN as i32)
    };
    ret >= 0
}

/// Opens `ctx` with these options. Fails if FFmpeg refuses to open or leaves an option
/// unused (a misspelled or unsupported option would otherwise be ignored silently).
pub(crate) fn open_with_options(ctx: &mut AVCodecContext, options: &FfmpegOptions) -> Result<(), String> {
    // Not `AVCodecContext::open`: rsmpeg 0.18 frees the dictionary twice when the open fails.
    let mut dict: *mut ffi::AVDictionary = ptr::null_mut();
    for (k, v) in options {
        let (ck, cv) = cstrings(k, v)?;
        // SAFETY: `dict` is null or a dictionary only this function owns; the strings are NUL-terminated
        // and copied by FFmpeg.
        unsafe { ffi::av_dict_set(&mut dict, ck.as_ptr(), cv.as_ptr(), 0) };
    }
    // SAFETY: `ctx` is a valid, unopened context; avcodec_open2 frees our dictionary and leaves the
    // unused entries in `dict` (or null), whether it succeeds or fails.
    let ret = unsafe { ffi::avcodec_open2(ctx.as_mut_ptr(), ptr::null(), &mut dict) };
    // SAFETY: `dict` is null or a dictionary FFmpeg just handed us; rsmpeg frees it on drop.
    let unused = NonNull::new(dict).map(|p| unsafe { AVDictionary::from_raw(p) });
    if ret < 0 {
        return Err(format!("open failed: {}", error_text(ret)));
    }
    match unused {
        Some(u) => Err(format!("unused options: {:?}", u.get_string(b'=', b',').unwrap_or_default())),
        None => Ok(()),
    }
}

/// The linked FFmpeg's version, e.g. `8.1-full_build-www.gyan.dev`.
pub(crate) fn version() -> String {
    // SAFETY: av_version_info returns a static NUL-terminated string.
    unsafe { std::ffi::CStr::from_ptr(ffi::av_version_info()) }.to_string_lossy().into_owned()
}

/// FFmpeg's message for an error code, for logs.
pub(crate) fn error_text(ret: i32) -> String {
    let mut buf = [0u8; 128];
    // SAFETY: av_strerror writes at most `buf.len()` bytes, NUL-terminated, into `buf`.
    unsafe { ffi::av_strerror(ret, buf.as_mut_ptr().cast(), buf.len()) };
    let text = std::ffi::CStr::from_bytes_until_nul(&buf).map(|c| c.to_string_lossy().into_owned());
    format!("{} ({ret})", text.unwrap_or_default())
}

/// Only FFmpeg errors reach stderr (its default level prints every encoder's banner).
#[cfg(test)]
pub(crate) fn errors_only_logging() {
    // SAFETY: sets a global integer; safe to call from any thread.
    unsafe { ffi::av_log_set_level(ffi::AV_LOG_ERROR as i32) };
}

/// FFmpeg's warnings and errors since [`capture_log`] started; at most [`LOG_LINES`] lines.
static CAPTURED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
const LOG_LINES: usize = 6;

/// FFmpeg log callback: keeps warnings and errors in [`CAPTURED`] instead of printing them.
unsafe extern "C" fn keep_log(
    avcl: *mut std::ffi::c_void,
    level: std::ffi::c_int,
    fmt: *const std::ffi::c_char,
    args: ffi::va_list,
) {
    if level > ffi::AV_LOG_WARNING as i32 {
        return;
    }
    let (mut line, mut prefix) = ([0 as std::ffi::c_char; 512], 1);
    // SAFETY: FFmpeg passes a valid format string and the argument list for it; `line` has the
    // size given, and av_log_format_line2 NUL-terminates what it writes.
    let n = unsafe { ffi::av_log_format_line2(avcl, level, fmt, args, line.as_mut_ptr(), 512, &mut prefix) };
    if n < 0 {
        return;
    }
    // SAFETY: NUL-terminated by av_log_format_line2 above.
    let text = unsafe { std::ffi::CStr::from_ptr(line.as_ptr()) }.to_string_lossy();
    let text = text.trim();
    let mut kept = CAPTURED.lock().unwrap_or_else(|e| e.into_inner());
    if !text.is_empty() && kept.len() < LOG_LINES {
        kept.push(text.to_owned());
    }
}

/// Runs `f` and returns what FFmpeg warned or complained about meanwhile, so a failure can say
/// why (`open failed` alone doesn't). Global: other threads' messages in the meantime are kept too.
pub(crate) fn capture_log<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    CAPTURED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    // SAFETY: sets global FFmpeg logging state; `keep_log` matches the callback signature, may be
    // called from any thread, and the default callback is put back below.
    let before = unsafe {
        let before = ffi::av_log_get_level();
        ffi::av_log_set_level(ffi::AV_LOG_WARNING as i32);
        ffi::av_log_set_callback(Some(keep_log));
        before
    };
    let out = f();
    // SAFETY: as above.
    unsafe {
        ffi::av_log_set_callback(Some(ffi::av_log_default_callback));
        ffi::av_log_set_level(before);
    }
    (out, std::mem::take(&mut *CAPTURED.lock().unwrap_or_else(|e| e.into_inner())))
}
