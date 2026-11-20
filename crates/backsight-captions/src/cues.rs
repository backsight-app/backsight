//! Pure caption logic: joining the speech stretches Whisper reads, mapping its times back to the
//! clip, cleaning its segments into readable cues, and writing SubRip (`.srt`) text.

use backsight_encode::media::SPEECH_RATE;

const RATE: usize = SPEECH_RATE as usize;

/// A stretch of speech in the clip's audio, as sample indices at 16 kHz: `(start, end)`.
pub type Span = (usize, usize);

/// Silence put between joined speech stretches (whisper.cpp's own VAD uses 0.1 s too).
pub const GAP: usize = RATE / 10;

/// Longest caption line, in characters (the usual subtitle limit).
pub const LINE: usize = 42;

/// Segments Whisper itself thinks are probably not speech.
const NO_SPEECH: f32 = 0.6;
/// Lines of one segment that are this close in time are shown together.
const PAIR_GAP_MS: u64 = 500;
/// A cue stays up at most this long, and at least `MIN_MS` when the next one allows.
const MAX_MS: u64 = 6000;
const MIN_MS: u64 = 1000;

/// Phrases Whisper is known to invent on silence or noise; dropped when they are a whole segment.
const HALLUCINATIONS: &[&str] = &[
    "thanks for watching",
    "thank you for watching",
    "thank you so much for watching",
    "please subscribe",
    "subtitles by the amaraorg community",
    "you",
];

/// One piece of text Whisper heard, with times in milliseconds from the clip's start.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Whisper's estimate that this is not speech at all (0 to 1).
    pub no_speech: f32,
}

/// One caption on screen: one or two lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub lines: Vec<String>,
}

/// The speech stretches of `audio` back to back with [`GAP`] of silence between, so Whisper
/// reads speech only (less work, and no invented words in the silence).
pub fn join_speech(audio: &[f32], spans: &[Span]) -> Vec<f32> {
    let mut out = Vec::with_capacity(spans.iter().map(|(a, b)| b - a + GAP).sum());
    for &(a, b) in spans {
        out.extend_from_slice(&audio[a.min(audio.len())..b.min(audio.len())]);
        out.resize(out.len() + GAP, 0.0);
    }
    out
}

/// A time in the joined audio (ms) as a time in the clip (ms), and the index of the stretch it
/// falls in. A time in a gap maps to the end of the stretch before it.
fn locate(spans: &[Span], joined_ms: u64) -> (usize, u64) {
    let t = joined_ms as usize * RATE / 1000;
    let mut at = 0;
    for (i, &(a, b)) in spans.iter().enumerate() {
        let len = b.saturating_sub(a);
        if t < at + len {
            return (i, ((a + t - at) * 1000 / RATE) as u64);
        }
        at += len + GAP;
        if t < at || i + 1 == spans.len() {
            return (i, (b * 1000 / RATE) as u64);
        }
    }
    (0, 0)
}

/// A time in the joined audio (ms) as a time in the clip (ms).
pub fn to_clip_ms(spans: &[Span], joined_ms: u64) -> u64 {
    locate(spans, joined_ms).1
}

/// A start and end in the joined audio as clip times. The end never runs past the stretch the
/// start is in: Whisper's ends often reach a little into the next stretch, which in the clip can
/// be many seconds later.
pub fn to_clip_range(spans: &[Span], start_ms: u64, end_ms: u64) -> (u64, u64) {
    let (i, start) = locate(spans, start_ms);
    let (j, end) = locate(spans, end_ms.max(start_ms));
    let end = if j == i { end } else { (spans[i].1 * 1000 / RATE) as u64 };
    (start, end.max(start))
}

/// One piece of a Whisper segment: its text bytes (a character can be split over two tokens)
/// and its times in the joined audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub bytes: Vec<u8>,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// A pause this long in the clip starts a new segment, even inside one of Whisper's.
const PAUSE_MS: u64 = 1000;

/// One Whisper segment's tokens as clip-time segments, split where the clip pauses, so a caption
/// never stays up through seconds of silence that the joined audio left out.
pub fn split_segment(spans: &[Span], tokens: &[Token], no_speech: f32) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut bytes = Vec::new();
    let mut piece: Option<(u64, u64)> = None;
    let mut flush = |bytes: &mut Vec<u8>, (start_ms, end_ms): (u64, u64)| {
        let text = String::from_utf8_lossy(&std::mem::take(bytes)).into_owned();
        out.push(Segment { start_ms, end_ms, text, no_speech });
    };
    for t in tokens.iter().filter(|t| !t.bytes.is_empty()) {
        let (start, end) = to_clip_range(spans, t.start_ms, t.end_ms);
        piece = match piece {
            Some(p) if start > p.1 + PAUSE_MS => {
                flush(&mut bytes, p);
                Some((start, end))
            }
            Some((s, e)) => Some((s, e.max(end))),
            None => Some((start, end)),
        };
        bytes.extend_from_slice(&t.bytes);
    }
    if let Some(p) = piece {
        flush(&mut bytes, p);
    }
    out
}

/// Lowercase letters and digits only, for comparing phrases.
fn simple(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().to_lowercase()
}

/// Not words worth showing: empty, only punctuation, a sound tag like `[BLANK_AUDIO]` or
/// `(music)`, or a phrase Whisper makes up on silence.
fn is_noise(text: &str) -> bool {
    let tagged = |open, close| text.starts_with(open) && text.ends_with(close);
    let words = simple(text);
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    words.is_empty()
        || tagged('[', ']')
        || tagged('(', ')')
        || tagged('*', '*')
        || tagged('♪', '♪')
        || HALLUCINATIONS.contains(&words.as_str())
}

/// `text` in lines of at most `width` characters, broken at spaces; a longer word (or text
/// without spaces, such as Chinese or Japanese) is cut at the width.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        let used = line.chars().count();
        if used > 0 && used + 1 + word.len() <= width {
            line.push(' ');
            line.extend(&word);
            continue;
        }
        if used > 0 {
            lines.push(std::mem::take(&mut line));
        }
        while word.len() > width {
            lines.push(word.drain(..width).collect());
        }
        line = word.into_iter().collect();
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Whisper's segments as cues: noise and repeats dropped, lines of at most [`LINE`] characters,
/// two lines per cue when they follow each other closely, each cue up at least a second when
/// the next one allows and never overlapping it.
pub fn cues(segments: &[Segment]) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let mut last_text = String::new();
    for s in segments {
        let text = s.text.trim();
        if s.no_speech > NO_SPEECH || is_noise(text) || simple(text) == last_text || s.end_ms <= s.start_ms {
            continue;
        }
        last_text = simple(text);
        // A long segment's lines share its time by length.
        let lines = wrap(text, LINE);
        let chars: usize = lines.iter().map(|l| l.chars().count()).sum();
        let span = s.end_ms - s.start_ms;
        let mut at = s.start_ms;
        let mut done = 0;
        for pair in lines.chunks(2) {
            done += pair.iter().map(|l| l.chars().count()).sum::<usize>();
            let end = s.start_ms + span * done as u64 / chars as u64;
            cues.push(Cue { start_ms: at, end_ms: end, lines: pair.to_vec() });
            at = end;
        }
    }
    // Two one-line cues close together become one two-line cue.
    let mut paired: Vec<Cue> = Vec::with_capacity(cues.len());
    for c in cues {
        if let Some(p) = paired.last_mut()
            && p.lines.len() == 1
            && c.lines.len() == 1
            && c.start_ms.saturating_sub(p.end_ms) <= PAIR_GAP_MS
            && c.end_ms - p.start_ms <= MAX_MS
        {
            p.lines.extend(c.lines);
            p.end_ms = c.end_ms;
            continue;
        }
        paired.push(c);
    }
    let starts: Vec<u64> = paired.iter().skip(1).map(|c| c.start_ms).chain(Some(u64::MAX)).collect();
    for (c, next) in paired.iter_mut().zip(starts) {
        c.end_ms = c.end_ms.max(c.start_ms + MIN_MS).min(next);
    }
    paired.retain(|c| c.end_ms > c.start_ms);
    paired
}

fn timestamp(ms: u64) -> String {
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// SubRip text: UTF-8 with a byte order mark and Windows line ends, which every player reads
/// (the mark keeps non-English text right in older Windows players).
pub fn srt(cues: &[Cue]) -> String {
    let mut out = String::from("\u{feff}");
    for (i, c) in cues.iter().enumerate() {
        out += &format!("{}\r\n{} --> {}\r\n", i + 1, timestamp(c.start_ms), timestamp(c.end_ms));
        for line in &c.lines {
            out += line;
            out += "\r\n";
        }
        out += "\r\n";
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: u64, end_ms: u64, text: &str) -> Segment {
        Segment { start_ms, end_ms, text: text.into(), no_speech: 0.0 }
    }

    #[test]
    fn joined_speech_maps_back_to_clip_times() {
        let audio: Vec<f32> = (0..10 * RATE).map(|i| i as f32).collect();
        let spans = [(RATE, 2 * RATE), (5 * RATE, 7 * RATE)];
        let joined = join_speech(&audio, &spans);
        assert_eq!(joined.len(), 3 * RATE + 2 * GAP);
        assert_eq!(joined[0], RATE as f32);
        assert_eq!(joined[RATE + GAP], (5 * RATE) as f32, "second stretch after the gap");
        assert_eq!(to_clip_ms(&spans, 0), 1000);
        assert_eq!(to_clip_ms(&spans, 500), 1500);
        assert_eq!(to_clip_ms(&spans, 1050), 2000, "in the gap: end of the first stretch");
        assert_eq!(to_clip_ms(&spans, 1100), 5000);
        assert_eq!(to_clip_ms(&spans, 2600), 6500);
        assert_eq!(to_clip_ms(&spans, 9000), 7000, "past the end: end of the last stretch");
        assert_eq!(to_clip_ms(&[], 500), 0);
        assert_eq!(to_clip_range(&spans, 800, 1050), (1800, 2000), "inside one stretch");
        assert_eq!(to_clip_range(&spans, 800, 1300), (1800, 2000), "an end in the next stretch stays in this one");
        assert_eq!(to_clip_range(&spans, 1200, 1100), (5100, 5100), "never ends before it starts");
        assert_eq!(join_speech(&audio[..RATE], &[(RATE / 2, 3 * RATE)]).len(), RATE / 2 + GAP, "clamped");
    }

    #[test]
    fn segments_split_where_the_clip_pauses() {
        let spans = [(0, 2 * RATE), (10 * RATE, 12 * RATE)];
        let tok = |text: &str, start_ms, end_ms| Token { bytes: text.as_bytes().to_vec(), start_ms, end_ms };
        // "Nice shot," in the first stretch; the end of "shot," runs into the second one.
        let tokens = [
            tok(" Nice", 100, 600),
            tok(" shot,", 700, 2200),
            tok("", 2200, 2200),
            tok(" rotate", 2200, 2900),
            tok(" now", 3000, 3500),
        ];
        let s = split_segment(&spans, &tokens, 0.1);
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!((s[0].text.as_str(), s[0].start_ms, s[0].end_ms), (" Nice shot,", 100, 2000));
        assert_eq!((s[1].text.as_str(), s[1].start_ms, s[1].end_ms), (" rotate now", 10_100, 11_400));
        assert_eq!(s[1].no_speech, 0.1);
        // A character split over two tokens comes back whole.
        let e = "é".as_bytes();
        let s = split_segment(
            &spans,
            &[
                tok(" caf", 0, 100),
                Token { bytes: e[..1].to_vec(), start_ms: 100, end_ms: 150 },
                Token { bytes: e[1..].to_vec(), start_ms: 150, end_ms: 200 },
            ],
            0.0,
        );
        assert_eq!(s[0].text, " café");
        assert!(split_segment(&spans, &[], 0.0).is_empty());
        // A pause of exactly a second doesn't split.
        assert_eq!(split_segment(&spans, &[tok(" a", 0, 500), tok(" b", 1500, 1900)], 0.0).len(), 1);
    }

    #[test]
    fn wrap_breaks_at_spaces_and_cuts_long_words() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("  spaced   out  ", 42), ["spaced out"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab cd", 4), ["ab", "cd"], "the space counts");
        assert_eq!(wrap("日本語のテキスト", 3), ["日本語", "のテキ", "スト"], "counts characters, not bytes");
        assert!(wrap("", 42).is_empty());
        let long = "Okay, we are pushing the left side now. Watch the corner, there is one player behind the boxes.";
        assert!(wrap(long, LINE).iter().all(|l| l.chars().count() <= LINE));
    }

    #[test]
    fn noise_repeats_and_unlikely_speech_are_dropped() {
        let mut quiet = seg(9000, 9500, "Really?");
        quiet.no_speech = 0.9;
        let c = cues(&[
            seg(0, 1000, " [BLANK_AUDIO]"),
            seg(1000, 2000, "(music)"),
            seg(2000, 3000, " Thanks for watching!"),
            seg(3000, 4000, " ..."),
            seg(4000, 5000, " Push left."),
            seg(5000, 6000, " push left"),
            quiet,
            seg(7000, 7000, "zero length"),
        ]);
        assert_eq!(c, [Cue { start_ms: 4000, end_ms: 5000, lines: vec!["Push left.".into()] }]);
        assert!(is_noise("*laughs*") && is_noise("♪ la la ♪"));
        assert!(!is_noise("(laughs) Nice one") && !is_noise("Nice one [sic]"), "only a whole tag is noise");
        let mut edge = seg(0, 1000, "Maybe.");
        edge.no_speech = NO_SPEECH;
        assert_eq!(cues(&[edge]).len(), 1, "Whisper's own threshold is still speech");
    }

    #[test]
    fn a_cue_cut_to_nothing_by_the_next_is_dropped() {
        let two_lines = "Behind you, behind you, turn around now, quickly!";
        let c = cues(&[seg(0, 2000, two_lines), seg(0, 1000, "Go.")]);
        assert_eq!(c, [Cue { start_ms: 0, end_ms: 1000, lines: vec!["Go.".into()] }]);
    }

    #[test]
    fn close_lines_pair_up_and_long_segments_split() {
        let c = cues(&[seg(0, 1200, "Nice shot."), seg(1300, 2000, "He is low."), seg(4000, 5000, "Rotate.")]);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].lines, ["Nice shot.", "He is low."]);
        assert_eq!((c[0].start_ms, c[0].end_ms), (0, 2000));
        assert_eq!(c[1].lines, ["Rotate."]);

        let long = "word ".repeat(40); // 199 characters: five lines, three cues
        let c = cues(&[seg(10_000, 20_000, &long)]);
        assert_eq!(c.iter().map(|c| c.lines.len()).collect::<Vec<_>>(), [2, 2, 1]);
        assert_eq!(c[0].start_ms, 10_000);
        assert_eq!(c[2].end_ms, 20_000);
        assert!(c.windows(2).all(|w| w[0].end_ms == w[1].start_ms), "{c:?}");
    }

    #[test]
    fn short_cues_stay_up_a_second_without_overlapping() {
        let c = cues(&[
            seg(0, 200, "Go."),
            seg(3000, 3100, "Now."),
            seg(3600, 5000, "Behind you, behind you, turn around now!"),
        ]);
        assert_eq!((c[0].start_ms, c[0].end_ms), (0, 1000));
        assert_eq!(c[1].lines, ["Now.", "Behind you, behind you, turn around now!"]);
        assert_eq!((c[1].start_ms, c[1].end_ms), (3000, 5000));
        let c = cues(&[seg(0, 200, "Go."), seg(900, 1000, "Stop."), seg(1000, 7500, "x")]);
        assert_eq!(c.len(), 3, "too far apart, then too long together: no pairs");
        assert_eq!((c[0].end_ms, c[1].end_ms), (900, 1000), "cut at the next cue");
    }

    #[test]
    fn srt_is_numbered_with_comma_milliseconds() {
        let text = srt(&[
            Cue { start_ms: 1500, end_ms: 3_725_042, lines: vec!["Hi.".into(), "Två".into()] },
            Cue { start_ms: 4_000_000, end_ms: 4_000_001, lines: vec!["Bye.".into()] },
        ]);
        assert_eq!(
            text,
            "\u{feff}1\r\n00:00:01,500 --> 01:02:05,042\r\nHi.\r\nTvå\r\n\r\n2\r\n01:06:40,000 --> 01:06:40,001\r\nBye.\r\n\r\n"
        );
        assert_eq!(srt(&[]), "\u{feff}");
    }
}
