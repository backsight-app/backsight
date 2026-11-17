# Settings and content

## The content folder

`content/` holds data that both Rust and the UI read. Neither side hard-codes encoders, options or explanations.

| File | Contents | Read by |
| --- | --- | --- |
| `glossary.en.json` | Every explained term: `term`, `short`, `eli5`, optional `pros`, `cons`, `pickIf` | UI tooltips and dropdown info panes; Rust tests |
| `encoders.json` | Containers, codecs, encoder families and their options, video and audio encoders, screenshot formats | `backsight-encode::catalog`, UI settings pages |
| `default-settings.json` | The full default settings object | `backsight-config` (`Settings::default`), UI preview mode |

### Integrity is tested, not hoped for

`cargo test` fails if:

- any `glossary` id in the catalog (encoders, options, choices, containers, formats) is missing from the glossary;
- any option's default isn't one of its allowed values or is out of range;
- an encoder points at an unknown codec or family, or a container lists an unknown codec or audio format;
- the default settings name an encoder, container, audio format or screenshot format that doesn't exist or doesn't fit together;
- the default hotkeys don't parse or conflict;
- a hardware encoder has no `requires` text (shown when the encoder probe finds it missing);
- the linked FFmpeg rejects any option name or choice an encoder offers, or a CPU encoder fails to open with any preset or tune.

Per-encoder `overrides` change a family option's `default` or `choices`, or drop it with `"omit": true` when that encoder has no such FFmpeg option.

### Adding an encoder option

1. Add it to the family in `content/encoders.json` with `id`, `label`, `glossary`, `kind`, `default`, `ffmpeg`, and `advanced` and `showIf` if needed.
2. Add its glossary entry (follow [brand-and-voice.md](../design/brand-and-voice.md)).
3. If the FFmpeg mapping isn't 1:1, handle it in `video_options` in `crates/backsight-encode/src/options.rs`. Units: `Kbps` values are multiplied by 1000.
4. `cargo test` checks the rest. The UI picks it up automatically.

## Settings file

- Location: `%APPDATA%\Backsight\config\settings.toml`. TOML because people do hand-edit it.
- **Atomic writes:** written to `settings.toml.tmp`, then renamed over the old file. A crash mid-save never corrupts it.
- **Never clobber:** if the file can't be parsed, loading returns an error, the UI shows it, and nothing is saved until the user chooses "Reset to defaults". A file from a newer Backsight (`version` higher than ours) is refused the same way.
- **Per-encoder values:** `video.encoder_options["nvenc-hevc"].bitrate = 40000`. Only changed values are stored; missing ones use the catalog default. Switching encoders and back keeps your tweaks.
- **Validation:** `backsight_encode::catalog::resolve` merges saved values over defaults and replaces anything unknown, of the wrong type or out of range with the default.
- **Schema changes:** bump `SCHEMA_VERSION` and migrate older files in `Settings::load_from`. Adding a field also means adding it to `default-settings.json`.

## The explanation UI

| Component | Behavior |
| --- | --- |
| `Term` | Dotted underline plus a ? icon. Hover or focus (Tab) shows a card after 150 ms; Esc closes it. Positioned with Floating UI so it never leaves the window. |
| `Row` | Label (`Term`) and the glossary's one-line `short` under it, so most people never need the tooltip. |
| `Select` | Custom listbox. Hovering or arrowing onto a choice shows its full explanation in a side pane before you pick it. Incompatible choices are shown disabled with the reason ("Can't hold AV1 video"). |
| `OptionList` | Renders any encoder's options from the catalog, honoring `advanced` (Simple and Advanced modes) and `showIf` (for example, bitrate only for CBR and VBR). |

Accessibility: every control is keyboard-reachable, the listbox uses `aria-activedescendant`, switches use `role="switch"`, and tooltips use `role="tooltip"` with `aria-describedby`.

## Localization (later)

Glossaries are per language (`glossary.<lang>.json`). Labels in `encoders.json` move into the glossary when the second language lands.
