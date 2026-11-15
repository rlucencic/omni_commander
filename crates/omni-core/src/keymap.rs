//! Keyboard shortcuts, independent of any UI toolkit.
//!
//! Front ends translate their native key events into a [`KeyChord`] and ask
//! the [`Keymap`] which [`Action`] it triggers. Keeping the bindings here (and
//! not in UI markup) is what will make them user-configurable.

use std::collections::HashMap;

use crate::sort::SortKey;

/// A key, independent of keyboard layout modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character, stored lower-case; Shift is a modifier.
    Char(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Tab,
    Backspace,
    Escape,
    Insert,
    Delete,
    Space,
    /// Function key F1..=F24.
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    /// Control; Command (⌘) on macOS.
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Windows key / Control on macOS.
    pub meta: bool,
}

/// A key plus the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub mods: Modifiers,
}

impl KeyChord {
    pub fn plain(key: Key) -> Self {
        Self {
            key,
            mods: Modifiers::default(),
        }
    }

    pub fn ctrl(key: Key) -> Self {
        Self {
            key,
            mods: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        }
    }

    pub fn shift(key: Key) -> Self {
        Self {
            key,
            mods: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        }
    }

    /// Normalizes a chord coming from a key event: letters are stored
    /// lower-case so `Shift+A` and `Shift+a` are the same binding.
    pub fn normalized(self) -> Self {
        let key = match self.key {
            Key::Char(' ') => Key::Space,
            Key::Char(c) => Key::Char(c.to_lowercase().next().unwrap_or(c)),
            other => other,
        };
        Self { key, ..self }
    }
}

/// Something the user can trigger from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    CursorUp,
    CursorDown,
    CursorPageUp,
    CursorPageDown,
    CursorFirst,
    CursorLast,
    /// Enter the directory under the cursor.
    Open,
    /// Go to the parent directory.
    GoUp,
    /// Make the other pane active.
    SwitchPane,
    /// Re-read the active pane's directory.
    Refresh,
    /// Sort the active pane by a column (again: reverse direction).
    SortBy(SortKey),
}

/// Maps key chords to actions.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<KeyChord, Action>,
}

impl Keymap {
    /// Bindings familiar to Total Commander / Double Commander users.
    pub fn commander_defaults() -> Self {
        use Action::*;
        let mut map = Self::default();
        let plain = [
            (Key::Up, CursorUp),
            (Key::Down, CursorDown),
            (Key::PageUp, CursorPageUp),
            (Key::PageDown, CursorPageDown),
            (Key::Home, CursorFirst),
            (Key::End, CursorLast),
            (Key::Enter, Open),
            (Key::Backspace, GoUp),
            (Key::Tab, SwitchPane),
        ];
        for (key, action) in plain {
            map.bind(KeyChord::plain(key), action);
        }
        map.bind(KeyChord::shift(Key::Tab), SwitchPane);
        map.bind(KeyChord::ctrl(Key::PageUp), GoUp);
        map.bind(KeyChord::ctrl(Key::Char('r')), Refresh);
        map.bind(KeyChord::ctrl(Key::F(3)), SortBy(SortKey::Name));
        map.bind(KeyChord::ctrl(Key::F(4)), SortBy(SortKey::Extension));
        map.bind(KeyChord::ctrl(Key::F(5)), SortBy(SortKey::Modified));
        map.bind(KeyChord::ctrl(Key::F(6)), SortBy(SortKey::Size));
        map
    }

    /// Binds `chord` to `action`, replacing any previous binding of `chord`.
    pub fn bind(&mut self, chord: KeyChord, action: Action) {
        self.bindings.insert(chord.normalized(), action);
    }

    pub fn lookup(&self, chord: KeyChord) -> Option<Action> {
        self.bindings.get(&chord.normalized()).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings() {
        let map = Keymap::commander_defaults();
        assert_eq!(map.lookup(KeyChord::plain(Key::Enter)), Some(Action::Open));
        assert_eq!(
            map.lookup(KeyChord::plain(Key::Tab)),
            Some(Action::SwitchPane)
        );
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::F(6))),
            Some(Action::SortBy(SortKey::Size))
        );
        assert_eq!(map.lookup(KeyChord::plain(Key::F(6))), None);
    }

    #[test]
    fn letters_ignore_case() {
        let map = Keymap::commander_defaults();
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::Char('R'))),
            Some(Action::Refresh)
        );
        assert_eq!(map.lookup(KeyChord::plain(Key::Char('r'))), None);
    }

    #[test]
    fn rebinding_replaces() {
        let mut map = Keymap::commander_defaults();
        map.bind(KeyChord::plain(Key::Enter), Action::Refresh);
        assert_eq!(
            map.lookup(KeyChord::plain(Key::Enter)),
            Some(Action::Refresh)
        );
    }
}
