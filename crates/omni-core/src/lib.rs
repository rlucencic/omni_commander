//! UI-independent application logic for OmniCommander.
//!
//! This crate never depends on a GUI toolkit. It contains:
//!
//! * [`app`] — the application actor: commands in, updates out.
//! * [`pane`] — the state machine of a single file pane.
//! * [`selection`] — multi-selection of entries.
//! * [`filter`] — the quick filter.
//! * [`listing`] — immutable, shareable directory snapshots.
//! * [`location`] — typed paths and breadcrumbs.
//! * [`sort`] — entry ordering (natural, directories first).
//! * [`keymap`] — keyboard shortcuts as data.
//! * [`format`](mod@format) — human-readable sizes, dates, and attributes.

pub mod app;
pub mod filter;
pub mod format;
pub mod keymap;
pub mod listing;
pub mod location;
pub mod pane;
pub mod selection;
pub mod sort;

pub use app::{
    AppConfig, AppHandle, Command, PaneId, PaneSetup, SessionSnapshot, TabSetup, Update,
};
pub use listing::{Listing, Row, SnapshotId};
pub use sort::{SortKey, SortSpec};
