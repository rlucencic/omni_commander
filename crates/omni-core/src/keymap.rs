//! Keyboard shortcuts, independent of any UI toolkit.
//!
//! Front ends translate their native key events into a [`KeyChord`] and ask
//! the [`Keymap`] which [`Action`] it triggers. Bindings are data: users
//! override them by action name with shortcut strings such as
//! `"Ctrl+Shift+Tab"` (see [`Keymap::apply_overrides`]).

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

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

    pub fn alt(key: Key) -> Self {
        Self {
            key,
            mods: Modifiers {
                alt: true,
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
    /// Move the cursor.
    Move(Motion),
    /// Move the cursor, selecting the range from where this run of
    /// selecting moves started.
    Select(Motion),
    /// Toggle the selection of the entry under the cursor; with `advance`,
    /// also move the cursor down.
    ToggleSelection {
        advance: bool,
    },
    SelectAll,
    DeselectAll,
    /// Open (or focus) the quick filter.
    Filter,
    /// Remove the quick filter.
    ClearFilter,
    /// Enter the directory under the cursor.
    Open,
    /// Go to the parent directory.
    GoUp,
    /// Go back in the active pane's history.
    GoBack,
    /// Go forward in the active pane's history.
    GoForward,
    /// Edit the active pane's location.
    EditLocation,
    /// Make the other pane active.
    SwitchPane,
    /// Open a new tab with the current location.
    NewTab,
    /// Open the directory under the cursor in a new tab.
    OpenInNewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    /// Re-read the active pane's directory.
    Refresh,
    /// Sort the active pane by a column (again: reverse direction).
    SortBy(SortKey),
    /// Show or hide hidden files in all panes.
    ToggleHidden,
}

impl Action {
    /// Every action with the name used in settings files.
    pub const ALL: &'static [(&'static str, Action)] = &[
        ("move_up", Action::Move(Motion::Up)),
        ("move_down", Action::Move(Motion::Down)),
        ("move_page_up", Action::Move(Motion::PageUp)),
        ("move_page_down", Action::Move(Motion::PageDown)),
        ("move_first", Action::Move(Motion::First)),
        ("move_last", Action::Move(Motion::Last)),
        ("select_up", Action::Select(Motion::Up)),
        ("select_down", Action::Select(Motion::Down)),
        ("select_page_up", Action::Select(Motion::PageUp)),
        ("select_page_down", Action::Select(Motion::PageDown)),
        ("select_to_first", Action::Select(Motion::First)),
        ("select_to_last", Action::Select(Motion::Last)),
        (
            "toggle_selection",
            Action::ToggleSelection { advance: false },
        ),
        (
            "toggle_selection_and_move_down",
            Action::ToggleSelection { advance: true },
        ),
        ("select_all", Action::SelectAll),
        ("deselect_all", Action::DeselectAll),
        ("filter", Action::Filter),
        ("clear_filter", Action::ClearFilter),
        ("open", Action::Open),
        ("go_up", Action::GoUp),
        ("go_back", Action::GoBack),
        ("go_forward", Action::GoForward),
        ("edit_location", Action::EditLocation),
        ("switch_pane", Action::SwitchPane),
        ("new_tab", Action::NewTab),
        ("open_in_new_tab", Action::OpenInNewTab),
        ("close_tab", Action::CloseTab),
        ("next_tab", Action::NextTab),
        ("previous_tab", Action::PreviousTab),
        ("refresh", Action::Refresh),
        ("sort_by_name", Action::SortBy(SortKey::Name)),
        ("sort_by_extension", Action::SortBy(SortKey::Extension)),
        ("sort_by_size", Action::SortBy(SortKey::Size)),
        ("sort_by_modified", Action::SortBy(SortKey::Modified)),
        ("toggle_hidden", Action::ToggleHidden),
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(n, _)| *n == name)
            .map(|&(_, action)| action)
    }

    pub fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, action)| *action == self)
            .map(|&(name, _)| name)
            .expect("every action is listed in Action::ALL")
    }
}

/// A cursor movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Motion {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
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
        let motions = [
            (Key::Up, Motion::Up),
            (Key::Down, Motion::Down),
            (Key::PageUp, Motion::PageUp),
            (Key::PageDown, Motion::PageDown),
            (Key::Home, Motion::First),
            (Key::End, Motion::Last),
        ];
        for (key, motion) in motions {
            map.bind(KeyChord::plain(key), Move(motion));
            map.bind(KeyChord::shift(key), Select(motion));
        }
        let plain = [
            (Key::Enter, Open),
            (Key::Backspace, GoUp),
            (Key::Tab, SwitchPane),
            (Key::Insert, ToggleSelection { advance: true }),
            (Key::Space, ToggleSelection { advance: false }),
            (Key::Escape, ClearFilter),
        ];
        for (key, action) in plain {
            map.bind(KeyChord::plain(key), action);
        }
        map.bind(KeyChord::ctrl(Key::Char('a')), SelectAll);
        map.bind(
            KeyChord {
                key: Key::Char('a'),
                mods: Modifiers {
                    ctrl: true,
                    shift: true,
                    ..Modifiers::default()
                },
            },
            DeselectAll,
        );
        map.bind(KeyChord::ctrl(Key::Char('f')), Filter);
        map.bind(KeyChord::ctrl(Key::Char('h')), ToggleHidden);
        map.bind(KeyChord::ctrl(Key::Char('t')), NewTab);
        map.bind(KeyChord::ctrl(Key::Up), OpenInNewTab);
        map.bind(KeyChord::ctrl(Key::Char('w')), CloseTab);
        map.bind(KeyChord::ctrl(Key::Tab), NextTab);
        map.bind(
            KeyChord {
                key: Key::Tab,
                mods: Modifiers {
                    ctrl: true,
                    shift: true,
                    ..Modifiers::default()
                },
            },
            PreviousTab,
        );
        map.bind(KeyChord::shift(Key::Tab), SwitchPane);
        map.bind(KeyChord::ctrl(Key::PageUp), GoUp);
        map.bind(KeyChord::ctrl(Key::Char('r')), Refresh);
        map.bind(KeyChord::alt(Key::Left), GoBack);
        map.bind(KeyChord::alt(Key::Right), GoForward);
        map.bind(KeyChord::ctrl(Key::Char('l')), EditLocation);
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

    /// Shortcuts bound to `action`, sorted by their text.
    pub fn chords_for(&self, action: Action) -> Vec<KeyChord> {
        let mut chords: Vec<KeyChord> = self
            .bindings
            .iter()
            .filter(|&(_, &a)| a == action)
            .map(|(&chord, _)| chord)
            .collect();
        chords.sort_by_key(|chord| chord.to_string());
        chords
    }

    /// Applies user overrides: for each `(action name, shortcuts)`, the
    /// shortcuts replace all of the action's current bindings (an empty list
    /// unbinds it). A shortcut taken from another action moves to this one.
    /// Unknown actions and unparsable shortcuts are skipped and reported.
    pub fn apply_overrides<'a>(
        &mut self,
        overrides: impl IntoIterator<Item = (&'a str, &'a [String])>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for (name, shortcuts) in overrides {
            let Some(action) = Action::from_name(name) else {
                warnings.push(format!("unknown action \"{name}\""));
                continue;
            };
            self.bindings.retain(|_, a| *a != action);
            for shortcut in shortcuts {
                match shortcut.parse::<KeyChord>() {
                    Ok(chord) => {
                        if let Some(previous) = self.lookup(chord).filter(|&p| p != action) {
                            tracing::info!(%chord, from = previous.name(), to = name, "shortcut reassigned");
                        }
                        self.bind(chord, action);
                    }
                    Err(err) => warnings.push(format!("{name}: {err}")),
                }
            }
        }
        warnings
    }
}

/// A shortcut string that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid shortcut \"{text}\": {reason}")]
pub struct ParseChordError {
    text: String,
    reason: &'static str,
}

/// Named keys accepted in shortcut strings (case-insensitive), with the
/// spelling used when displaying them first.
const KEY_NAMES: &[(&str, Key)] = &[
    ("Up", Key::Up),
    ("Down", Key::Down),
    ("Left", Key::Left),
    ("Right", Key::Right),
    ("PageUp", Key::PageUp),
    ("PgUp", Key::PageUp),
    ("PageDown", Key::PageDown),
    ("PgDn", Key::PageDown),
    ("Home", Key::Home),
    ("End", Key::End),
    ("Enter", Key::Enter),
    ("Return", Key::Enter),
    ("Tab", Key::Tab),
    ("Backspace", Key::Backspace),
    ("Escape", Key::Escape),
    ("Esc", Key::Escape),
    ("Insert", Key::Insert),
    ("Ins", Key::Insert),
    ("Delete", Key::Delete),
    ("Del", Key::Delete),
    ("Space", Key::Space),
    ("Plus", Key::Char('+')),
];

impl FromStr for KeyChord {
    type Err = ParseChordError;

    /// Parses `Ctrl+Shift+Tab`, `F5`, `Alt+Left`, `Ctrl+.` and the like.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let error = |reason| ParseChordError {
            text: text.to_string(),
            reason,
        };
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (key_part, modifier_parts) = parts.split_last().ok_or_else(|| error("empty"))?;
        let mut mods = Modifiers::default();
        for part in modifier_parts {
            let flag = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "cmd" | "command" => &mut mods.ctrl,
                "alt" | "option" => &mut mods.alt,
                "shift" => &mut mods.shift,
                "meta" | "super" | "win" => &mut mods.meta,
                "" => return Err(error("missing key name")),
                _ => return Err(error("unknown modifier")),
            };
            if *flag {
                return Err(error("modifier given twice"));
            }
            *flag = true;
        }
        let key = parse_key(key_part).ok_or_else(|| error("unknown key"))?;
        Ok(KeyChord { key, mods }.normalized())
    }
}

fn parse_key(name: &str) -> Option<Key> {
    if let Some(&(_, key)) = KEY_NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return Some(key);
    }
    if let Some(number) = name
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<u8>().ok())
    {
        return (1..=24).contains(&number).then_some(Key::F(number));
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_whitespace() && !c.is_control() => Some(Key::Char(c)),
        _ => None,
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mods = [
            (self.mods.ctrl, "Ctrl+"),
            (self.mods.alt, "Alt+"),
            (self.mods.shift, "Shift+"),
            (self.mods.meta, "Meta+"),
        ];
        for (on, name) in mods {
            if on {
                f.write_str(name)?;
            }
        }
        match self.key {
            Key::Char('+') => f.write_str("Plus"),
            Key::Char(c) => write!(f, "{}", c.to_uppercase()),
            Key::F(n) => write!(f, "F{n}"),
            key => {
                let name = KEY_NAMES
                    .iter()
                    .find(|&&(_, k)| k == key)
                    .map(|&(name, _)| name)
                    .unwrap_or("?");
                f.write_str(name)
            }
        }
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
    fn navigation_bindings() {
        let map = Keymap::commander_defaults();
        assert_eq!(map.lookup(KeyChord::alt(Key::Left)), Some(Action::GoBack));
        assert_eq!(
            map.lookup(KeyChord::alt(Key::Right)),
            Some(Action::GoForward)
        );
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::Char('L'))),
            Some(Action::EditLocation)
        );
        assert_eq!(map.lookup(KeyChord::plain(Key::Left)), None);
    }

    #[test]
    fn selection_bindings() {
        let map = Keymap::commander_defaults();
        assert_eq!(
            map.lookup(KeyChord::plain(Key::Down)),
            Some(Action::Move(Motion::Down))
        );
        assert_eq!(
            map.lookup(KeyChord::shift(Key::End)),
            Some(Action::Select(Motion::Last))
        );
        assert_eq!(
            map.lookup(KeyChord::plain(Key::Insert)),
            Some(Action::ToggleSelection { advance: true })
        );
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::Char('a'))),
            Some(Action::SelectAll)
        );
        let ctrl_shift_a = KeyChord {
            key: Key::Char('A'),
            mods: Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::default()
            },
        };
        assert_eq!(map.lookup(ctrl_shift_a), Some(Action::DeselectAll));
    }

    #[test]
    fn tab_bindings() {
        let map = Keymap::commander_defaults();
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::Char('t'))),
            Some(Action::NewTab)
        );
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::Char('w'))),
            Some(Action::CloseTab)
        );
        assert_eq!(map.lookup(KeyChord::ctrl(Key::Tab)), Some(Action::NextTab));
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::Up)),
            Some(Action::OpenInNewTab)
        );
        let ctrl_shift_tab = KeyChord {
            key: Key::Tab,
            mods: Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::default()
            },
        };
        assert_eq!(map.lookup(ctrl_shift_tab), Some(Action::PreviousTab));
        // Plain Tab still switches panes.
        assert_eq!(
            map.lookup(KeyChord::plain(Key::Tab)),
            Some(Action::SwitchPane)
        );
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

    #[test]
    fn every_action_has_a_unique_name() {
        let mut names: Vec<&str> = Action::ALL.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Action::ALL.len());
        for &(name, action) in Action::ALL {
            assert_eq!(Action::from_name(name), Some(action));
            assert_eq!(action.name(), name);
        }
        // Every default binding uses a nameable action.
        let defaults = Keymap::commander_defaults();
        for action in defaults.bindings.values() {
            let _ = action.name();
        }
    }

    #[test]
    fn parses_shortcuts() {
        let parse = |s: &str| s.parse::<KeyChord>();
        assert_eq!(parse("F5"), Ok(KeyChord::plain(Key::F(5))));
        assert_eq!(parse("ctrl+r"), Ok(KeyChord::ctrl(Key::Char('r'))));
        assert_eq!(parse("Alt + Left"), Ok(KeyChord::alt(Key::Left)));
        assert_eq!(parse("Ctrl+."), Ok(KeyChord::ctrl(Key::Char('.'))));
        assert_eq!(parse("Ctrl+Plus"), Ok(KeyChord::ctrl(Key::Char('+'))));
        assert_eq!(parse("PgDn"), Ok(KeyChord::plain(Key::PageDown)));
        assert_eq!(parse("Space"), Ok(KeyChord::plain(Key::Space)));
        let ctrl_shift_tab = parse("Ctrl+Shift+Tab").unwrap();
        assert!(ctrl_shift_tab.mods.ctrl && ctrl_shift_tab.mods.shift);
        for bad in ["", "Ctrl+", "Hyper+A", "Ctrl+Ctrl+A", "F25", "Banana"] {
            assert!(parse(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn shortcuts_display_and_parse_back() {
        for &chord in Keymap::commander_defaults().bindings.keys() {
            let text = chord.to_string();
            assert_eq!(text.parse::<KeyChord>(), Ok(chord), "{text}");
        }
        assert_eq!(KeyChord::ctrl(Key::F(6)).to_string(), "Ctrl+F6");
    }

    #[test]
    fn overrides_replace_and_reassign_bindings() {
        let mut map = Keymap::commander_defaults();
        let refresh = vec!["F2".to_string(), "Ctrl+Q".to_string()];
        let none: Vec<String> = Vec::new();
        let steal = vec!["Ctrl+R".to_string()]; // Refresh's old shortcut
        let bad = vec!["Ctrl+Nope".to_string()];
        let warnings = map.apply_overrides([
            ("refresh", refresh.as_slice()),
            ("close_tab", none.as_slice()),
            ("new_tab", steal.as_slice()),
            ("frobnicate", none.as_slice()),
            ("go_up", bad.as_slice()),
        ]);
        assert_eq!(
            map.lookup(KeyChord::plain(Key::F(2))),
            Some(Action::Refresh)
        );
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::Char('q'))),
            Some(Action::Refresh)
        );
        assert_eq!(
            map.lookup(KeyChord::ctrl(Key::Char('r'))),
            Some(Action::NewTab)
        );
        assert_eq!(map.lookup(KeyChord::ctrl(Key::Char('t'))), None, "replaced");
        assert_eq!(map.lookup(KeyChord::ctrl(Key::Char('w'))), None, "unbound");
        assert!(
            map.chords_for(Action::GoUp).is_empty(),
            "invalid shortcut leaves none"
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }
}
