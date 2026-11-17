//! Hotkey strings such as `"Ctrl+Alt+F10"` or `"Mouse4"`.
//!
//! The canonical form is `Ctrl+Alt+Shift+Win+Key`. Key names match what the UI
//! produces from `KeyboardEvent.code` (see `app/src/lib/hotkeys.ts`).

use std::{collections::BTreeMap, fmt, str::FromStr};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Canonical key name, e.g. `F10`, `A`, `Numpad5`, `Mouse4`.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyError {
    #[error("a hotkey needs one key besides Ctrl, Alt, Shift or Win")]
    NoKey,
    #[error("a hotkey can only have one main key")]
    TooManyKeys,
    #[error("unknown key \"{0}\"")]
    UnknownKey(String),
}

const NAMED_KEYS: &[&str] = &[
    "Space",
    "Tab",
    "Enter",
    "Backspace",
    "Insert",
    "Delete",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
    "PrintScreen",
    "Pause",
    "ScrollLock",
    "NumLock",
    "NumpadAdd",
    "NumpadSubtract",
    "NumpadMultiply",
    "NumpadDivide",
    "NumpadDecimal",
    "NumpadEnter",
    "Minus",
    "Equal",
    "BracketLeft",
    "BracketRight",
    "Backslash",
    "Semicolon",
    "Quote",
    "Backquote",
    "Comma",
    "Period",
    "Slash",
    "Mouse3",
    "Mouse4",
    "Mouse5",
];

fn canonical_key(raw: &str) -> Option<String> {
    let upper = raw.to_ascii_uppercase();
    let single_alnum = raw.len() == 1 && raw.chars().all(|c| c.is_ascii_alphanumeric());
    let function_key =
        upper.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=24).contains(&n));
    let numpad_digit =
        upper.strip_prefix("NUMPAD").is_some_and(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()));

    if single_alnum || function_key {
        Some(upper)
    } else if numpad_digit {
        Some(format!("Numpad{}", &raw[6..]))
    } else {
        NAMED_KEYS.iter().find(|k| k.eq_ignore_ascii_case(raw)).map(|k| (*k).to_string())
    }
}

impl FromStr for Hotkey {
    type Err = HotkeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut hk = Hotkey { ctrl: false, alt: false, shift: false, win: false, key: String::new() };
        for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hk.ctrl = true,
                "alt" => hk.alt = true,
                "shift" => hk.shift = true,
                "win" | "meta" | "super" => hk.win = true,
                _ if !hk.key.is_empty() => return Err(HotkeyError::TooManyKeys),
                _ => hk.key = canonical_key(part).ok_or_else(|| HotkeyError::UnknownKey(part.into()))?,
            }
        }
        if hk.key.is_empty() { Err(HotkeyError::NoKey) } else { Ok(hk) }
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mods = [(self.ctrl, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+"), (self.win, "Win+")];
        for (on, name) in mods {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&self.key)
    }
}

/// Every pair of actions bound to the same hotkey.
pub fn conflicts<A: Copy + Ord>(bindings: &BTreeMap<A, Hotkey>) -> Vec<(A, A)> {
    let items: Vec<_> = bindings.iter().collect();
    let mut out = Vec::new();
    for (i, (a, hk_a)) in items.iter().enumerate() {
        for (b, hk_b) in &items[i + 1..] {
            if hk_a == hk_b {
                out.push((**a, **b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes() {
        let hk: Hotkey = " alt + shift+f10 ".parse().unwrap();
        assert_eq!(hk.to_string(), "Alt+Shift+F10");
        assert_eq!("control+numpad5".parse::<Hotkey>().unwrap().to_string(), "Ctrl+Numpad5");
        assert_eq!("mouse4".parse::<Hotkey>().unwrap().to_string(), "Mouse4");
        assert_eq!("Win+Alt+c".parse::<Hotkey>().unwrap().to_string(), "Alt+Win+C");
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!("Ctrl+Alt".parse::<Hotkey>(), Err(HotkeyError::NoKey));
        assert_eq!("".parse::<Hotkey>(), Err(HotkeyError::NoKey));
        assert_eq!("A+B".parse::<Hotkey>(), Err(HotkeyError::TooManyKeys));
        assert!(matches!("Alt+F25".parse::<Hotkey>(), Err(HotkeyError::UnknownKey(_))));
        assert!(matches!("Alt+Hyper".parse::<Hotkey>(), Err(HotkeyError::UnknownKey(_))));
    }

    #[test]
    fn finds_conflicts() {
        let map = BTreeMap::from([
            (1, "Alt+F10".parse().unwrap()),
            (2, "alt+f10".parse().unwrap()),
            (3, "Alt+F9".parse().unwrap()),
        ]);
        assert_eq!(conflicts(&map), vec![(1, 2)]);
    }
}
