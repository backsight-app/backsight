#![no_main]
//! Names Backsight doesn't choose: a game's window title or exe name turned into a file and folder
//! name, file names in the clips folder (recovery), and log text scrubbed for crash reports.
use backsight_engine::{crash, fuzz, game};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    if let Some(name) = game::clean_name(text) {
        assert!(!name.is_empty() && name.chars().count() <= 60, "{name:?}");
        assert!(!name.chars().any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c)), "{name:?}");
        assert!(!name.ends_with(['.', ' ']) && !name.starts_with(' '), "{name:?}");
        assert_eq!(game::clean_name(&name).as_deref(), Some(name.as_str()), "the same name again");
        let stem = format!("{name} 2026-11-19 12.00.00");
        fuzz::recovery_names(&format!("{stem}.mp4.recording.part"));
    }
    fuzz::recovery_names(text);

    let parts: Vec<&str> = text.splitn(5, '\0').collect();
    if let [profile, user, pc, folder, log] = parts[..] {
        let rules = crash::Rules::new(
            profile,
            user,
            &[pc.to_owned()],
            &[(folder.to_owned(), "<clips folder>")],
            vec!["mp4".into(), "mkv".into()],
            &|_| None,
        );
        let _ = crash::scrub(log, &rules);
    } else {
        let _ = crash::last_lines(text, 3);
    }
});
