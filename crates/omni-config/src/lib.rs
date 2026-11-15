//! Settings and session persistence for OmniCommander.
//!
//! Two files live in the platform configuration directory (on Linux
//! `$XDG_CONFIG_HOME/omnicommander`, overridable with `OMNI_CONFIG_DIR`):
//!
//! * `settings.toml`: user preferences. Created with comments on first run
//!   and never written by the application afterwards, so the user's edits
//!   and comments survive. See [`Settings`].
//! * `session.toml`: state written by the application on exit (open tabs,
//!   sort order, ...) and restored on start. See [`Session`].
//!
//! Loading never fails: problems are returned as human-readable warnings and
//! the affected values fall back to their defaults.
//!
//! This crate deliberately knows nothing about the rest of the application;
//! it only defines the file formats.

mod session;
mod settings;

use std::io;
use std::path::{Path, PathBuf};

pub use session::{PaneSession, Session, Side, TabSession};
pub use settings::{
    Appearance, PaneDefaults, Renderer, Settings, SortColumn, Theme, default_settings_file,
};

/// Environment variable that overrides the configuration directory.
pub const CONFIG_DIR_ENV: &str = "OMNI_CONFIG_DIR";

const SETTINGS_FILE: &str = "settings.toml";
const SESSION_FILE: &str = "session.toml";

/// A loaded value plus anything worth telling the user about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<T> {
    pub value: T,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot encode the session: {0}")]
    Encode(#[from] toml::ser::Error),
}

/// Collapses whitespace (including the line breaks some parser errors
/// contain), so a warning fits on one line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The directory holding the configuration files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDir {
    dir: PathBuf,
}

impl ConfigDir {
    /// `$OMNI_CONFIG_DIR` if set, otherwise the platform's configuration
    /// directory for OmniCommander. `None` if neither can be determined.
    pub fn locate() -> Option<Self> {
        if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV) {
            return Some(Self::new(dir));
        }
        directories::ProjectDirs::from("", "", "OmniCommander")
            .map(|dirs| Self::new(dirs.config_dir()))
    }

    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn settings_path(&self) -> PathBuf {
        self.dir.join(SETTINGS_FILE)
    }

    pub fn session_path(&self) -> PathBuf {
        self.dir.join(SESSION_FILE)
    }

    /// Reads `settings.toml`. If it does not exist yet, writes the commented
    /// default file (listing `action_names` as the bindable actions) so the
    /// user can discover and edit the options.
    pub fn load_settings(&self, action_names: &[&str]) -> Loaded<Settings> {
        let path = self.settings_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (value, warnings) = settings::parse(&text);
                let warnings = warnings
                    .into_iter()
                    .map(|w| one_line(&format!("{SETTINGS_FILE}: {w}")))
                    .collect();
                Loaded { value, warnings }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let mut warnings = Vec::new();
                if let Err(err) = self.write_new(&path, &default_settings_file(action_names)) {
                    warnings.push(format!("cannot create default settings: {err}"));
                } else {
                    tracing::info!(path = %path.display(), "created default settings");
                }
                Loaded {
                    value: Settings::default(),
                    warnings,
                }
            }
            Err(err) => Loaded {
                value: Settings::default(),
                warnings: vec![one_line(&format!("{}: {err}", path.display()))],
            },
        }
    }

    /// Reads `session.toml`; `None` if there is no usable session.
    pub fn load_session(&self) -> Loaded<Option<Session>> {
        let path = self.session_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Session>(&text) {
                Ok(session) => Loaded {
                    value: Some(session),
                    warnings: Vec::new(),
                },
                Err(err) => Loaded {
                    value: None,
                    warnings: vec![one_line(&format!(
                        "{SESSION_FILE}: ignoring the saved session: {err}"
                    ))],
                },
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Loaded {
                value: None,
                warnings: Vec::new(),
            },
            Err(err) => Loaded {
                value: None,
                warnings: vec![one_line(&format!("{}: {err}", path.display()))],
            },
        }
    }

    /// Writes `session.toml` atomically: a crash mid-write leaves the
    /// previous session intact.
    pub fn save_session(&self, session: &Session) -> Result<(), ConfigError> {
        let text = format!(
            "# Written by OmniCommander when it exits; edits here are overwritten.\n\n{}",
            toml::to_string_pretty(session)?
        );
        let path = self.session_path();
        let tmp = self.dir.join(format!("{SESSION_FILE}.tmp"));
        let io_err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| ConfigError::Io { path, source }
        };
        std::fs::create_dir_all(&self.dir).map_err(io_err(&self.dir))?;
        std::fs::write(&tmp, text).map_err(io_err(&tmp))?;
        std::fs::rename(&tmp, &path).map_err(io_err(&path))
    }

    /// Creates `path` with `contents`, never replacing an existing file.
    fn write_new(&self, path: &Path, contents: &str) -> io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(&self.dir)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(contents.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_creates_commented_settings_once() {
        let dir = tempfile::tempdir().unwrap();
        let config = ConfigDir::new(dir.path().join("omni"));
        let loaded = config.load_settings(&["refresh", "new_tab"]);
        assert_eq!(loaded.value, Settings::default());
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let text = std::fs::read_to_string(config.settings_path()).unwrap();
        assert!(text.contains("refresh, new_tab"), "{text}");

        // The user's edits are kept: the file is not rewritten.
        std::fs::write(config.settings_path(), "[appearance]\ntheme = \"dark\"\n").unwrap();
        let loaded = config.load_settings(&[]);
        assert_eq!(loaded.value.appearance.theme, Theme::Dark);
    }

    #[test]
    fn session_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let config = ConfigDir::new(dir.path());
        assert_eq!(config.load_session().value, None);

        let session = Session {
            active_pane: Side::Right,
            show_hidden: false,
            left: PaneSession {
                active_tab: 1,
                tabs: vec![
                    TabSession::new("/tmp", SortColumn::Name, false),
                    TabSession::new("/home/me/Music", SortColumn::Modified, true),
                ],
            },
            right: PaneSession::default(),
        };
        config.save_session(&session).unwrap();
        assert!(!config.path().join("session.toml.tmp").exists());
        let loaded = config.load_session();
        assert_eq!(loaded.value, Some(session));
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn broken_session_is_ignored_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let config = ConfigDir::new(dir.path());
        std::fs::write(config.session_path(), "this is [not toml").unwrap();
        let loaded = config.load_session();
        assert_eq!(loaded.value, None);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("ignoring the saved session"));
        assert!(!loaded.warnings[0].contains('\n'), "{:?}", loaded.warnings);
    }
}
