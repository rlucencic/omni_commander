//! State of one file pane.
//!
//! [`PaneState`] is a synchronous state machine: it never performs I/O. Its
//! methods return [`Changes`] describing what the caller must do (start a
//! directory enumeration) and what changed (listing, status). The async
//! [`crate::app`] actor drives it; tests drive it directly.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use omni_vfs::{DirEvent, Entry, FileSystem, VfsError, child_path};

use crate::filter::Filter;
use crate::listing::{Listing, Row, SnapshotId};
use crate::location;
use crate::selection::{Selection, SelectionSummary};
use crate::sort::{SortKey, SortSpec};

/// Minimum time between progress updates while a directory is loading.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// A slow directory shows its first partial listing after this long...
const PARTIAL_DELAY: Duration = Duration::from_millis(400);
/// ...and refreshes the partial listing at most this often...
const PARTIAL_INTERVAL: Duration = Duration::from_secs(1);
/// ...and never spends more than about 1/(1 + this) of its time on partial
/// listings: each one sorts everything received so far, and that cost grows
/// with the directory (and is much higher in unoptimized builds).
const PARTIAL_COST_FACTOR: u32 = 10;
/// Locations remembered in each direction of the navigation history.
const HISTORY_LIMIT: usize = 100;
/// Selection changes touching more rows than this are reported as "all
/// rows changed" rather than row by row.
const MAX_CHANGED_ROWS: usize = 1024;

/// Identifies one directory enumeration of one pane. Events carrying an
/// outdated token are ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LoadToken(u64);

/// A directory enumeration the caller has to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRequest {
    pub token: LoadToken,
    pub path: PathBuf,
}

/// What a state transition changed.
#[derive(Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct Changes {
    /// Start this enumeration; any previous one for the pane is obsolete.
    pub load: Option<LoadRequest>,
    /// A new listing must be shown, with the cursor on this row.
    pub listing: Option<usize>,
    /// [`PaneState::status`] changed.
    pub status: bool,
    /// The selection changed (and the listing did not; a new listing always
    /// comes with the current selection).
    pub selection: Option<SelectionChange>,
}

/// Which rows' selection state may have changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionChange {
    Rows(Vec<usize>),
    All,
}

impl SelectionChange {
    fn rows(rows: impl ExactSizeIterator<Item = usize>) -> Self {
        if rows.len() > MAX_CHANGED_ROWS {
            Self::All
        } else {
            Self::Rows(rows.collect())
        }
    }
}

impl Changes {
    fn status() -> Self {
        Self {
            status: true,
            ..Self::default()
        }
    }
}

/// Severity of a status message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub level: Level,
    pub text: String,
}

/// Transient state shown next to the listing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneStatus {
    /// Set while a directory is loading.
    pub loading: Option<Loading>,
    pub message: Option<Message>,
    pub history: HistoryState,
    pub selection: SelectionSummary,
    /// Text of the active quick filter; empty if none.
    pub filter: String,
}

/// Whether the pane can go back or forward in its navigation history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryState {
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loading {
    pub path: PathBuf,
    /// Entries received so far.
    pub entries: usize,
}

/// Where the cursor should be: on the entry called `name` if it exists,
/// otherwise on `row` (clamped).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Focus {
    name: Option<OsString>,
    row: usize,
}

impl Focus {
    fn top() -> Self {
        Self::default()
    }

    fn resolve(&self, listing: &Listing) -> usize {
        self.name
            .as_deref()
            .and_then(|name| listing.find(name))
            .unwrap_or_else(|| self.row.min(listing.len().saturating_sub(1)))
    }
}

/// A location in the navigation history, with the cursor as the user left it.
#[derive(Debug, Clone)]
struct Visit {
    path: PathBuf,
    focus: Focus,
}

/// How a load moves through the history. Applied only once the load shows
/// its first listing, so failed navigations never enter the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryStep {
    /// Re-reading the same place.
    Stay,
    /// A new location: the current one goes onto the back stack.
    Push,
    /// This many steps back (several when Back is pressed again while the
    /// previous step is still loading).
    Back(usize),
    Forward(usize),
}

#[derive(Debug)]
struct Load {
    token: LoadToken,
    path: PathBuf,
    entries: Vec<Entry>,
    /// Cursor target for the first listing of `path`; taken once published,
    /// after which the user's own cursor movements win. `None` therefore
    /// also means "this load has arrived".
    focus: Option<Focus>,
    history: HistoryStep,
    /// For typed locations: if `path` is a file, show its directory with
    /// the file under the cursor instead of failing.
    open_parent_of_file: bool,
    next_progress: Instant,
    next_partial: Instant,
    /// Whether a partial listing of `path` has been shown.
    partial_shown: bool,
    warnings: Vec<String>,
}

/// A Shift-selection in progress: the rows between `anchor` and the cursor
/// are selected on top of what was selected before it started (`base`).
#[derive(Debug)]
struct RangeSelect {
    anchor: usize,
    base: Selection,
    /// Rows selected by the previous step of this range, inclusive.
    span: (usize, usize),
}

/// One file pane: a location on a filesystem, its listing, and loading state.
#[derive(Debug)]
pub struct PaneState {
    fs: Arc<dyn FileSystem>,
    location: PathBuf,
    /// Every entry of the location, sorted.
    full: Arc<Listing>,
    /// What is shown: `full`, or `full` narrowed by `filter`.
    listing: Arc<Listing>,
    filter: Option<Filter>,
    /// Indexed by entry index, so it applies to `full` and `listing` alike.
    selection: Arc<Selection>,
    range: Option<RangeSelect>,
    sort: SortSpec,
    focus: Focus,
    load: Option<Load>,
    message: Option<Message>,
    /// False until the first enumeration finished (successfully or not).
    settled: bool,
    next_token: u64,
    back: Vec<Visit>,
    forward: Vec<Visit>,
}

impl PaneState {
    /// Creates a pane and requests the initial enumeration of `path`.
    pub fn new(fs: Arc<dyn FileSystem>, path: PathBuf, sort: SortSpec) -> (Self, Changes) {
        let has_parent = path.parent().is_some();
        let empty = Arc::new(Listing::new(Vec::new(), sort, has_parent));
        let mut pane = Self {
            fs,
            location: path.clone(),
            full: Arc::clone(&empty),
            listing: empty,
            filter: None,
            selection: Arc::new(Selection::new(0)),
            range: None,
            sort,
            focus: Focus::top(),
            load: None,
            message: None,
            settled: false,
            next_token: 0,
            back: Vec::new(),
            forward: Vec::new(),
        };
        let changes = pane.navigate(path);
        (pane, changes)
    }

    /// A new pane at the same location (e.g. for a new tab). It shares the
    /// current listing, so it can be shown at once, and requests a refresh
    /// to bring it up to date. History, selection and filter start empty.
    pub fn duplicate(&self) -> (Self, Changes) {
        let mut pane = Self {
            fs: Arc::clone(&self.fs),
            location: self.location.clone(),
            full: Arc::clone(&self.full),
            listing: Arc::clone(&self.full),
            filter: None,
            selection: Arc::new(Selection::new(self.full.entries().len())),
            range: None,
            sort: self.sort,
            focus: self.focus.clone(),
            load: None,
            message: None,
            settled: self.settled,
            next_token: 0,
            back: Vec::new(),
            forward: Vec::new(),
        };
        let changes = pane.refresh();
        (pane, changes)
    }

    pub fn fs(&self) -> &Arc<dyn FileSystem> {
        &self.fs
    }

    /// The directory whose listing is shown.
    pub fn location(&self) -> &Path {
        &self.location
    }

    pub fn listing(&self) -> &Arc<Listing> {
        &self.listing
    }

    pub fn sort(&self) -> SortSpec {
        self.sort
    }

    /// The row the cursor is on in [`Self::listing`].
    pub fn cursor(&self) -> usize {
        self.focus.resolve(&self.listing)
    }

    /// Short name for tabs: the last path component (or the root itself).
    pub fn title(&self) -> String {
        match self.location.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => self.location.display().to_string(),
        }
    }

    /// The directory that row `row` of listing `snapshot` leads to: a
    /// directory entry, or the parent for `..`. `None` for files and for
    /// outdated snapshots.
    pub fn directory_at(&self, snapshot: SnapshotId, row: usize) -> Option<PathBuf> {
        if snapshot != self.listing.id() {
            return None;
        }
        match self.listing.row(row)? {
            Row::Parent => self.location.parent().map(Path::to_path_buf),
            Row::Entry(entry) if entry.is_dir_like() => {
                child_path(&self.location, &entry.name).ok()
            }
            Row::Entry(_) => None,
        }
    }

    /// Selected entries, by entry index of [`Self::listing`].
    pub fn selection(&self) -> &Arc<Selection> {
        &self.selection
    }

    pub fn status(&self) -> PaneStatus {
        PaneStatus {
            loading: self.load.as_ref().map(|load| Loading {
                path: load.path.clone(),
                entries: load.entries.len(),
            }),
            message: self.message.clone(),
            history: HistoryState {
                can_go_back: !self.back.is_empty(),
                can_go_forward: !self.forward.is_empty(),
            },
            selection: SelectionSummary::of(&self.selection, self.full.entries()),
            filter: self
                .filter
                .as_ref()
                .map(|f| f.text().to_string())
                .unwrap_or_default(),
        }
    }

    /// Goes to `path` with the cursor on the first row.
    pub fn navigate(&mut self, path: PathBuf) -> Changes {
        self.start_load(path, Focus::top(), HistoryStep::Push)
    }

    /// Goes to a location typed by the user (see
    /// [`location::resolve_input`]). If it names a file, its directory is
    /// shown with the file under the cursor.
    pub fn navigate_to_input(&mut self, input: &str, home: Option<&Path>) -> Changes {
        let Some(path) = location::resolve_input(input, &self.location, home) else {
            return Changes::default();
        };
        let changes = self.start_load(path, Focus::top(), HistoryStep::Push);
        if let Some(load) = self.load.as_mut() {
            load.open_parent_of_file = true;
        }
        changes
    }

    /// Goes back in the history, restoring the cursor as it was left there.
    pub fn go_back(&mut self) -> Changes {
        let depth = match self.load.as_ref().map(|load| load.history) {
            Some(HistoryStep::Back(depth)) => depth + 1,
            _ => 1,
        };
        let Some(visit) = nth_from_end(&self.back, depth).cloned() else {
            return Changes::default();
        };
        self.start_load(visit.path, visit.focus, HistoryStep::Back(depth))
    }

    /// Goes forward in the history (undoes [`Self::go_back`]).
    pub fn go_forward(&mut self) -> Changes {
        let depth = match self.load.as_ref().map(|load| load.history) {
            Some(HistoryStep::Forward(depth)) => depth + 1,
            _ => 1,
        };
        let Some(visit) = nth_from_end(&self.forward, depth).cloned() else {
            return Changes::default();
        };
        self.start_load(visit.path, visit.focus, HistoryStep::Forward(depth))
    }

    /// Re-reads the current directory, keeping the cursor on the same entry.
    pub fn refresh(&mut self) -> Changes {
        self.start_load(self.location.clone(), self.focus.clone(), HistoryStep::Stay)
    }

    /// Goes to the parent directory with the cursor on the directory we left.
    pub fn go_up(&mut self) -> Changes {
        let Some(parent) = self.location.parent() else {
            return Changes::default();
        };
        let focus = Focus {
            name: self.location.file_name().map(OsString::from),
            row: 0,
        };
        self.start_load(parent.to_path_buf(), focus, HistoryStep::Push)
    }

    /// Opens the row `row` of listing `snapshot`: enters directories, goes up
    /// for `..`. Requests for an outdated snapshot are ignored.
    pub fn open(&mut self, snapshot: SnapshotId, row: usize) -> Changes {
        if snapshot != self.listing.id() {
            tracing::debug!(?snapshot, row, "ignoring open on an outdated listing");
            return Changes::default();
        }
        let target = match self.listing.row(row) {
            None => return Changes::default(),
            Some(Row::Parent) => return self.go_up(),
            Some(Row::Entry(entry)) => {
                if !entry.is_dir_like() {
                    let name = entry.name.to_string_lossy().into_owned();
                    return self.set_message(
                        Level::Info,
                        format!("Opening files is not implemented yet ({name})"),
                    );
                }
                child_path(&self.location, &entry.name)
            }
        };
        match target {
            Ok(path) => self.navigate(path),
            Err(err) => self.set_message(Level::Error, err.to_string()),
        }
    }

    /// Records that the user moved the cursor to `row` of `snapshot`.
    pub fn cursor_moved(&mut self, snapshot: SnapshotId, row: usize) {
        if snapshot != self.listing.id() {
            return;
        }
        let name = match self.listing.row(row) {
            Some(Row::Entry(entry)) => Some(entry.name.clone()),
            Some(Row::Parent) | None => None,
        };
        self.focus = Focus { name, row };
    }

    /// Sorts by `key` (reversing the direction if it is already the key).
    pub fn sort_by(&mut self, key: SortKey) -> Changes {
        self.sort = self.sort.toggled(key);
        self.full = Arc::new(self.full.resorted(self.sort));
        self.listing = self.apply_filter();
        self.range = None;
        Changes {
            listing: Some(self.place_cursor()),
            ..Changes::default()
        }
    }

    /// Shows only entries matching `text` (see [`Filter`]); blank text shows
    /// everything. Entries hidden by the filter are deselected, so actions on
    /// the selection never touch files the user cannot see.
    pub fn set_filter(&mut self, text: &str) -> Changes {
        let filter = Filter::new(text);
        if filter == self.filter {
            return Changes::default();
        }
        self.filter = filter;
        self.listing = self.apply_filter();
        self.deselect_hidden();
        self.range = None;
        // Stay on the current entry if it is still shown, otherwise go to
        // the first match (not `..`).
        let focused_visible = self
            .focus
            .name
            .as_deref()
            .is_some_and(|name| self.listing.find(name).is_some());
        if !focused_visible {
            self.focus = Focus {
                name: None,
                row: self.listing.first_entry_row(),
            };
        }
        Changes {
            listing: Some(self.place_cursor()),
            status: true,
            ..Changes::default()
        }
    }

    /// Toggles the selection of the entry in `row` (never the `..` row).
    pub fn toggle_selection(&mut self, snapshot: SnapshotId, row: usize) -> Changes {
        if snapshot != self.listing.id() {
            return Changes::default();
        }
        let Some(index) = self.listing.entry_index(row) else {
            return Changes::default();
        };
        Arc::make_mut(&mut self.selection).toggle(index);
        self.range = None;
        selection_changes(SelectionChange::Rows(vec![row]))
    }

    /// Shift-selection: selects the rows between `anchor` and `row` on top of
    /// the selection that existed when this range started. Repeated calls
    /// with the same anchor grow or shrink the range.
    pub fn select_range(&mut self, snapshot: SnapshotId, anchor: usize, row: usize) -> Changes {
        if snapshot != self.listing.id() || self.listing.is_empty() {
            return Changes::default();
        }
        let last = self.listing.len() - 1;
        let (lo, hi) = (anchor.min(row).min(last), anchor.max(row).min(last));
        let (base, previous) = match self.range.take() {
            Some(range) if range.anchor == anchor => (range.base, Some(range.span)),
            _ => ((*self.selection).clone(), None),
        };
        let mut selection = base.clone();
        for r in lo..=hi {
            if let Some(index) = self.listing.entry_index(r) {
                selection.set(index, true);
            }
        }
        let (from, to) = previous.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi)));
        self.selection = Arc::new(selection);
        self.range = Some(RangeSelect {
            anchor,
            base,
            span: (lo, hi),
        });
        selection_changes(SelectionChange::rows(from..to + 1))
    }

    /// Selects every shown entry.
    pub fn select_all(&mut self) -> Changes {
        let mut selection = Selection::new(self.full.entries().len());
        for &index in self.listing.visible() {
            selection.set(index, true);
        }
        self.range = None;
        if selection == *self.selection {
            return Changes::default();
        }
        self.selection = Arc::new(selection);
        selection_changes(SelectionChange::All)
    }

    pub fn deselect_all(&mut self) -> Changes {
        self.range = None;
        if self.selection.is_empty() {
            return Changes::default();
        }
        Arc::make_mut(&mut self.selection).clear();
        selection_changes(SelectionChange::All)
    }

    /// Feeds one event of the enumeration identified by `token`.
    pub fn on_dir_event(&mut self, token: LoadToken, event: DirEvent, now: Instant) -> Changes {
        let Some(load) = self.load.as_mut().filter(|load| load.token == token) else {
            tracing::trace!(?token, "ignoring event of an outdated load");
            return Changes::default();
        };
        match event {
            DirEvent::Entries(batch) => {
                load.entries.extend(batch);
                if now >= load.next_partial {
                    return self.show_partial(now);
                }
                if now >= load.next_progress {
                    load.next_progress = now + PROGRESS_INTERVAL;
                    return Changes::status();
                }
                Changes::default()
            }
            DirEvent::Warning(err) => {
                tracing::warn!(%err, "directory listing warning");
                load.warnings.push(err.to_string());
                Changes::default()
            }
            DirEvent::Done => {
                let load = self.load.take().expect("load checked above");
                self.settled = true;
                self.message = summarize_warnings(&load.warnings);
                self.show(load.path, load.focus, load.history, load.entries)
            }
            DirEvent::Failed(err) => {
                let load = self.load.take().expect("load checked above");
                let arrived = load.focus.is_none();
                if !arrived
                    && load.open_parent_of_file
                    && matches!(err, VfsError::NotADirectory { .. })
                    && let (Some(parent), Some(name)) = (load.path.parent(), load.path.file_name())
                {
                    let focus = Focus {
                        name: Some(name.to_os_string()),
                        row: 0,
                    };
                    return self.start_load(parent.to_path_buf(), focus, load.history);
                }
                tracing::info!(%err, "cannot list directory");
                if !arrived {
                    // A history entry that cannot be shown would block the
                    // way back; drop it.
                    match load.history {
                        HistoryStep::Back(depth) => remove_nth_from_end(&mut self.back, depth),
                        HistoryStep::Forward(depth) => {
                            remove_nth_from_end(&mut self.forward, depth)
                        }
                        HistoryStep::Stay | HistoryStep::Push => {}
                    }
                }
                let mut changes = Changes::status();
                if !self.settled && !load.partial_shown {
                    // Nothing was ever shown: show the failed location empty so
                    // the user can still navigate up from it.
                    let has_parent = load.path.parent().is_some();
                    self.replace_listing(
                        load.path,
                        Listing::new(Vec::new(), self.sort, has_parent),
                    );
                    changes.listing = Some(0);
                }
                self.settled = true;
                let text = if load.partial_shown {
                    format!("Listing incomplete: {err}")
                } else {
                    err.to_string()
                };
                self.message = Some(Message {
                    level: Level::Error,
                    text,
                });
                changes
            }
        }
    }

    fn start_load(&mut self, path: PathBuf, focus: Focus, history: HistoryStep) -> Changes {
        self.next_token += 1;
        let token = LoadToken(self.next_token);
        self.load = Some(Load {
            token,
            path: path.clone(),
            entries: Vec::new(),
            focus: Some(focus),
            history,
            open_parent_of_file: false,
            next_progress: Instant::now(),
            next_partial: Instant::now() + PARTIAL_DELAY,
            partial_shown: false,
            warnings: Vec::new(),
        });
        Changes {
            load: Some(LoadRequest { token, path }),
            status: true,
            ..Changes::default()
        }
    }

    /// Shows what the current load has received so far, then schedules the
    /// next partial listing based on how long this one took.
    fn show_partial(&mut self, now: Instant) -> Changes {
        let work = Instant::now();
        let load = self.load.as_mut().expect("partial listings require a load");
        load.partial_shown = true;
        let (path, focus, entries) = (load.path.clone(), load.focus.take(), load.entries.clone());
        let history = load.history;
        let changes = self.show(path, focus, history, entries);

        let cost = work.elapsed();
        let load = self.load.as_mut().expect("show keeps the load");
        let done = now + cost;
        load.next_partial = done + PARTIAL_INTERVAL.max(cost * PARTIAL_COST_FACTOR);
        load.next_progress = done + PROGRESS_INTERVAL;
        if cost > Duration::from_millis(100) {
            tracing::debug!(?cost, entries = load.entries.len(), "slow partial listing");
        }
        changes
    }

    /// Shows `entries` as the listing of `path`. `focus` is set for a load's
    /// first listing: the load arrives, its history step is applied, and the
    /// cursor goes to `focus`. Later listings keep the user's cursor.
    fn show(
        &mut self,
        path: PathBuf,
        focus: Option<Focus>,
        history: HistoryStep,
        entries: Vec<Entry>,
    ) -> Changes {
        if let Some(focus) = focus {
            self.record_history(&path, history);
            self.focus = focus;
        }
        let has_parent = path.parent().is_some();
        self.replace_listing(path, Listing::new(entries, self.sort, has_parent));
        Changes {
            listing: Some(self.place_cursor()),
            status: true,
            ..Changes::default()
        }
    }

    /// Installs a new listing of `path`. For the same location (a refresh)
    /// the selection follows entry names and the filter stays; a different
    /// location starts unfiltered with nothing selected.
    fn replace_listing(&mut self, path: PathBuf, full: Listing) {
        let selection = if path == self.location {
            remap_selection(&self.full, &self.selection, &full)
        } else {
            self.filter = None;
            Selection::new(full.entries().len())
        };
        self.location = path;
        self.full = Arc::new(full);
        self.listing = self.apply_filter();
        self.selection = Arc::new(selection);
        self.deselect_hidden();
        self.range = None;
    }

    fn apply_filter(&self) -> Arc<Listing> {
        match &self.filter {
            Some(filter) => Arc::new(self.full.filtered(filter)),
            None => Arc::clone(&self.full),
        }
    }

    /// Deselects entries the filter hides.
    fn deselect_hidden(&mut self) {
        if self.filter.is_none() || self.selection.is_empty() {
            return;
        }
        let mut visible = Selection::new(self.full.entries().len());
        for &index in self.listing.visible() {
            visible.set(index, true);
        }
        let hidden: Vec<u32> = self
            .selection
            .iter()
            .filter(|&i| !visible.contains(i))
            .collect();
        if !hidden.is_empty() {
            let selection = Arc::make_mut(&mut self.selection);
            for index in hidden {
                selection.set(index, false);
            }
        }
    }

    /// Resolves the focus against the current listing and remembers the
    /// result, so the core agrees with the cursor position it sends out.
    fn place_cursor(&mut self) -> usize {
        let row = self.focus.resolve(&self.listing);
        self.cursor_moved(self.listing.id(), row);
        row
    }

    /// Updates the history for arriving at `destination`. Uses the location
    /// and cursor being left, so call before replacing them.
    fn record_history(&mut self, destination: &Path, step: HistoryStep) {
        let here = Visit {
            path: self.location.clone(),
            focus: self.focus.clone(),
        };
        match step {
            HistoryStep::Stay => {}
            // Nothing was shown before the first load, so there is nowhere
            // to go back to.
            HistoryStep::Push if !self.settled || destination == self.location => {}
            HistoryStep::Push => {
                push_limited(&mut self.back, here);
                self.forward.clear();
            }
            HistoryStep::Back(depth) => {
                step_through(&mut self.back, &mut self.forward, here, depth)
            }
            HistoryStep::Forward(depth) => {
                step_through(&mut self.forward, &mut self.back, here, depth)
            }
        }
    }

    fn set_message(&mut self, level: Level, text: String) -> Changes {
        self.message = Some(Message { level, text });
        Changes::status()
    }
}

fn selection_changes(change: SelectionChange) -> Changes {
    Changes {
        selection: Some(change),
        status: true,
        ..Changes::default()
    }
}

/// Carries a selection over to a new listing of the same directory, by name.
fn remap_selection(old: &Listing, selection: &Selection, new: &Listing) -> Selection {
    let mut remapped = Selection::new(new.entries().len());
    if selection.is_empty() {
        return remapped;
    }
    let names: HashSet<&OsStr> = selection
        .iter()
        .filter_map(|i| old.entries().get(i as usize))
        .map(|entry| entry.name.as_os_str())
        .collect();
    for (i, entry) in new.entries().iter().enumerate() {
        if names.contains(entry.name.as_os_str()) {
            remapped.set(i as u32, true);
        }
    }
    remapped
}

/// Moves `depth` steps from the `from` stack to the `to` stack: the location
/// being left and any skipped ones go onto `to`, the destination leaves
/// `from`.
fn step_through(from: &mut Vec<Visit>, to: &mut Vec<Visit>, here: Visit, depth: usize) {
    push_limited(to, here);
    for _ in 1..depth {
        if let Some(skipped) = from.pop() {
            push_limited(to, skipped);
        }
    }
    from.pop();
}

fn push_limited(stack: &mut Vec<Visit>, visit: Visit) {
    if stack.len() == HISTORY_LIMIT {
        stack.remove(0);
    }
    stack.push(visit);
}

/// The `n`-th visit from the top of a history stack (1 = top).
fn nth_from_end(stack: &[Visit], n: usize) -> Option<&Visit> {
    stack.len().checked_sub(n).map(|i| &stack[i])
}

fn remove_nth_from_end(stack: &mut Vec<Visit>, n: usize) {
    if let Some(i) = stack.len().checked_sub(n) {
        stack.remove(i);
    }
}

fn summarize_warnings(warnings: &[String]) -> Option<Message> {
    let first = warnings.first()?;
    let text = match warnings.len() {
        1 => first.clone(),
        n => format!("{first} (and {} more warnings)", n - 1),
    };
    Some(Message {
        level: Level::Warning,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_vfs::{Capabilities, DirStream, EntryKind, VfsError};

    /// The pane never calls the filesystem itself.
    #[derive(Debug)]
    struct UnusedFs;

    impl FileSystem for UnusedFs {
        fn name(&self) -> &str {
            "unused"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn read_dir(&self, _path: &Path) -> DirStream {
            unreachable!("PaneState must not perform I/O")
        }
    }

    fn file(name: &str) -> Entry {
        Entry::new(name, EntryKind::File)
    }

    fn dir(name: &str) -> Entry {
        Entry::new(name, EntryKind::Directory)
    }

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\")
        } else {
            PathBuf::from("/")
        }
    }

    /// A pane at `<root>/home/user` that finished loading `entries`.
    fn loaded_pane(entries: Vec<Entry>) -> PaneState {
        let path = root().join("home").join("user");
        let (mut pane, changes) =
            PaneState::new(Arc::new(UnusedFs), path.clone(), SortSpec::default());
        let req = changes.load.expect("initial load requested");
        assert_eq!(req.path, path);
        let _ = complete(&mut pane, &req, entries);
        pane
    }

    fn complete(pane: &mut PaneState, req: &LoadRequest, entries: Vec<Entry>) -> Changes {
        let now = Instant::now();
        let _ = pane.on_dir_event(req.token, DirEvent::Entries(entries), now);
        pane.on_dir_event(req.token, DirEvent::Done, now)
    }

    fn row_name(pane: &PaneState, row: usize) -> String {
        match pane.listing().row(row) {
            Some(Row::Parent) => "..".into(),
            Some(Row::Entry(e)) => e.name.to_string_lossy().into_owned(),
            None => "<none>".into(),
        }
    }

    #[test]
    fn initial_load_shows_sorted_entries_with_parent_row() {
        let pane = loaded_pane(vec![file("b"), dir("a"), file("a")]);
        assert_eq!(pane.listing().len(), 4);
        assert_eq!(
            (0..4).map(|r| row_name(&pane, r)).collect::<Vec<_>>(),
            ["..", "a", "a", "b"]
        );
        assert_eq!(pane.status(), PaneStatus::default());
    }

    #[test]
    fn root_has_no_parent_row() {
        let (mut pane, changes) = PaneState::new(Arc::new(UnusedFs), root(), SortSpec::default());
        let _ = complete(&mut pane, &changes.load.unwrap(), vec![dir("etc")]);
        assert_eq!(row_name(&pane, 0), "etc");
        assert_eq!(pane.go_up(), Changes::default());
    }

    #[test]
    fn opening_a_directory_then_going_up_focuses_where_we_came_from() {
        let mut pane = loaded_pane(vec![dir("docs"), dir("music"), file("x")]);
        let snapshot = pane.listing().id();

        let changes = pane.open(snapshot, 2); // "music"
        let req = changes.load.expect("navigation starts a load");
        assert_eq!(req.path, root().join("home").join("user").join("music"));
        assert!(pane.status().loading.is_some());
        // Until the new listing arrives, the old one stays.
        assert_eq!(pane.listing().id(), snapshot);

        let changes = complete(&mut pane, &req, vec![file("song.mp3")]);
        assert_eq!(changes.listing, Some(0), "cursor starts on the first row");
        assert_eq!(pane.location(), req.path);

        let req = pane.go_up().load.unwrap();
        let changes = complete(&mut pane, &req, vec![dir("docs"), dir("music"), file("x")]);
        assert_eq!(changes.listing, Some(2), "cursor is back on 'music'");
    }

    #[test]
    fn opening_parent_row_goes_up() {
        let mut pane = loaded_pane(vec![file("x")]);
        let req = pane.open(pane.listing().id(), 0).load.unwrap();
        assert_eq!(req.path, root().join("home"));
    }

    #[test]
    fn opening_a_file_reports_that_it_is_not_supported_yet() {
        let mut pane = loaded_pane(vec![file("x")]);
        let changes = pane.open(pane.listing().id(), 1);
        assert_eq!(changes, Changes::status());
        assert_eq!(pane.status().message.unwrap().level, Level::Info);
    }

    #[test]
    fn outdated_snapshots_and_tokens_are_ignored() {
        let mut pane = loaded_pane(vec![dir("a"), dir("b")]);
        let old_snapshot = pane.listing().id();
        let first = pane.refresh().load.unwrap();
        let second = pane.refresh().load.unwrap();
        assert_ne!(first.token, second.token);

        // Events of the superseded load change nothing.
        let now = Instant::now();
        assert_eq!(
            pane.on_dir_event(first.token, DirEvent::Entries(vec![dir("stale")]), now),
            Changes::default()
        );
        assert_eq!(
            pane.on_dir_event(first.token, DirEvent::Done, now),
            Changes::default()
        );

        let _ = complete(&mut pane, &second, vec![dir("a"), dir("b"), dir("c")]);
        assert_eq!(pane.listing().entry_count(), 3);
        // Rows of the old snapshot can no longer be opened.
        assert_eq!(pane.open(old_snapshot, 1), Changes::default());
    }

    #[test]
    fn refresh_keeps_cursor_on_the_same_entry_or_row() {
        let mut pane = loaded_pane(vec![file("a"), file("b"), file("c")]);
        pane.cursor_moved(pane.listing().id(), 2); // "b"

        let req = pane.refresh().load.unwrap();
        let changes = complete(
            &mut pane,
            &req,
            vec![file("0"), file("a"), file("b"), file("c")],
        );
        assert_eq!(changes.listing, Some(3), "follows 'b' to its new row");

        // If the entry disappears, the cursor stays on the same row.
        let req = pane.refresh().load.unwrap();
        let changes = complete(&mut pane, &req, vec![file("0"), file("a"), file("c")]);
        assert_eq!(changes.listing, Some(3));
    }

    #[test]
    fn sorting_keeps_cursor_on_the_same_entry() {
        let mut big = file("big");
        big.size = Some(100);
        let mut small = file("small");
        small.size = Some(1);
        let mut pane = loaded_pane(vec![big, small]);
        pane.cursor_moved(pane.listing().id(), 2); // "small"

        let changes = pane.sort_by(SortKey::Size); // descending: big, small
        assert_eq!(changes.listing, Some(2));
        let changes = pane.sort_by(SortKey::Size); // ascending: small, big
        assert_eq!(changes.listing, Some(1));
        assert_eq!(row_name(&pane, 1), "small");
    }

    #[test]
    fn failed_navigation_keeps_the_current_listing() {
        let mut pane = loaded_pane(vec![dir("locked")]);
        let snapshot = pane.listing().id();
        let req = pane.open(snapshot, 1).load.unwrap();
        let err = VfsError::PermissionDenied {
            path: req.path.clone(),
        };
        let changes = pane.on_dir_event(req.token, DirEvent::Failed(err), Instant::now());

        assert_eq!(changes, Changes::status());
        assert_eq!(pane.listing().id(), snapshot);
        assert_eq!(pane.location(), root().join("home").join("user"));
        let status = pane.status();
        assert!(status.loading.is_none());
        let message = status.message.unwrap();
        assert_eq!(message.level, Level::Error);
        assert!(
            message.text.contains("permission denied"),
            "{}",
            message.text
        );
    }

    #[test]
    fn failed_initial_load_shows_the_location_so_the_user_can_go_up() {
        let path = root().join("gone");
        let (mut pane, changes) =
            PaneState::new(Arc::new(UnusedFs), path.clone(), SortSpec::default());
        let req = changes.load.unwrap();
        let err = VfsError::NotFound { path: path.clone() };
        let changes = pane.on_dir_event(req.token, DirEvent::Failed(err), Instant::now());
        assert_eq!(changes.listing, Some(0));
        assert_eq!(pane.location(), path);
        assert_eq!(row_name(&pane, 0), "..");
        assert_eq!(pane.go_up().load.unwrap().path, root());
    }

    #[test]
    fn slow_loads_publish_progress_and_partial_listings() {
        let (mut pane, changes) =
            PaneState::new(Arc::new(UnusedFs), root().join("slow"), SortSpec::default());
        let req = changes.load.unwrap();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);

        // First batch: progress, no listing yet.
        let c = pane.on_dir_event(req.token, DirEvent::Entries(vec![file("a")]), at(10));
        assert_eq!(c, Changes::status());
        assert_eq!(pane.status().loading.unwrap().entries, 1);
        // Too soon for another progress update.
        let c = pane.on_dir_event(req.token, DirEvent::Entries(vec![file("b")]), at(50));
        assert_eq!(c, Changes::default());
        // After the partial delay a partial listing appears.
        let c = pane.on_dir_event(req.token, DirEvent::Entries(vec![file("c")]), at(500));
        assert_eq!(c.listing, Some(0));
        assert_eq!(pane.listing().entry_count(), 3);
        assert!(pane.status().loading.is_some());

        // The user moves the cursor in the partial listing; completion keeps it.
        pane.cursor_moved(pane.listing().id(), 2); // "b"
        let c = pane.on_dir_event(req.token, DirEvent::Entries(vec![file("0")]), at(600));
        assert_eq!(c.listing, None, "partial listings are rate limited");
        let c = pane.on_dir_event(req.token, DirEvent::Done, at(700));
        assert_eq!(c.listing, Some(3), "'b' moved down after '0' arrived");
        assert!(pane.status().loading.is_none());
    }

    #[test]
    fn partial_listings_repeat_at_a_bounded_rate() {
        let (mut pane, changes) =
            PaneState::new(Arc::new(UnusedFs), root().join("slow"), SortSpec::default());
        let req = changes.load.unwrap();
        let start = Instant::now();
        let mut partials = 0;
        // One batch every 100 ms for 5 seconds.
        for tick in 1..=50u64 {
            let batch = DirEvent::Entries(vec![file(&format!("f{tick}"))]);
            let at = start + Duration::from_millis(tick * 100);
            if pane.on_dir_event(req.token, batch, at).listing.is_some() {
                partials += 1;
            }
        }
        // First after 0.4 s, then at most one per second.
        assert!((4..=5).contains(&partials), "{partials} partial listings");
    }

    #[test]
    fn warnings_are_summarized() {
        let (mut pane, changes) =
            PaneState::new(Arc::new(UnusedFs), root().join("w"), SortSpec::default());
        let req = changes.load.unwrap();
        let now = Instant::now();
        for name in ["x", "y"] {
            let err = VfsError::NotFound {
                path: PathBuf::from(name),
            };
            let _ = pane.on_dir_event(req.token, DirEvent::Warning(err), now);
        }
        let _ = pane.on_dir_event(req.token, DirEvent::Done, now);
        let message = pane.status().message.unwrap();
        assert_eq!(message.level, Level::Warning);
        assert!(message.text.contains("and 1 more"), "{}", message.text);
    }

    /// Completes the load requested by `changes` with `entries`.
    fn arrive(pane: &mut PaneState, changes: Changes, entries: Vec<Entry>) -> Changes {
        let req = changes.load.expect("a load was requested");
        complete(pane, &req, entries)
    }

    fn history(pane: &PaneState) -> (bool, bool) {
        let h = pane.status().history;
        (h.can_go_back, h.can_go_forward)
    }

    fn home() -> PathBuf {
        root().join("home").join("user")
    }

    #[test]
    fn back_and_forward_restore_location_and_cursor() {
        let mut pane = loaded_pane(vec![dir("a"), dir("b"), file("x")]);
        assert_eq!(
            history(&pane),
            (false, false),
            "the start location is not history"
        );
        pane.cursor_moved(pane.listing().id(), 2); // "b"
        let changes = pane.open(pane.listing().id(), 2);
        let _ = arrive(&mut pane, changes, vec![file("inside")]);
        assert_eq!(pane.location(), home().join("b"));
        assert_eq!(history(&pane), (true, false));

        let changes = pane.go_back();
        assert_eq!(changes.load.as_ref().unwrap().path, home());
        let changes = arrive(&mut pane, changes, vec![dir("a"), dir("b"), file("x")]);
        assert_eq!(changes.listing, Some(2), "cursor back on 'b'");
        assert_eq!(history(&pane), (false, true));

        let changes = pane.go_forward();
        let _ = arrive(&mut pane, changes, vec![file("inside")]);
        assert_eq!(pane.location(), home().join("b"));
        assert_eq!(history(&pane), (true, false));
    }

    #[test]
    fn new_navigation_clears_forward_history() {
        let mut pane = loaded_pane(vec![dir("a"), dir("b")]);
        let changes = pane.navigate(home().join("a"));
        let _ = arrive(&mut pane, changes, vec![]);
        let changes = pane.go_back();
        let _ = arrive(&mut pane, changes, vec![dir("a"), dir("b")]);
        assert_eq!(history(&pane), (false, true));

        let changes = pane.navigate(home().join("b"));
        let _ = arrive(&mut pane, changes, vec![]);
        assert_eq!(history(&pane), (true, false));
    }

    #[test]
    fn refreshes_and_failures_do_not_enter_history() {
        let mut pane = loaded_pane(vec![dir("a")]);
        let changes = pane.refresh();
        let _ = arrive(&mut pane, changes, vec![dir("a")]);
        let changes = pane.navigate(home());
        let _ = arrive(&mut pane, changes, vec![dir("a")]);
        assert_eq!(history(&pane), (false, false));

        let req = pane.navigate(home().join("a")).load.unwrap();
        let err = VfsError::PermissionDenied {
            path: req.path.clone(),
        };
        let _ = pane.on_dir_event(req.token, DirEvent::Failed(err), Instant::now());
        assert_eq!(history(&pane), (false, false));
    }

    #[test]
    fn pressing_back_again_while_loading_goes_further() {
        let mut pane = loaded_pane(vec![]); // A = home
        for name in ["b", "c"] {
            let changes = pane.navigate(home().join(name));
            let _ = arrive(&mut pane, changes, vec![]);
        }
        // History: A, B; current C.
        let first = pane.go_back().load.unwrap();
        assert_eq!(first.path, home().join("b"));
        let changes = pane.go_back();
        assert_eq!(changes.load.as_ref().unwrap().path, home());
        let _ = arrive(&mut pane, changes, vec![]);
        assert_eq!(pane.location(), home());
        assert_eq!(history(&pane), (false, true));

        // Forward walks through the skipped location again.
        let changes = pane.go_forward();
        assert_eq!(changes.load.as_ref().unwrap().path, home().join("b"));
        let _ = arrive(&mut pane, changes, vec![]);
        let changes = pane.go_forward();
        assert_eq!(changes.load.as_ref().unwrap().path, home().join("c"));
    }

    #[test]
    fn unreachable_history_entries_are_dropped() {
        let mut pane = loaded_pane(vec![]);
        let changes = pane.navigate(home().join("gone"));
        let _ = arrive(&mut pane, changes, vec![]);
        let changes = pane.navigate(home().join("next"));
        let _ = arrive(&mut pane, changes, vec![]);
        // History: home, gone; current next. "gone" disappears.
        let req = pane.go_back().load.unwrap();
        let err = VfsError::NotFound {
            path: req.path.clone(),
        };
        let _ = pane.on_dir_event(req.token, DirEvent::Failed(err), Instant::now());
        assert_eq!(pane.location(), home().join("next"));
        let changes = pane.go_back();
        assert_eq!(
            changes.load.unwrap().path,
            home(),
            "skips the dropped entry"
        );
    }

    #[test]
    fn history_is_bounded() {
        let mut pane = loaded_pane(vec![]);
        for i in 0..HISTORY_LIMIT + 20 {
            let changes = pane.navigate(home().join(i.to_string()));
            let _ = arrive(&mut pane, changes, vec![]);
        }
        assert_eq!(pane.back.len(), HISTORY_LIMIT);
    }

    #[test]
    fn typed_locations_resolve_against_the_current_directory() {
        let mut pane = loaded_pane(vec![dir("sub")]);
        assert_eq!(pane.navigate_to_input("   ", None), Changes::default());
        let req = pane.navigate_to_input("sub/../sub", None).load.unwrap();
        assert_eq!(req.path, home().join("sub"));
    }

    #[test]
    fn typed_file_path_opens_its_directory_with_the_file_selected() {
        let mut pane = loaded_pane(vec![dir("sub")]);
        let req = pane.navigate_to_input("sub/notes.txt", None).load.unwrap();
        let err = VfsError::NotADirectory {
            path: req.path.clone(),
        };
        let changes = pane.on_dir_event(req.token, DirEvent::Failed(err), Instant::now());
        let changes = arrive(&mut pane, changes, vec![file("a.txt"), file("notes.txt")]);
        assert_eq!(pane.location(), home().join("sub"));
        assert_eq!(changes.listing, Some(2), "cursor on notes.txt");
        assert_eq!(pane.status().message, None);
        assert_eq!(history(&pane), (true, false));
    }

    #[test]
    fn opening_a_file_path_from_a_listing_still_fails_normally() {
        // Only typed locations fall back to the parent directory.
        let mut pane = loaded_pane(vec![dir("sub")]);
        let req = pane.navigate(home().join("file")).load.unwrap();
        let err = VfsError::NotADirectory {
            path: req.path.clone(),
        };
        let changes = pane.on_dir_event(req.token, DirEvent::Failed(err), Instant::now());
        assert_eq!(changes.load, None);
        assert_eq!(pane.status().message.unwrap().level, Level::Error);
    }

    fn selected_names(pane: &PaneState) -> Vec<String> {
        let entries = pane.listing().entries();
        let mut names: Vec<String> = pane
            .selection()
            .iter()
            .map(|i| entries[i as usize].name.to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn sized(name: &str, size: u64) -> Entry {
        let mut entry = file(name);
        entry.size = Some(size);
        entry
    }

    #[test]
    fn toggling_selects_entries_but_not_the_parent_row() {
        let mut pane = loaded_pane(vec![dir("d"), sized("a", 10), sized("b", 5)]);
        let id = pane.listing().id();
        assert_eq!(
            pane.toggle_selection(id, 0),
            Changes::default(),
            "'..' is not selectable"
        );
        let changes = pane.toggle_selection(id, 2);
        assert_eq!(changes.selection, Some(SelectionChange::Rows(vec![2])));
        assert!(changes.status);
        let _ = pane.toggle_selection(id, 1);
        assert_eq!(selected_names(&pane), ["a", "d"]);
        let summary = pane.status().selection;
        assert_eq!(
            summary,
            SelectionSummary {
                dirs: 1,
                files: 1,
                bytes: 10
            }
        );

        let _ = pane.toggle_selection(id, 2);
        assert_eq!(selected_names(&pane), ["d"]);
        let stale = pane.listing().id();
        let _ = pane.sort_by(SortKey::Size);
        assert_eq!(
            pane.toggle_selection(stale, 1),
            Changes::default(),
            "outdated snapshot"
        );
    }

    #[test]
    fn range_selection_grows_and_shrinks_on_top_of_earlier_selection() {
        let mut pane = loaded_pane((0..6).map(|i| file(&format!("f{i}"))).collect());
        let id = pane.listing().id();
        let _ = pane.toggle_selection(id, 6); // f5, selected before the range
        let changes = pane.select_range(id, 2, 4); // f1..f3
        assert_eq!(
            changes.selection,
            Some(SelectionChange::Rows(vec![2, 3, 4]))
        );
        assert_eq!(selected_names(&pane), ["f1", "f2", "f3", "f5"]);

        // Same anchor, shorter range: f3 is released, f5 stays.
        let changes = pane.select_range(id, 2, 3);
        assert_eq!(
            changes.selection,
            Some(SelectionChange::Rows(vec![2, 3, 4]))
        );
        assert_eq!(selected_names(&pane), ["f1", "f2", "f5"]);

        // Upwards past the anchor, including the `..` row which is skipped.
        let _ = pane.select_range(id, 2, 0);
        assert_eq!(selected_names(&pane), ["f0", "f1", "f5"]);
    }

    #[test]
    fn select_all_and_deselect_all() {
        let mut pane = loaded_pane(vec![dir("d"), file("a"), file("b")]);
        let changes = pane.select_all();
        assert_eq!(changes.selection, Some(SelectionChange::All));
        assert_eq!(selected_names(&pane), ["a", "b", "d"]);
        assert_eq!(pane.select_all(), Changes::default(), "nothing changed");
        let _ = pane.deselect_all();
        assert!(pane.selection().is_empty());
        assert_eq!(pane.deselect_all(), Changes::default());
    }

    #[test]
    fn filter_narrows_rows_and_moves_cursor_to_the_first_match() {
        let mut pane = loaded_pane(vec![
            dir("docs"),
            file("a.txt"),
            file("b.rs"),
            file("c.txt"),
        ]);
        let changes = pane.set_filter("*.txt");
        assert_eq!(
            (0..pane.listing().len())
                .map(|r| row_name(&pane, r))
                .collect::<Vec<_>>(),
            ["..", "a.txt", "c.txt"]
        );
        assert_eq!(changes.listing, Some(1), "first match, not '..'");
        assert_eq!(pane.status().filter, "*.txt");
        assert_eq!(pane.set_filter("*.txt"), Changes::default());

        // The cursor stays on its entry while it remains visible.
        pane.cursor_moved(pane.listing().id(), 2); // c.txt
        let changes = pane.set_filter("c.t");
        assert_eq!(changes.listing, Some(1));
        assert_eq!(row_name(&pane, 1), "c.txt");

        let changes = pane.set_filter("");
        assert_eq!(pane.listing().entry_count(), 4);
        assert_eq!(row_name(&pane, changes.listing.unwrap()), "c.txt");
        assert_eq!(pane.status().filter, "");
    }

    #[test]
    fn filtering_deselects_hidden_entries_and_select_all_respects_it() {
        let mut pane = loaded_pane(vec![file("a.txt"), file("b.rs")]);
        let _ = pane.select_all();
        let _ = pane.set_filter("*.txt");
        assert_eq!(
            selected_names(&pane),
            ["a.txt"],
            "b.rs is hidden, so deselected"
        );
        let _ = pane.set_filter("");
        let _ = pane.set_filter("*.rs");
        let _ = pane.select_all();
        assert_eq!(selected_names(&pane), ["b.rs"]);
    }

    #[test]
    fn sorting_keeps_the_selection() {
        let mut pane = loaded_pane(vec![sized("big", 100), sized("small", 1)]);
        let _ = pane.toggle_selection(pane.listing().id(), 1); // big
        let _ = pane.sort_by(SortKey::Size);
        let _ = pane.sort_by(SortKey::Size); // ascending: small, big
        assert_eq!(selected_names(&pane), ["big"]);
        assert_eq!(
            pane.listing().entry_index(2),
            pane.selection().iter().next()
        );
    }

    #[test]
    fn refresh_keeps_selection_and_filter_but_navigation_clears_them() {
        let mut pane = loaded_pane(vec![file("a.txt"), file("b.txt"), file("c.rs")]);
        let _ = pane.set_filter("txt");
        let _ = pane.toggle_selection(pane.listing().id(), 2); // b.txt

        // Refresh: entries arrive in a different order, plus a new one.
        let changes = pane.refresh();
        let _ = arrive(
            &mut pane,
            changes,
            vec![file("new.txt"), file("c.rs"), file("b.txt"), file("a.txt")],
        );
        assert_eq!(selected_names(&pane), ["b.txt"]);
        assert_eq!(pane.status().filter, "txt");
        assert_eq!(pane.listing().entry_count(), 3);

        let changes = pane.navigate(home().join("elsewhere"));
        let _ = arrive(&mut pane, changes, vec![file("b.txt"), file("z.rs")]);
        assert!(pane.selection().is_empty());
        assert_eq!(pane.status().filter, "");
        assert_eq!(pane.listing().entry_count(), 2);
    }

    #[test]
    fn duplicate_shares_the_listing_and_refreshes() {
        let mut pane = loaded_pane(vec![dir("a"), file("b")]);
        let _ = pane.navigate(home().join("x")); // pending load is not copied
        let _ = pane.toggle_selection(pane.listing().id(), 2);
        let (copy, changes) = pane.duplicate();
        assert!(Arc::ptr_eq(copy.listing(), &pane.full));
        assert_eq!(
            changes.load.unwrap().path,
            home(),
            "refreshes its own location"
        );
        assert!(copy.selection().is_empty());
        assert_eq!(copy.status().history, HistoryState::default());
        assert_eq!(copy.title(), "user");
    }

    #[test]
    fn directory_at_rows() {
        let pane = loaded_pane(vec![dir("a"), file("b")]);
        let id = pane.listing().id();
        assert_eq!(pane.directory_at(id, 0), Some(root().join("home")));
        assert_eq!(pane.directory_at(id, 1), Some(home().join("a")));
        assert_eq!(pane.directory_at(id, 2), None, "files are not directories");
        let (root_pane, _) = PaneState::new(Arc::new(UnusedFs), root(), SortSpec::default());
        assert_eq!(root_pane.title(), root().display().to_string());
    }
}
