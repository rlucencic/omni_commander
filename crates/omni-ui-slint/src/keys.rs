//! Translation of Slint key events into toolkit-independent [`KeyChord`]s.

use omni_core::keymap::{Key, KeyChord, Modifiers};
use slint::platform::Key as SlintKey;

/// Slint encodes special keys as private-use characters in the event text.
const SPECIAL_KEYS: &[(SlintKey, Key)] = &[
    (SlintKey::UpArrow, Key::Up),
    (SlintKey::DownArrow, Key::Down),
    (SlintKey::LeftArrow, Key::Left),
    (SlintKey::RightArrow, Key::Right),
    (SlintKey::PageUp, Key::PageUp),
    (SlintKey::PageDown, Key::PageDown),
    (SlintKey::Home, Key::Home),
    (SlintKey::End, Key::End),
    (SlintKey::Return, Key::Enter),
    (SlintKey::Tab, Key::Tab),
    (SlintKey::Backspace, Key::Backspace),
    (SlintKey::Escape, Key::Escape),
    (SlintKey::Insert, Key::Insert),
    (SlintKey::Delete, Key::Delete),
    (SlintKey::Space, Key::Space),
    (SlintKey::F1, Key::F(1)),
    (SlintKey::F2, Key::F(2)),
    (SlintKey::F3, Key::F(3)),
    (SlintKey::F4, Key::F(4)),
    (SlintKey::F5, Key::F(5)),
    (SlintKey::F6, Key::F(6)),
    (SlintKey::F7, Key::F(7)),
    (SlintKey::F8, Key::F(8)),
    (SlintKey::F9, Key::F(9)),
    (SlintKey::F10, Key::F(10)),
    (SlintKey::F11, Key::F(11)),
    (SlintKey::F12, Key::F(12)),
];

/// Converts the `text` of a Slint `KeyEvent` plus its modifiers into a chord.
/// Returns `None` for text that is not a single key (e.g. IME composition)
/// or for keys we do not bind (bare modifiers, control characters).
pub fn chord_from_slint(text: &str, mut mods: Modifiers) -> Option<KeyChord> {
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let key = if c == char::from(SlintKey::Backtab) {
        mods.shift = true;
        Key::Tab
    } else if let Some(&(_, key)) = SPECIAL_KEYS.iter().find(|(k, _)| char::from(*k) == c) {
        key
    } else if !c.is_control() && !is_private_use(c) {
        Key::Char(c)
    } else {
        return None;
    };
    Some(KeyChord { key, mods }.normalized())
}

fn is_private_use(c: char) -> bool {
    ('\u{E000}'..='\u{F8FF}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: impl Into<String>) -> Option<Key> {
        chord_from_slint(&text.into(), Modifiers::default()).map(|c| c.key)
    }

    #[test]
    fn special_keys() {
        assert_eq!(key(char::from(SlintKey::UpArrow)), Some(Key::Up));
        assert_eq!(key(char::from(SlintKey::F5)), Some(Key::F(5)));
        assert_eq!(key("\t"), Some(Key::Tab));
        assert_eq!(key("\n"), Some(Key::Enter));
        assert_eq!(key(" "), Some(Key::Space));
    }

    #[test]
    fn backtab_is_shift_tab() {
        let chord = chord_from_slint(
            &char::from(SlintKey::Backtab).to_string(),
            Modifiers::default(),
        );
        assert_eq!(chord, Some(KeyChord::shift(Key::Tab)));
    }

    #[test]
    fn characters_are_lowercased_and_keep_modifiers() {
        let mods = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::default()
        };
        let chord = chord_from_slint("R", mods).unwrap();
        assert_eq!(chord.key, Key::Char('r'));
        assert_eq!(chord.mods, mods);
    }

    #[test]
    fn ignored_inputs() {
        assert_eq!(key(""), None);
        assert_eq!(key("ab"), None);
        assert_eq!(key(char::from(SlintKey::Shift)), None);
        assert_eq!(key(char::from(SlintKey::Menu)), None);
    }
}
