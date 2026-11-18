//! The whisper.cpp edge: find the speech (Silero VAD), then turn it into text (Whisper).

use crate::cues::{Segment, Span, Token, join_speech, split_segment, to_clip_range};
use std::path::Path;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext, WhisperVadContextParams,
    WhisperVadParams,
};

/// Why a run failed, so the caller can tell the user what to do.
#[derive(Debug)]
pub enum Error {
    /// A model file wouldn't load: damaged or not a model.
    Model(String),
    /// Whisper failed on this audio.
    Run(String),
}

fn path_str(p: &Path) -> Result<&str, Error> {
    p.to_str().ok_or_else(|| Error::Model(format!("{} is not a Unicode path", p.display())))
}

/// Transcribes `audio` (mono, 16 kHz) with the Whisper model at `model` and the VAD model at
/// `vad`, on `threads` threads. `loaded` runs once both models are in memory (to lower this
/// process's priority for the long part). `language` is a Whisper code such as `en`, or `auto`.
/// Times are milliseconds from the start of `audio`; empty when nobody talks.
pub fn transcribe(
    audio: &[f32],
    model: &Path,
    vad: &Path,
    language: &str,
    threads: usize,
    loaded: impl FnOnce(),
) -> Result<Vec<Segment>, Error> {
    whisper_rs::install_logging_hooks(); // whisper.cpp's own console output, dropped
    let threads = threads as i32;
    let ctx = WhisperContext::new_with_params(path_str(model)?, WhisperContextParameters::default())
        .map_err(|e| Error::Model(format!("whisper model: {e}")))?;
    let mut state = ctx.create_state().map_err(|e| Error::Run(format!("whisper state: {e}")))?;
    let mut vad_params = WhisperVadContextParams::new();
    vad_params.set_n_threads(threads);
    vad_params.set_use_gpu(false);
    let mut vad =
        WhisperVadContext::new(path_str(vad)?, vad_params).map_err(|e| Error::Model(format!("VAD model: {e}")))?;
    loaded();

    if audio.is_empty() {
        return Ok(Vec::new());
    }
    // whisper-rs's `state.full` skips whisper.cpp's built-in VAD (that runs only in `whisper_full`),
    // so the speech is found and joined here.
    let found =
        vad.segments_from_samples(WhisperVadParams::new(), audio).map_err(|e| Error::Run(format!("VAD: {e}")))?;
    // Centiseconds to samples at 16 kHz.
    let spans: Vec<Span> = (0..found.num_segments())
        .filter_map(|i| Some((found.get_segment_start_timestamp(i)?, found.get_segment_end_timestamp(i)?)))
        .map(|(a, b)| ((a.max(0.0) * 160.0) as usize, ((b.max(0.0) * 160.0) as usize).min(audio.len())))
        .filter(|(a, b)| b > a)
        .collect();
    if spans.is_empty() {
        return Ok(Vec::new());
    }
    let speech = join_speech(audio, &spans);

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(threads);
    params.set_language(Some(language));
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_special(false);
    params.set_print_timestamps(false);
    // Each window on its own: one misheard phrase can't start a loop of repeats.
    params.set_no_context(true);
    params.set_suppress_nst(true);
    // Short segments with word-level times, split at spaces: caption-sized pieces.
    params.set_token_timestamps(true);
    params.set_max_len(crate::cues::LINE as i32);
    params.set_split_on_word(true);
    state.full(params, &speech).map_err(|e| Error::Run(format!("whisper: {e}")))?;

    // Whisper's times are centiseconds of the joined speech; special tokens (timestamps, end of
    // text) have ids from `eot` up.
    let eot = ctx.token_eot();
    let mut out = Vec::new();
    for s in state.as_iter() {
        let mut tokens = Vec::new();
        for i in 0..s.n_tokens() {
            let Some(t) = s.get_token(i) else { continue };
            if t.token_id() >= eot {
                continue;
            }
            let data = t.token_data();
            let bytes = t.to_bytes().map_err(|e| Error::Run(format!("token text: {e}")))?.to_vec();
            tokens.push(Token { bytes, start_ms: data.t0.max(0) as u64 * 10, end_ms: data.t1.max(0) as u64 * 10 });
        }
        if tokens.is_empty() {
            let text = s.to_str_lossy().map_err(|e| Error::Run(format!("segment text: {e}")))?.into_owned();
            let ms = |cs: i64| cs.max(0) as u64 * 10;
            let (start_ms, end_ms) = to_clip_range(&spans, ms(s.start_timestamp()), ms(s.end_timestamp()));
            out.push(Segment { start_ms, end_ms, text, no_speech: s.no_speech_probability() });
        } else {
            out.extend(split_segment(&spans, &tokens, s.no_speech_probability()));
        }
    }
    Ok(out)
}
