//! Turning settings, the saved session and the command line into the core's
//! configuration, and the core's snapshot back into a session to save.
//!
//! Pure functions, so the rules are unit-tested without a window.

use std::path::PathBuf;

use omni_config::{PaneSession, Session, Settings, Side, SortColumn, TabSession};
use omni_core::app::{AppConfig, PaneId, PaneSetup, SessionSnapshot, TabSetup};
use omni_core::{SortKey, SortSpec};

/// Folders given on the command line; they take precedence over the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLine {
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
}

/// Where panes open when neither the command line nor a session says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaults {
    pub left: PathBuf,
    pub right: PathBuf,
}

/// Builds the core configuration (minus the filesystem).
pub fn initial_panes(
    settings: &Settings,
    session: Option<&Session>,
    command_line: &CommandLine,
    defaults: &Defaults,
) -> (PaneSetup, PaneSetup, PaneId, bool) {
    let session = session.filter(|_| settings.panes.restore_session);
    let sort = sort_spec(settings.panes.sort, settings.panes.sort_descending);
    let pane = |cli: &Option<PathBuf>, saved: Option<&PaneSession>, default: &PathBuf| {
        if let Some(path) = cli {
            return PaneSetup::single(path.clone(), sort);
        }
        saved
            .and_then(restore_pane)
            .unwrap_or_else(|| PaneSetup::single(default.clone(), sort))
    };
    let left = pane(&command_line.left, session.map(|s| &s.left), &defaults.left);
    let right = pane(
        &command_line.right,
        session.map(|s| &s.right),
        &defaults.right,
    );
    let active = match session.map(|s| s.active_pane) {
        Some(Side::Right) => PaneId::Right,
        _ => PaneId::Left,
    };
    let show_hidden = session.is_none_or(|s| s.show_hidden);
    (left, right, active, show_hidden)
}

/// Convenience wrapper producing a complete [`AppConfig`].
pub fn app_config(
    fs: std::sync::Arc<dyn omni_vfs::FileSystem>,
    settings: &Settings,
    session: Option<&Session>,
    command_line: &CommandLine,
    defaults: &Defaults,
) -> AppConfig {
    let (left, right, active, show_hidden) =
        initial_panes(settings, session, command_line, defaults);
    AppConfig {
        fs,
        left,
        right,
        active,
        show_hidden,
    }
}

/// `None` if the saved pane has no tabs.
fn restore_pane(saved: &PaneSession) -> Option<PaneSetup> {
    let tabs: Vec<TabSetup> = saved
        .tabs
        .iter()
        .map(|tab| TabSetup {
            path: PathBuf::from(&tab.path),
            sort: sort_spec(tab.sort, tab.sort_descending),
        })
        .collect();
    if tabs.is_empty() {
        return None;
    }
    let active_tab = saved.active_tab.min(tabs.len() - 1);
    Some(PaneSetup { tabs, active_tab })
}

/// Converts what was open into a session file. Tabs whose path is not valid
/// Unicode cannot be stored in TOML and are skipped (and reported).
pub fn session_from(snapshot: &SessionSnapshot) -> (Session, Vec<String>) {
    let mut warnings = Vec::new();
    let mut pane = |setup: &PaneSetup| {
        let mut active_tab = 0;
        let mut tabs = Vec::new();
        for (index, tab) in setup.tabs.iter().enumerate() {
            let Some(path) = tab.path.to_str() else {
                warnings.push(format!(
                    "not saving the tab {}: its path is not valid Unicode",
                    tab.path.display()
                ));
                continue;
            };
            if index == setup.active_tab {
                active_tab = tabs.len();
            }
            let (sort, descending) = sort_column(tab.sort);
            tabs.push(TabSession::new(path, sort, descending));
        }
        PaneSession { active_tab, tabs }
    };
    let session = Session {
        active_pane: match snapshot.active {
            PaneId::Left => Side::Left,
            PaneId::Right => Side::Right,
        },
        show_hidden: snapshot.show_hidden,
        left: pane(&snapshot.left),
        right: pane(&snapshot.right),
    };
    (session, warnings)
}

fn sort_spec(column: SortColumn, descending: bool) -> SortSpec {
    let key = match column {
        SortColumn::Name => SortKey::Name,
        SortColumn::Extension => SortKey::Extension,
        SortColumn::Size => SortKey::Size,
        SortColumn::Modified => SortKey::Modified,
    };
    SortSpec { key, descending }
}

fn sort_column(sort: SortSpec) -> (SortColumn, bool) {
    let column = match sort.key {
        SortKey::Name => SortColumn::Name,
        SortKey::Extension => SortColumn::Extension,
        SortKey::Size => SortColumn::Size,
        SortKey::Modified => SortColumn::Modified,
    };
    (column, sort.descending)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Defaults {
        Defaults {
            left: PathBuf::from("/cwd"),
            right: PathBuf::from("/home/me"),
        }
    }

    fn saved_session() -> Session {
        Session {
            active_pane: Side::Right,
            show_hidden: false,
            left: PaneSession {
                active_tab: 1,
                tabs: vec![
                    TabSession::new("/a", SortColumn::Name, false),
                    TabSession::new("/b", SortColumn::Size, true),
                ],
            },
            right: PaneSession {
                active_tab: 7, // out of range: clamped
                tabs: vec![TabSession::new("/c", SortColumn::Modified, false)],
            },
        }
    }

    #[test]
    fn without_session_or_arguments_uses_defaults_and_settings() {
        let mut settings = Settings::default();
        settings.panes.sort = SortColumn::Size;
        let (left, right, active, hidden) =
            initial_panes(&settings, None, &CommandLine::default(), &defaults());
        assert_eq!(
            left,
            PaneSetup::single("/cwd".into(), sort_spec(SortColumn::Size, false))
        );
        assert_eq!(right.tabs[0].path, PathBuf::from("/home/me"));
        assert_eq!(active, PaneId::Left);
        assert!(hidden);
    }

    #[test]
    fn restores_the_session() {
        let session = saved_session();
        let (left, right, active, hidden) = initial_panes(
            &Settings::default(),
            Some(&session),
            &CommandLine::default(),
            &defaults(),
        );
        assert_eq!(left.tabs.len(), 2);
        assert_eq!(left.active_tab, 1);
        assert_eq!(left.tabs[1].sort, sort_spec(SortColumn::Size, true));
        assert_eq!(right.active_tab, 0);
        assert_eq!(active, PaneId::Right);
        assert!(!hidden);
    }

    #[test]
    fn command_line_wins_per_pane_and_restore_can_be_disabled() {
        let session = saved_session();
        let cli = CommandLine {
            left: Some("/given".into()),
            right: None,
        };
        let (left, right, _, _) =
            initial_panes(&Settings::default(), Some(&session), &cli, &defaults());
        assert_eq!(left.tabs.len(), 1);
        assert_eq!(left.tabs[0].path, PathBuf::from("/given"));
        assert_eq!(
            right.tabs[0].path,
            PathBuf::from("/c"),
            "the other pane is still restored"
        );

        let mut settings = Settings::default();
        settings.panes.restore_session = false;
        let (left, ..) = initial_panes(
            &settings,
            Some(&session),
            &CommandLine::default(),
            &defaults(),
        );
        assert_eq!(left.tabs[0].path, PathBuf::from("/cwd"));
    }

    #[test]
    fn session_round_trips_through_a_snapshot() {
        let session = saved_session();
        let (left, right, active, show_hidden) = initial_panes(
            &Settings::default(),
            Some(&session),
            &CommandLine::default(),
            &defaults(),
        );
        let snapshot = SessionSnapshot {
            left,
            right,
            active,
            show_hidden,
        };
        let (saved, warnings) = session_from(&snapshot);
        assert!(warnings.is_empty());
        let mut expected = session;
        expected.right.active_tab = 0;
        assert_eq!(saved, expected);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tabs_with_non_unicode_paths_are_skipped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = PathBuf::from(OsString::from_vec(b"/bad\xff".to_vec()));
        let snapshot = SessionSnapshot {
            left: PaneSetup {
                tabs: vec![
                    TabSetup {
                        path: bad,
                        sort: SortSpec::default(),
                    },
                    TabSetup {
                        path: "/ok".into(),
                        sort: SortSpec::default(),
                    },
                ],
                active_tab: 1,
            },
            right: PaneSetup::single("/r".into(), SortSpec::default()),
            active: PaneId::Left,
            show_hidden: true,
        };
        let (session, warnings) = session_from(&snapshot);
        assert_eq!(session.left.tabs.len(), 1);
        assert_eq!(session.left.active_tab, 0, "still points at /ok");
        assert_eq!(warnings.len(), 1);
    }
}
