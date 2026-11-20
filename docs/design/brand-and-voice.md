# Brand and voice

<img src="../../assets/brand/backsight-mark.svg" alt="Backsight mark" width="96" align="right">

## The mark

A **rewind ring** around a **record dot**: Instant Replay and recording in one shape. The ring runs clockwise from 12 o'clock to 8 o'clock, and the arrow at the top points back in time.

| File | Use |
| --- | --- |
| `assets/brand/backsight-mark.svg` | Source of truth for the app icon and in-app logo |
| `assets/brand/backsight-mark-1024.png` | Input for `tauri icon` |
| `assets/brand/banner.svg` / `.png` | README header |
| `assets/brand/social-preview.svg` / `.png` | GitHub social preview (Settings → General → Social preview) |

Keep clear space of at least 25% of the mark's width around it. Don't recolor the ring, rotate the mark, or put it on a busy photo.

## Color

**Brand art** (mark, banner, social preview):

| Token | Hex | Use |
| --- | --- | --- |
| Violet | `#8B5CF6` | Gradient start, accents |
| Indigo | `#6366F1` | Gradient middle |
| Cyan | `#22D3EE` | Gradient end |
| Record | `#FF4D6D` | Record dot |
| Background | `#0B0F1A` | Banner background |

The brand gradient is `135°, #8B5CF6 → #6366F1 → #22D3EE`.

**App UI** (since 0.1.7): near-black neutral greys with one violet accent. Tokens live in `app/src/app.css`:

| Token | Hex | Use |
| --- | --- | --- |
| `--bg` / `--bg-card` | `#0A0B0F` / `#111218` | Window background / cards |
| `--border` | `#1F212A` | Card and input borders |
| `--text` / `--text-dim` / `--text-faint` | `#ECECF1` / `#A0A2B0` / `#7E818F` | Primary / secondary / faint text |
| `--accent` | `#A395FF` | Links, focus, selected items |
| `--accent-grad` | `135°, #7859F6 → #466DE0` | Primary buttons (Save clip) |
| `--rec` | `#F0506E` | Recording state, conflicts. Never decorative |
| `--ok` / `--warn` | `#34D399` / `#D4B25B` | Status |

## Type

The app bundles **Inter** (text) and **Sora** (headings and the wordmark) as variable fonts through `@fontsource-variable`, so nothing is downloaded at run time; Segoe UI is the fallback. Brand art uses Segoe UI Variable Display (700 weight, -0.02em tracking); in its wordmark "Back" is white and "sight" carries the gradient.

## Voice

**In-app explanations** (`content/glossary.en.json` and UI text) are for players, not engineers: plain words, direct, no filler.

1. **`short` is one line, under 10 words.** It says what the setting *does*, not what it *is*. "How much data each second of video gets," not "Target bitrate in kbps."
2. **`eli5` is at most 3 sentences** and uses one everyday comparison when it helps (a tap, a box, a language). Define jargon in the same sentence, or don't use it.
3. **Give numbers people can act on.** "15–25 Mbps for 1080p60," "about 11 MB per minute."
4. **`pros` and `cons` have at most 3 items each**, short fragments, no trailing periods.
5. **`pickIf` is one sentence** starting from the user's situation: "You have an NVIDIA card and…"
6. **Be honest about trade-offs.** Never call an option "best" without saying for what.
7. **Say "you," not "the user."** No exclamation marks. No blame ("invalid input"); say what to do instead.

**Documentation** (README, docs/) is concise and technical: short declarative sentences, no step-by-step instructions for obvious actions, no exclamation marks, no emoji except the roadmap's status marks. Terms are consistent: Instant Replay, clip (saved from Instant Replay), recording, screenshot.
