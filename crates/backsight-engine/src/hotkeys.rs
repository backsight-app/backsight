//! Hotkey matching: Raw Input key events in, actions out.
//!
//! The Raw Input thread (`input/raw_input.rs`) turns each `WM_INPUT` into an
//! [`Input`] and feeds it here. Modifiers are tracked per side, either side satisfies a
//! binding, auto-repeat never fires twice, and releases are reported so push-to-talk works.
//! See `docs/architecture/hotkeys.md`.

use backsight_config::{Hotkey, conflicts};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    LCtrl,
    RCtrl,
    LAlt,
    RAlt,
    LShift,
    RShift,
    LWin,
    RWin,
}

impl Modifier {
    pub const ALL: [Modifier; 8] = [
        Modifier::LCtrl,
        Modifier::RCtrl,
        Modifier::LAlt,
        Modifier::RAlt,
        Modifier::LShift,
        Modifier::RShift,
        Modifier::LWin,
        Modifier::RWin,
    ];
}

/// One key or mouse button. Key names are the canonical names from `backsight_config::Hotkey`
/// (`F10`, `A`, `Mouse4`), which the Raw Input layer maps from scan codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Modifier(Modifier),
    Key(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event<A> {
    Pressed(A),
    /// The key that fired the action went up (push-to-talk ends).
    Released(A),
}

pub struct Matcher<A> {
    bindings: Vec<(Hotkey, A)>,
    /// Held modifiers, one bit per [`Modifier`].
    mods: u8,
    /// Non-modifier keys that are down, with the action each one fired.
    held: Vec<(&'static str, Option<A>)>,
    /// The action that, bound without modifiers, also fires while modifiers are held.
    loose: Option<A>,
}

impl<A: Copy + Ord> Matcher<A> {
    /// When two actions share a combo, the later one is ignored (the UI shows the conflict).
    pub fn new(bindings: &BTreeMap<A, Hotkey>) -> Self {
        let ignored: Vec<A> = conflicts(bindings).into_iter().map(|(_, later)| later).collect();
        let bindings = bindings.iter().filter(|(a, _)| !ignored.contains(a)).map(|(a, hk)| (hk.clone(), *a)).collect();
        Matcher { bindings, mods: 0, held: Vec::with_capacity(16), loose: None }
    }

    /// Lets `action`, when bound without modifiers, fire while some are held: push-to-talk while
    /// holding Shift to sprint. Other actions match exactly, so F12 alone isn't Ctrl+F12.
    pub fn with_loose(mut self, action: A) -> Self {
        self.loose = Some(action);
        self
    }

    pub fn input(&mut self, input: Input, down: bool) -> Option<Event<A>> {
        match input {
            Input::Modifier(m) => {
                let bit = 1 << m as u8;
                if down {
                    self.mods |= bit
                } else {
                    self.mods &= !bit
                }
                None
            }
            Input::Key(key) if down => {
                if self.held.iter().any(|(k, _)| *k == key) {
                    return None; // auto-repeat
                }
                // An exact combo wins over the loose action (see `with_loose`).
                let loose = |hk: &Hotkey, a: &A| {
                    self.loose == Some(*a) && hk.key == key && !(hk.ctrl || hk.alt || hk.shift || hk.win)
                };
                let action = (self.bindings.iter().find(|(hk, _)| self.matches(hk, key)))
                    .or_else(|| self.bindings.iter().find(|(hk, a)| loose(hk, a)))
                    .map(|(_, a)| *a);
                self.held.push((key, action));
                action.map(Event::Pressed)
            }
            Input::Key(key) => {
                let i = self.held.iter().position(|(k, _)| *k == key)?;
                self.held.swap_remove(i).1.map(Event::Released)
            }
        }
    }

    /// Forgets every held key, where key-ups may have been lost (`WM_INPUT_DEVICE_CHANGE`, the
    /// session locking: the "stuck Alt" after Win+L). Returns a release for each action still
    /// held, so push-to-talk never stays on.
    pub fn reset(&mut self) -> impl Iterator<Item = Event<A>> + '_ {
        self.mods = 0;
        self.held.drain(..).filter_map(|(_, action)| action.map(Event::Released))
    }

    /// Clears held modifiers that `is_down` says are up. Key-ups made on the secure desktop
    /// (Ctrl+Alt+Del, UAC prompts) never reach us, and no notification says it happened.
    pub fn forget_released_modifiers(&mut self, is_down: impl Fn(Modifier) -> bool) {
        for m in Modifier::ALL {
            if !is_down(m) {
                self.mods &= !(1 << m as u8);
            }
        }
    }

    /// Whether any key is held. A key released out of sight (in an elevated window) would
    /// otherwise stay held and swallow its next press as auto-repeat.
    pub fn holds_key(&self) -> bool {
        !self.held.is_empty()
    }

    /// Forgets held keys that `is_down` says are up, with a release for each action among them.
    /// Like modifiers, a key-up made on the secure desktop or in an elevated window never
    /// reaches us, and push-to-talk would stay on until the next press.
    pub fn forget_released_keys<'a>(
        &'a mut self,
        is_down: impl Fn(&str) -> bool + 'a,
    ) -> impl Iterator<Item = Event<A>> + 'a {
        self.held.extract_if(.., move |(k, _)| !is_down(k)).filter_map(|(_, a)| a.map(Event::Released))
    }

    fn matches(&self, hk: &Hotkey, key: &str) -> bool {
        let side = |l: Modifier, r: Modifier| self.mods & (1 << l as u8 | 1 << r as u8) != 0;
        hk.key == key
            && hk.ctrl == side(Modifier::LCtrl, Modifier::RCtrl)
            && hk.alt == side(Modifier::LAlt, Modifier::RAlt)
            && hk.shift == side(Modifier::LShift, Modifier::RShift)
            && hk.win == side(Modifier::LWin, Modifier::RWin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Input::{Key, Modifier as M};
    use Modifier::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Act {
        Save,
        Toggle,
        Talk,
        Duplicate,
        Shot,
    }

    fn matcher() -> Matcher<Act> {
        Matcher::new(&BTreeMap::from([
            (Act::Save, "Alt+F10".parse().unwrap()),
            (Act::Toggle, "Alt+Shift+F10".parse().unwrap()),
            (Act::Talk, "Mouse4".parse().unwrap()),
            (Act::Duplicate, "Alt+F10".parse().unwrap()),
            (Act::Shot, "F12".parse().unwrap()),
        ]))
        .with_loose(Act::Talk)
    }

    /// Feeds `(input, down)` pairs and collects the events.
    fn run(m: &mut Matcher<Act>, seq: &[(Input, bool)]) -> Vec<Event<Act>> {
        seq.iter().filter_map(|(i, d)| m.input(*i, *d)).collect()
    }

    #[test]
    fn either_side_of_a_modifier_works() {
        let mut m = matcher();
        let left = [(M(LAlt), true), (Key("F10"), true), (Key("F10"), false), (M(LAlt), false)];
        let right = [(M(RAlt), true), (Key("F10"), true), (Key("F10"), false), (M(RAlt), false)];
        assert_eq!(run(&mut m, &left), [Event::Pressed(Act::Save), Event::Released(Act::Save)]);
        assert_eq!(run(&mut m, &right), [Event::Pressed(Act::Save), Event::Released(Act::Save)]);
    }

    #[test]
    fn combos_must_match_exactly() {
        let mut m = matcher();
        let seq = [(M(LAlt), true), (M(RShift), true), (Key("F10"), true)];
        assert_eq!(run(&mut m, &seq), [Event::Pressed(Act::Toggle)], "not Alt+F10");
        let mut m = matcher();
        let seq = [(M(LCtrl), true), (M(LAlt), true), (Key("F10"), true)];
        assert_eq!(run(&mut m, &seq), [], "Ctrl+Alt+F10 is not Alt+F10");
        assert_eq!(run(&mut matcher(), &[(Key("F10"), true)]), []);
    }

    #[test]
    fn push_to_talk_works_while_shift_is_held() {
        let mut m = matcher();
        let seq = [(M(LShift), true), (Key("Mouse4"), true), (Key("Mouse4"), false), (M(LShift), false)];
        assert_eq!(run(&mut m, &seq), [Event::Pressed(Act::Talk), Event::Released(Act::Talk)]);
        let seq = [(M(LCtrl), true), (M(RAlt), true), (Key("Mouse4"), true)];
        assert_eq!(run(&mut m, &seq), [Event::Pressed(Act::Talk)], "any modifiers");
    }

    #[test]
    fn other_plain_bindings_match_exactly() {
        let mut m = matcher();
        assert_eq!(run(&mut m, &[(M(LCtrl), true), (Key("F12"), true), (Key("F12"), false)]), [], "Ctrl+F12");
        assert_eq!(run(&mut matcher(), &[(Key("F12"), true)]), [Event::Pressed(Act::Shot)]);
    }

    #[test]
    fn keys_released_out_of_sight_are_forgotten() {
        let mut m = matcher();
        run(&mut m, &[(Key("W"), true), (Key("Mouse4"), true)]);
        assert!(m.holds_key());
        // UAC prompt: Mouse4 went up on the secure desktop. W is still held.
        let released: Vec<_> = m.forget_released_keys(|k| k == "W").collect();
        assert_eq!(released, [Event::Released(Act::Talk)]);
        assert!(m.holds_key(), "W is still held");
        assert!(m.forget_released_keys(|_| false).next().is_none(), "W fired nothing");
        assert!(!m.holds_key());
        assert_eq!(run(&mut m, &[(Key("Mouse4"), false)]), [], "a late key-up is harmless");
        assert_eq!(run(&mut m, &[(Key("Mouse4"), true)]), [Event::Pressed(Act::Talk)], "the next press works");
        assert_eq!(m.forget_released_keys(|_| true).count(), 0, "held keys stay");
    }

    #[test]
    fn holding_a_key_fires_once() {
        let mut m = matcher();
        let seq = [(M(LAlt), true), (Key("F10"), true), (Key("F10"), true), (Key("F10"), true)];
        assert_eq!(run(&mut m, &seq), [Event::Pressed(Act::Save)]);
    }

    #[test]
    fn other_held_keys_do_not_block_a_hotkey() {
        let mut m = matcher();
        let seq = [(Key("W"), true), (M(LAlt), true), (Key("F10"), true)];
        assert_eq!(run(&mut m, &seq), [Event::Pressed(Act::Save)]);
    }

    #[test]
    fn push_to_talk_reports_press_and_release() {
        let mut m = matcher();
        assert_eq!(
            run(&mut m, &[(Key("Mouse4"), true), (Key("Mouse4"), false)]),
            [Event::Pressed(Act::Talk), Event::Released(Act::Talk)]
        );
    }

    #[test]
    fn release_reaches_the_action_even_after_the_modifier_goes_up() {
        let mut m = matcher();
        let seq = [(M(LAlt), true), (Key("F10"), true), (M(LAlt), false), (Key("F10"), false)];
        assert_eq!(run(&mut m, &seq), [Event::Pressed(Act::Save), Event::Released(Act::Save)]);
    }

    #[test]
    fn reset_clears_a_stuck_alt() {
        let mut m = matcher();
        run(&mut m, &[(M(LAlt), true)]); // Alt-up lost while the PC was locked
        assert_eq!(m.reset().count(), 0, "nothing was firing");
        assert_eq!(run(&mut m, &[(Key("F10"), true)]), []);
    }

    #[test]
    fn reset_releases_push_to_talk() {
        let mut m = matcher();
        assert_eq!(run(&mut m, &[(Key("Mouse4"), true)]), [Event::Pressed(Act::Talk)]);
        // A mouse reconnects while the button is held: its key-up may never come.
        assert_eq!(m.reset().collect::<Vec<_>>(), [Event::Released(Act::Talk)]);
        assert_eq!(run(&mut m, &[(Key("Mouse4"), false)]), [], "a late key-up is harmless");
    }

    #[test]
    fn modifiers_released_on_the_secure_desktop_are_forgotten() {
        let mut m = matcher();
        // Ctrl+Alt+Del: we see Ctrl and Alt go down, then the secure desktop swallows the key-ups.
        run(&mut m, &[(M(LCtrl), true), (M(LAlt), true)]);
        // Back on the desktop only Alt is really down.
        m.forget_released_modifiers(|k| k == LAlt);
        assert_eq!(run(&mut m, &[(Key("F10"), true)]), [Event::Pressed(Act::Save)], "Ctrl no longer counts");
    }

    #[test]
    fn a_conflicting_later_binding_is_ignored() {
        let mut m = matcher();
        let seq = [(M(LAlt), true), (Key("F10"), true)];
        assert_eq!(run(&mut m, &seq), [Event::Pressed(Act::Save)]);
        assert!(!m.bindings.iter().any(|(_, a)| *a == Act::Duplicate));
    }
}
