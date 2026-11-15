//! `settings.toml`: user preferences.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

/// User preferences. Every field has a default, so any subset of the file
/// (including an empty file) is valid.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    pub appearance: Appearance,
    pub panes: PaneDefaults,
    /// Shortcut overrides: action name → shortcuts that replace the
    /// action's default ones (an empty list unbinds it).
    pub keys: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub theme: Theme,
    pub renderer: Renderer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the operating system.
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Renderer {
    /// GPU rendering with automatic software fallback.
    #[default]
    Auto,
    /// GPU rendering through wgpu (Vulkan, Metal or Direct3D).
    Gpu,
    /// GPU rendering through OpenGL.
    Opengl,
    /// CPU rendering.
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PaneDefaults {
    /// Reopen the previous session's tabs on start.
    pub restore_session: bool,
    /// Sort order of panes that do not come from the session.
    pub sort: SortColumn,
    pub sort_descending: bool,
}

impl Default for PaneDefaults {
    fn default() -> Self {
        Self {
            restore_session: true,
            sort: SortColumn::Name,
            sort_descending: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortColumn {
    #[default]
    Name,
    Extension,
    Size,
    Modified,
}

/// The file written on first run. `action_names` are listed as the actions
/// that `[keys]` accepts.
pub fn default_settings_file(action_names: &[&str]) -> String {
    let mut actions = String::new();
    let mut line = String::from("#  ");
    for (i, name) in action_names.iter().enumerate() {
        let item = if i + 1 < action_names.len() {
            format!(" {name},")
        } else {
            format!(" {name}")
        };
        if line.len() + item.len() > 78 {
            actions.push_str(line.trim_end());
            actions.push('\n');
            line = String::from("#  ");
        }
        line.push_str(&item);
    }
    actions.push_str(line.trim_end());
    DEFAULT_SETTINGS.replace("{ACTIONS}", &actions)
}

const DEFAULT_SETTINGS: &str = r#"# OmniCommander settings
#
# OmniCommander reads this file at startup and never rewrites it. Delete it to
# get a fresh copy listing every option with its default value.

[appearance]
# "system", "light" or "dark".
theme = "system"
# "auto": GPU (Vulkan / Metal / Direct3D) with automatic software fallback,
# "gpu", "opengl" or "software". The SLINT_BACKEND environment variable
# takes precedence.
renderer = "auto"

[panes]
# Reopen the tabs of the previous session.
restore_session = true
# Sort order for panes that are not restored from the session:
# "name", "extension", "size" or "modified".
sort = "name"
sort_descending = false

[keys]
# Override shortcuts by action name. A list replaces all default shortcuts
# of the action; an empty list removes them. Examples:
#   refresh = ["Ctrl+R", "F2"]
#   toggle_hidden = "Ctrl+H"
#   new_tab = []
# Modifiers: Ctrl, Alt, Shift, Meta (Ctrl is Cmd on macOS). Keys: letters,
# digits, F1-F24, Up, Down, Left, Right, PageUp, PageDown, Home, End, Enter,
# Tab, Backspace, Escape, Insert, Delete, Space, or a single character.
#
# Actions:
{ACTIONS}
"#;

/// Parses `settings.toml` section by section: a mistake in one section only
/// resets that section, with a warning.
pub(crate) fn parse(text: &str) -> (Settings, Vec<String>) {
    let mut warnings = Vec::new();
    let table: Table = match toml::from_str(text) {
        Ok(table) => table,
        Err(err) => {
            warnings.push(format!("ignoring the file: {err}"));
            return (Settings::default(), warnings);
        }
    };
    let mut settings = Settings::default();
    for (name, value) in table {
        match name.as_str() {
            "appearance" => settings.appearance = section(value, &name, &mut warnings),
            "panes" => settings.panes = section(value, &name, &mut warnings),
            "keys" => settings.keys = parse_keys(value, &mut warnings),
            other => warnings.push(format!("unknown section [{other}]")),
        }
    }
    (settings, warnings)
}

fn section<T: DeserializeOwned + Default>(
    value: Value,
    name: &str,
    warnings: &mut Vec<String>,
) -> T {
    value.try_into().unwrap_or_else(|err| {
        warnings.push(format!(
            "[{name}]: {err}; using the defaults for this section"
        ));
        T::default()
    })
}

/// `action = "Shortcut"` or `action = ["Shortcut", ...]`.
fn parse_keys(value: Value, warnings: &mut Vec<String>) -> BTreeMap<String, Vec<String>> {
    let Value::Table(table) = value else {
        warnings.push("[keys] must be a table".to_string());
        return BTreeMap::new();
    };
    let mut keys = BTreeMap::new();
    for (action, value) in table {
        let shortcuts = match value {
            Value::String(shortcut) => vec![shortcut],
            Value::Array(items) => {
                let strings: Option<Vec<String>> = items
                    .into_iter()
                    .map(|item| match item {
                        Value::String(s) => Some(s),
                        _ => None,
                    })
                    .collect();
                match strings {
                    Some(strings) => strings,
                    None => {
                        warnings.push(format!("[keys] {action}: shortcuts must be strings"));
                        continue;
                    }
                }
            }
            _ => {
                warnings.push(format!(
                    "[keys] {action}: expected a shortcut or a list of shortcuts"
                ));
                continue;
            }
        };
        keys.insert(action, shortcuts);
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_file_parses_to_the_defaults() {
        let text = default_settings_file(&["open", "refresh", "new_tab"]);
        let (settings, warnings) = parse(&text);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(settings, Settings::default());
        assert!(text.contains("#   open, refresh, new_tab"), "{text}");
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(parse(""), (Settings::default(), vec![]));
    }

    #[test]
    fn values_are_read() {
        let (settings, warnings) = parse(
            r#"
            [appearance]
            theme = "dark"
            renderer = "software"
            [panes]
            sort = "modified"
            sort_descending = true
            restore_session = false
            [keys]
            refresh = ["Ctrl+R", "F2"]
            new_tab = "Ctrl+N"
            close_tab = []
            "#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            settings.appearance,
            Appearance {
                theme: Theme::Dark,
                renderer: Renderer::Software
            }
        );
        assert_eq!(
            settings.panes,
            PaneDefaults {
                restore_session: false,
                sort: SortColumn::Modified,
                sort_descending: true
            }
        );
        assert_eq!(settings.keys["refresh"], ["Ctrl+R", "F2"]);
        assert_eq!(settings.keys["new_tab"], ["Ctrl+N"]);
        assert!(settings.keys["close_tab"].is_empty());
    }

    #[test]
    fn mistakes_only_reset_the_affected_section() {
        let (settings, warnings) = parse(
            r#"
            [appearance]
            theme = "purple"
            [panes]
            sort = "size"
            [keys]
            refresh = 5
            open = "Enter"
            [colours]
            x = 1
            "#,
        );
        assert_eq!(settings.appearance, Appearance::default());
        assert_eq!(
            settings.panes.sort,
            SortColumn::Size,
            "other sections still apply"
        );
        assert_eq!(settings.keys.len(), 1);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("[appearance]")));
        assert!(warnings.iter().any(|w| w.contains("refresh")));
        assert!(warnings.iter().any(|w| w.contains("[colours]")));
    }

    #[test]
    fn unknown_fields_are_reported() {
        let (_, warnings) = parse("[panes]\nshow_hiden = true\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("show_hiden"), "{warnings:?}");
    }

    #[test]
    fn syntax_errors_keep_all_defaults() {
        let (settings, warnings) = parse("[appearance\ntheme = 1");
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 1);
    }
}
