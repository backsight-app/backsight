//! Short sounds for actions (ADR 0010), made in code as WAV files in memory: no files to ship,
//! and Windows plays them in any game, full screen included. [`play`] is the only Windows call.

use std::sync::OnceLock;

/// What happened, for its sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// A recording started or resumed.
    Start,
    /// A recording, clip or recovered file was saved.
    Saved,
    Screenshot,
    Pause,
    Marker,
    MicOn,
    MicOff,
    /// Something didn't work.
    Problem,
    /// Nothing worth a sound (a reminder like "not recording").
    Quiet,
}

const RATE: u32 = 44_100;
const ALL: [Cue; 9] = [
    Cue::Start,
    Cue::Saved,
    Cue::Screenshot,
    Cue::Pause,
    Cue::Marker,
    Cue::MicOn,
    Cue::MicOff,
    Cue::Problem,
    Cue::Quiet,
];

/// Plays `cue` without waiting; a newer sound cuts off the one playing.
pub fn play(cue: Cue) {
    if let Some(wav) = sound(cue) {
        crate::system::play_wav(wav);
    }
}

/// The cue's WAV, made once; `None` for [`Cue::Quiet`].
fn sound(cue: Cue) -> Option<&'static [u8]> {
    if cue == Cue::Quiet {
        return None;
    }
    static WAVS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    let wavs = WAVS.get_or_init(|| ALL.iter().map(|&c| wav(c)).collect());
    let i = ALL.iter().position(|&c| c == cue).expect("every cue is in ALL");
    Some(&wavs[i])
}

/// `(frequency Hz, milliseconds)` notes played one after another; 0 Hz is a short burst of noise.
fn notes(cue: Cue) -> &'static [(f32, u32)] {
    match cue {
        Cue::Start => &[(660.0, 70), (990.0, 110)],
        Cue::Saved => &[(880.0, 70), (1320.0, 160)],
        Cue::Screenshot => &[(0.0, 35), (1760.0, 45)],
        Cue::Pause => &[(660.0, 80), (495.0, 110)],
        Cue::Marker => &[(1320.0, 60)],
        Cue::MicOn => &[(784.0, 50), (1046.0, 70)],
        Cue::MicOff => &[(1046.0, 50), (784.0, 70)],
        Cue::Problem => &[(330.0, 110), (247.0, 170)],
        Cue::Quiet => &[],
    }
}

/// A mono 16-bit PCM WAV file of the cue's notes, each faded in and out so nothing clicks.
fn wav(cue: Cue) -> Vec<u8> {
    let mut samples = Vec::new();
    let mut noise = 0x1234_5678u32;
    for &(hz, ms) in notes(cue) {
        let n = (RATE * ms / 1000) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let fade = (i.min(n - i) as f32 / (RATE as f32 * 0.008)).min(1.0);
            let decay = 1.0 - 0.5 * i as f32 / n as f32;
            let v = if hz == 0.0 {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                noise as f32 / u32::MAX as f32 * 2.0 - 1.0
            } else {
                let w = std::f32::consts::TAU * hz * t;
                w.sin() + 0.25 * (2.0 * w).sin()
            };
            samples.push((v * fade * decay * 0.22 * i16::MAX as f32) as i16);
        }
    }
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    samples.iter().for_each(|s| out.extend_from_slice(&s.to_le_bytes()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cue_is_a_short_valid_wav() {
        for cue in ALL.into_iter().filter(|&c| c != Cue::Quiet) {
            let w = wav(cue);
            assert_eq!((&w[0..4], &w[8..16], &w[36..40]), (&b"RIFF"[..], &b"WAVEfmt "[..], &b"data"[..]), "{cue:?}");
            let size = |at: usize| u32::from_le_bytes(w[at..at + 4].try_into().unwrap()) as usize;
            assert_eq!((size(4), size(40)), (w.len() - 8, w.len() - 44), "{cue:?}: sizes match the bytes");
            let ms = (w.len() - 44) / 2 * 1000 / RATE as usize;
            assert!((40..=400).contains(&ms), "{cue:?} lasts {ms} ms");
            let peak = w[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs()).max().unwrap();
            assert!((2000..i16::MAX as u16 / 3).contains(&peak), "{cue:?}: audible, not loud ({peak})");
        }
    }

    #[test]
    fn each_cue_plays_its_own_sound_and_quiet_plays_none() {
        assert_eq!(sound(Cue::Quiet), None);
        for cue in ALL.into_iter().filter(|&c| c != Cue::Quiet) {
            assert_eq!(sound(cue), Some(&wav(cue)[..]), "{cue:?}");
        }
    }

    fn samples(cue: Cue) -> Vec<i16> {
        wav(cue)[44..].chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
    }

    fn crossings(s: &[i16]) -> usize {
        s.windows(2).filter(|p| (p[0] < 0) != (p[1] < 0)).count()
    }

    fn peak(s: &[i16]) -> u16 {
        s.iter().map(|v| v.unsigned_abs()).max().unwrap()
    }

    #[test]
    fn the_header_says_mono_16_bit_at_the_rate() {
        let w = wav(Cue::Marker);
        let u16_at = |at: usize| u16::from_le_bytes([w[at], w[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(w[at..at + 4].try_into().unwrap());
        assert_eq!((u32_at(16), u16_at(20), u16_at(22)), (16, 1, 1), "PCM, mono");
        assert_eq!((u32_at(24), u32_at(28), u16_at(32), u16_at(34)), (RATE, RATE * 2, 2, 16));
    }

    #[test]
    fn a_note_is_a_tone_at_its_pitch_fading_in_decaying_and_fading_out() {
        // Marker: one 1320 Hz note for 60 ms, so about 2 × 1320 × 0.06 = 158 zero crossings.
        let s = samples(Cue::Marker);
        assert!((150..=170).contains(&crossings(&s)), "{} zero crossings", crossings(&s));
        let (ms, all) = (RATE as usize / 1000, peak(&s));
        let (head, tail) = (peak(&s[..ms]), peak(&s[s.len() - ms..]));
        assert!(head < all / 4 && tail < all / 4, "fades: {head} and {tail} at the ends, {all} peak");
        let half = s.len() / 2;
        let (early, late) = (peak(&s[10 * ms..half]), peak(&s[half..s.len() - 10 * ms]));
        assert!(late < early * 9 / 10, "decays: {early} then {late}");
    }

    #[test]
    fn the_screenshot_click_starts_with_noise() {
        // 35 ms of noise crosses zero far more often than any note.
        let s = samples(Cue::Screenshot);
        let noise = &s[..RATE as usize * 35 / 1000];
        assert!(crossings(noise) > noise.len() / 4, "{} crossings", crossings(noise));
    }

    #[test]
    fn cues_sound_different() {
        let all: Vec<_> = ALL.into_iter().filter(|&c| c != Cue::Quiet).map(notes).collect();
        for (i, a) in all.iter().enumerate() {
            assert!(!all[i + 1..].contains(a), "two cues share {a:?}");
        }
    }
}
