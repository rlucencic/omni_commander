//! `session.toml`: what was open when the application last exited.

use serde::{Deserialize, Serialize};

use crate::SortColumn;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub active_pane: Side,
    pub show_hidden: bool,
    pub left: PaneSession,
    pub right: PaneSession,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            active_pane: Side::Left,
            show_hidden: true,
            left: PaneSession::default(),
            right: PaneSession::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneSession {
    pub active_tab: usize,
    pub tabs: Vec<TabSession>,
}

/// One tab. Paths are stored as text, so locations whose names are not
/// valid Unicode cannot be saved (the application skips those tabs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabSession {
    pub path: String,
    #[serde(default)]
    pub sort: SortColumn,
    #[serde(default)]
    pub sort_descending: bool,
}

impl TabSession {
    pub fn new(path: impl Into<String>, sort: SortColumn, sort_descending: bool) -> Self {
        Self {
            path: path.into(),
            sort,
            sort_descending,
        }
    }
}
