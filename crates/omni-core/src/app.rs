//! The application core: an actor task that owns all pane state.
//!
//! Front ends send [`Command`]s through an [`AppHandle`] and receive
//! [`Update`]s on a bounded channel. All state lives inside the actor, so there
//! is no shared mutable state and no locking; filesystem work happens on
//! background tasks whose results come back as messages.
//!
//! Each of the two panes holds one or more tabs. Commands addressed to a pane
//! act on its active tab, and updates describe the active tab only: when the
//! active tab changes, its complete state is published again. Background tabs
//! keep loading, but stay invisible until activated.
//!
//! ```text
//!  UI thread ──Command──▶ actor ──read_dir──▶ provider (blocking pool)
//!      ▲                    │  ◀──DirEvent───┘
//!      └──────Update────────┘
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use omni_vfs::{DirEvent, FileSystem};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::listing::{Listing, SnapshotId};
use crate::pane::{Changes, Level, LoadRequest, LoadToken, PaneState, PaneStatus, SelectionChange};
use crate::selection::Selection;
use crate::sort::{SortKey, SortSpec};

/// Capacity of the update channel. When the UI falls behind, the core waits
/// instead of queueing unbounded work.
const UPDATE_CAPACITY: usize = 64;
/// Capacity of the channel carrying directory events into the actor.
const DIR_EVENT_CAPACITY: usize = 16;

/// One of the two file panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneId {
    Left,
    Right,
}

impl PaneId {
    pub const ALL: [Self; 2] = [Self::Left, Self::Right];

    pub fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// A request from the front end. Commands that refer to a row carry the
/// [`SnapshotId`] of the listing the row was taken from. Commands addressed
/// to a pane act on its active tab.
#[derive(Debug)]
pub enum Command {
    /// Enter the directory at `row`, or go up for the `..` row.
    Open {
        pane: PaneId,
        snapshot: SnapshotId,
        row: usize,
    },
    /// The user moved the cursor; remembered to keep it across refresh/sort.
    CursorMoved {
        pane: PaneId,
        snapshot: SnapshotId,
        row: usize,
    },
    GoUp {
        pane: PaneId,
    },
    GoBack {
        pane: PaneId,
    },
    GoForward {
        pane: PaneId,
    },
    /// Go to an exact location (e.g. a breadcrumb).
    Navigate {
        pane: PaneId,
        path: PathBuf,
    },
    /// Go to a location typed by the user: may be relative, start with `~`,
    /// or name a file (whose directory is then shown).
    NavigateToInput {
        pane: PaneId,
        input: String,
    },
    Refresh {
        pane: PaneId,
    },
    SortBy {
        pane: PaneId,
        key: SortKey,
    },
    /// Select or deselect the entry in `row` (Insert, Space, Ctrl+click).
    ToggleSelection {
        pane: PaneId,
        snapshot: SnapshotId,
        row: usize,
    },
    /// Shift-selection from `anchor` to `row`; repeat with the same anchor
    /// to grow or shrink the range.
    SelectRange {
        pane: PaneId,
        snapshot: SnapshotId,
        anchor: usize,
        row: usize,
    },
    SelectAll {
        pane: PaneId,
    },
    DeselectAll {
        pane: PaneId,
    },
    /// Set the quick filter; empty text removes it.
    SetFilter {
        pane: PaneId,
        text: String,
    },
    /// Open a new tab next to the active one, showing the same location.
    NewTab {
        pane: PaneId,
    },
    /// Open the directory at `row` (or the parent for `..`) in a new tab;
    /// in the `background` unless the new tab should become active.
    OpenInNewTab {
        pane: PaneId,
        snapshot: SnapshotId,
        row: usize,
        background: bool,
    },
    /// Close tab `index` (the active tab if `None`). The last tab of a pane
    /// is never closed.
    CloseTab {
        pane: PaneId,
        index: Option<usize>,
    },
    SelectTab {
        pane: PaneId,
        index: usize,
    },
    /// Activate the next (or previous) tab, wrapping around.
    CycleTab {
        pane: PaneId,
        forward: bool,
    },
    /// Show or hide entries with the hidden attribute, in every tab.
    ToggleHidden,
    /// Show a message in the pane's message line.
    ShowMessage {
        pane: PaneId,
        level: Level,
        text: String,
    },
    /// Reply with what is open, e.g. to save the session on exit.
    Snapshot(oneshot::Sender<SessionSnapshot>),
    /// Make `pane` the active pane.
    Activate(PaneId),
    /// Make the other pane active.
    SwitchPane,
}

/// A change the front end must display. Apart from [`Update::Tabs`], updates
/// describe the active tab of `pane`.
#[derive(Debug, Clone)]
pub enum Update {
    /// New listing for a pane; place the cursor on `cursor`.
    Listing {
        pane: PaneId,
        location: PathBuf,
        listing: Arc<Listing>,
        sort: SortSpec,
        cursor: usize,
        /// Selected entries, by entry index of `listing`.
        selection: Arc<Selection>,
    },
    /// The selection of the listing `snapshot` changed in the given rows.
    Selection {
        pane: PaneId,
        snapshot: SnapshotId,
        selection: Arc<Selection>,
        change: SelectionChange,
    },
    /// Loading progress, status message, selection summary or filter of a
    /// pane changed.
    Status { pane: PaneId, status: PaneStatus },
    /// The tabs of a pane: their titles in order, and which one is active.
    Tabs {
        pane: PaneId,
        titles: Vec<String>,
        active: usize,
    },
    /// The active pane changed.
    ActivePane(PaneId),
}

/// A tab: where it is and how it is sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSetup {
    pub path: PathBuf,
    pub sort: SortSpec,
}

/// The tabs of a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSetup {
    /// Never empty.
    pub tabs: Vec<TabSetup>,
    pub active_tab: usize,
}

impl PaneSetup {
    pub fn single(path: PathBuf, sort: SortSpec) -> Self {
        Self {
            tabs: vec![TabSetup { path, sort }],
            active_tab: 0,
        }
    }
}

/// What the core starts with.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub fs: Arc<dyn FileSystem>,
    pub left: PaneSetup,
    pub right: PaneSetup,
    pub active: PaneId,
    pub show_hidden: bool,
}

impl AppConfig {
    /// One tab per pane, sorted by name, hidden entries shown.
    pub fn new(fs: Arc<dyn FileSystem>, left: PathBuf, right: PathBuf) -> Self {
        Self {
            fs,
            left: PaneSetup::single(left, SortSpec::default()),
            right: PaneSetup::single(right, SortSpec::default()),
            active: PaneId::Left,
            show_hidden: true,
        }
    }
}

/// What is open, in the same shape as the configuration it can restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub left: PaneSetup,
    pub right: PaneSetup,
    pub active: PaneId,
    pub show_hidden: bool,
}

/// Sends commands to the running core. Cheap to clone; never blocks.
#[derive(Debug, Clone)]
pub struct AppHandle {
    commands: mpsc::UnboundedSender<Command>,
}

impl AppHandle {
    pub fn send(&self, command: Command) {
        // Commands are user-generated (low rate), so an unbounded channel
        // cannot grow meaningfully and keeps the UI thread from ever waiting.
        if let Err(err) = self.commands.send(command) {
            tracing::warn!(command = ?err.0, "application core is not running; command dropped");
        }
    }
}

/// Starts the core on `runtime`.
///
/// Returns the command handle, the receiver of updates, and the actor's task.
/// The actor stops when every [`AppHandle`] is dropped or the update receiver
/// is closed.
///
/// # Panics
///
/// If a pane in `config` has no tabs.
pub fn spawn(
    runtime: &tokio::runtime::Handle,
    config: AppConfig,
) -> (AppHandle, mpsc::Receiver<Update>, JoinHandle<()>) {
    assert!(
        !config.left.tabs.is_empty() && !config.right.tabs.is_empty(),
        "every pane needs at least one tab"
    );
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (update_tx, update_rx) = mpsc::channel(UPDATE_CAPACITY);
    let task = runtime.spawn(run(config, command_rx, update_tx));
    (
        AppHandle {
            commands: command_tx,
        },
        update_rx,
        task,
    )
}

/// Identifies a tab for its whole lifetime, independent of its position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TabId(u64);

/// Directory event tagged with the tab and load it belongs to.
#[derive(Debug)]
struct TabDirEvent {
    tab: TabId,
    token: LoadToken,
    event: DirEvent,
}

/// The update receiver was dropped: the front end is gone.
#[derive(Debug)]
struct Disconnected;

struct Tab {
    id: TabId,
    state: PaneState,
    /// Task forwarding the tab's current enumeration. Replacing or dropping
    /// it aborts the task, which drops the stream and cancels the provider.
    loader: Option<JoinHandle<()>>,
    /// A restored background tab's first load, started when it is first
    /// activated rather than all at once on startup.
    deferred: Option<LoadRequest>,
}

impl Drop for Tab {
    fn drop(&mut self) {
        if let Some(loader) = self.loader.take() {
            loader.abort();
        }
    }
}

struct Side {
    tabs: Vec<Tab>,
    active: usize,
    /// The tab bar as last sent to the front end.
    published_tabs: Option<(Vec<String>, usize)>,
}

impl Side {
    fn active(&self) -> &PaneState {
        &self.tabs[self.active].state
    }
}

struct Core {
    sides: [Side; 2],
    active: PaneId,
    show_hidden: bool,
    next_tab: u64,
    updates: mpsc::Sender<Update>,
    dir_events: mpsc::Sender<TabDirEvent>,
}

async fn run(
    config: AppConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: mpsc::Sender<Update>,
) {
    let (dir_event_tx, mut dir_events) = mpsc::channel(DIR_EVENT_CAPACITY);
    let mut next_tab = 0;
    let (left, left_changes) =
        open_side(config.left, &config.fs, config.show_hidden, &mut next_tab);
    let (right, right_changes) =
        open_side(config.right, &config.fs, config.show_hidden, &mut next_tab);
    let (left_active, right_active) = (left.active, right.active);
    let mut core = Core {
        sides: [left, right],
        active: config.active,
        show_hidden: config.show_hidden,
        next_tab,
        updates,
        dir_events: dir_event_tx,
    };

    let started = async {
        core.send(Update::ActivePane(core.active)).await?;
        core.apply(PaneId::Left, left_active, left_changes).await?;
        core.apply(PaneId::Right, right_active, right_changes)
            .await?;
        core.publish_tabs(PaneId::Left).await?;
        core.publish_tabs(PaneId::Right).await
    };
    if started.await.is_ok() {
        loop {
            let result = tokio::select! {
                // User input first: navigation must not wait behind a flood
                // of directory events.
                biased;
                command = commands.recv() => match command {
                    Some(command) => core.handle_command(command).await,
                    None => break,
                },
                Some(event) = dir_events.recv() => core.handle_dir_event(event).await,
            };
            if result.is_err() {
                break;
            }
        }
    }
    // Dropping the tabs aborts their loaders.
    drop(core);
    tracing::debug!("application core stopped");
}

/// Creates the tabs of a pane. Only the active tab starts loading; the
/// others keep their first load for when they are activated. Returns the
/// side and the active tab's changes.
fn open_side(
    setup: PaneSetup,
    fs: &Arc<dyn FileSystem>,
    show_hidden: bool,
    next_tab: &mut u64,
) -> (Side, Changes) {
    let active = setup.active_tab.min(setup.tabs.len() - 1);
    let mut active_changes = Changes::default();
    let tabs = setup
        .tabs
        .into_iter()
        .enumerate()
        .map(|(index, tab)| {
            let (state, changes) = PaneState::new(Arc::clone(fs), tab.path, tab.sort);
            let mut deferred = None;
            if index == active {
                active_changes = changes;
            } else {
                deferred = changes.load;
            }
            *next_tab += 1;
            Tab {
                id: TabId(*next_tab),
                state: state.with_show_hidden(show_hidden),
                loader: None,
                deferred,
            }
        })
        .collect();
    let side = Side {
        tabs,
        active,
        published_tabs: None,
    };
    (side, active_changes)
}

impl Core {
    async fn handle_command(&mut self, command: Command) -> Result<(), Disconnected> {
        tracing::trace!(?command, "command");
        match command {
            Command::Open {
                pane,
                snapshot,
                row,
            } => self.on_active(pane, |s| s.open(snapshot, row)).await,
            Command::CursorMoved {
                pane,
                snapshot,
                row,
            } => {
                self.on_active(pane, |s| {
                    s.cursor_moved(snapshot, row);
                    Changes::default()
                })
                .await
            }
            Command::GoUp { pane } => self.on_active(pane, PaneState::go_up).await,
            Command::GoBack { pane } => self.on_active(pane, PaneState::go_back).await,
            Command::GoForward { pane } => self.on_active(pane, PaneState::go_forward).await,
            Command::Navigate { pane, path } => self.on_active(pane, |s| s.navigate(path)).await,
            Command::NavigateToInput { pane, input } => {
                let home = std::env::home_dir();
                self.on_active(pane, |s| s.navigate_to_input(&input, home.as_deref()))
                    .await
            }
            Command::Refresh { pane } => self.on_active(pane, PaneState::refresh).await,
            Command::SortBy { pane, key } => self.on_active(pane, |s| s.sort_by(key)).await,
            Command::ToggleSelection {
                pane,
                snapshot,
                row,
            } => {
                self.on_active(pane, |s| s.toggle_selection(snapshot, row))
                    .await
            }
            Command::SelectRange {
                pane,
                snapshot,
                anchor,
                row,
            } => {
                self.on_active(pane, |s| s.select_range(snapshot, anchor, row))
                    .await
            }
            Command::SelectAll { pane } => self.on_active(pane, PaneState::select_all).await,
            Command::DeselectAll { pane } => self.on_active(pane, PaneState::deselect_all).await,
            Command::SetFilter { pane, text } => {
                self.on_active(pane, |s| s.set_filter(&text)).await
            }
            Command::NewTab { pane } => {
                let (state, changes) = self.sides[pane.index()].active().duplicate();
                self.insert_tab(pane, state, changes, true).await
            }
            Command::OpenInNewTab {
                pane,
                snapshot,
                row,
                background,
            } => {
                let side = &self.sides[pane.index()];
                let Some(path) = side.active().directory_at(snapshot, row) else {
                    return Ok(());
                };
                let current = side.active();
                let (state, changes) =
                    PaneState::new(Arc::clone(current.fs()), path, current.sort());
                let state = state.with_show_hidden(self.show_hidden);
                self.insert_tab(pane, state, changes, !background).await
            }
            Command::CloseTab { pane, index } => self.close_tab(pane, index).await,
            Command::SelectTab { pane, index } => self.select_tab(pane, index).await,
            Command::CycleTab { pane, forward } => {
                let side = &self.sides[pane.index()];
                let count = side.tabs.len();
                let index = if forward {
                    (side.active + 1) % count
                } else {
                    (side.active + count - 1) % count
                };
                self.select_tab(pane, index).await
            }
            Command::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                for pane in PaneId::ALL {
                    for index in 0..self.sides[pane.index()].tabs.len() {
                        let changes = self.sides[pane.index()].tabs[index]
                            .state
                            .set_show_hidden(self.show_hidden);
                        self.apply(pane, index, changes).await?;
                    }
                }
                Ok(())
            }
            Command::ShowMessage { pane, level, text } => {
                self.on_active(pane, |s| s.notify(level, text)).await
            }
            Command::Snapshot(reply) => {
                let pane = |side: &Side| PaneSetup {
                    tabs: side
                        .tabs
                        .iter()
                        .map(|tab| TabSetup {
                            path: tab.state.location().to_path_buf(),
                            sort: tab.state.sort(),
                        })
                        .collect(),
                    active_tab: side.active,
                };
                let snapshot = SessionSnapshot {
                    left: pane(&self.sides[0]),
                    right: pane(&self.sides[1]),
                    active: self.active,
                    show_hidden: self.show_hidden,
                };
                // The requester may have given up waiting; nothing to do then.
                let _ = reply.send(snapshot);
                Ok(())
            }
            Command::Activate(pane) => self.set_active(pane).await,
            Command::SwitchPane => self.set_active(self.active.other()).await,
        }
    }

    async fn handle_dir_event(&mut self, event: TabDirEvent) -> Result<(), Disconnected> {
        let TabDirEvent { tab, token, event } = event;
        let Some((pane, index)) = self.find_tab(tab) else {
            return Ok(()); // the tab was closed
        };
        let changes =
            self.sides[pane.index()].tabs[index]
                .state
                .on_dir_event(token, event, Instant::now());
        self.apply(pane, index, changes).await
    }

    /// Runs `f` on the active tab of `pane` and carries out its changes.
    async fn on_active(
        &mut self,
        pane: PaneId,
        f: impl FnOnce(&mut PaneState) -> Changes,
    ) -> Result<(), Disconnected> {
        let side = &mut self.sides[pane.index()];
        let index = side.active;
        let changes = f(&mut side.tabs[index].state);
        self.apply(pane, index, changes).await
    }

    async fn insert_tab(
        &mut self,
        pane: PaneId,
        state: PaneState,
        changes: Changes,
        activate: bool,
    ) -> Result<(), Disconnected> {
        let id = TabId(self.next_tab);
        self.next_tab += 1;
        let side = &mut self.sides[pane.index()];
        let index = side.active + 1;
        side.tabs.insert(
            index,
            Tab {
                id,
                state,
                loader: None,
                deferred: None,
            },
        );
        if activate {
            side.active = index;
            self.publish_active_tab(pane).await?;
        }
        self.apply(pane, index, changes).await?;
        self.publish_tabs(pane).await
    }

    async fn close_tab(&mut self, pane: PaneId, index: Option<usize>) -> Result<(), Disconnected> {
        let side = &mut self.sides[pane.index()];
        let index = index.unwrap_or(side.active);
        if side.tabs.len() <= 1 || index >= side.tabs.len() {
            return Ok(());
        }
        side.tabs.remove(index);
        let was_active = index == side.active;
        if index < side.active || side.active == side.tabs.len() {
            side.active -= 1;
        }
        if was_active {
            // The neighbour to the right (or the new last tab) takes over.
            self.publish_active_tab(pane).await?;
        }
        self.publish_tabs(pane).await
    }

    async fn select_tab(&mut self, pane: PaneId, index: usize) -> Result<(), Disconnected> {
        let side = &mut self.sides[pane.index()];
        if index >= side.tabs.len() || index == side.active {
            return Ok(());
        }
        side.active = index;
        self.publish_active_tab(pane).await?;
        self.publish_tabs(pane).await
    }

    async fn set_active(&mut self, pane: PaneId) -> Result<(), Disconnected> {
        if self.active == pane {
            return Ok(());
        }
        self.active = pane;
        self.send(Update::ActivePane(pane)).await
    }

    fn find_tab(&self, id: TabId) -> Option<(PaneId, usize)> {
        PaneId::ALL.into_iter().find_map(|pane| {
            self.sides[pane.index()]
                .tabs
                .iter()
                .position(|tab| tab.id == id)
                .map(|index| (pane, index))
        })
    }

    /// Carries out what a state transition of tab `index` asked for. Only the
    /// active tab is published; a background tab may change its title.
    async fn apply(
        &mut self,
        pane: PaneId,
        index: usize,
        changes: Changes,
    ) -> Result<(), Disconnected> {
        if let Some(request) = changes.load {
            self.start_load(pane, index, request);
        }
        let side = &self.sides[pane.index()];
        if index == side.active {
            let state = side.active();
            if let Some(cursor) = changes.listing {
                let update = Update::Listing {
                    pane,
                    location: state.location().to_path_buf(),
                    listing: Arc::clone(state.listing()),
                    sort: state.sort(),
                    cursor,
                    selection: Arc::clone(state.selection()),
                };
                self.send(update).await?;
            } else if let Some(change) = changes.selection {
                let update = Update::Selection {
                    pane,
                    snapshot: state.listing().id(),
                    selection: Arc::clone(state.selection()),
                    change,
                };
                self.send(update).await?;
            }
            if changes.status {
                let status = self.sides[pane.index()].active().status();
                self.send(Update::Status { pane, status }).await?;
            }
        }
        if changes.listing.is_some() {
            self.publish_tabs(pane).await?;
        }
        Ok(())
    }

    /// Publishes the complete state of the active tab of `pane`, starting
    /// its first load if it was deferred.
    async fn publish_active_tab(&mut self, pane: PaneId) -> Result<(), Disconnected> {
        let side = &mut self.sides[pane.index()];
        let index = side.active;
        if let Some(request) = side.tabs[index].deferred.take() {
            self.start_load(pane, index, request);
        }
        let state = self.sides[pane.index()].active();
        let listing = Update::Listing {
            pane,
            location: state.location().to_path_buf(),
            listing: Arc::clone(state.listing()),
            sort: state.sort(),
            cursor: state.cursor(),
            selection: Arc::clone(state.selection()),
        };
        let status = Update::Status {
            pane,
            status: state.status(),
        };
        self.send(listing).await?;
        self.send(status).await
    }

    /// Sends the tab bar of `pane` if it differs from what was last sent.
    async fn publish_tabs(&mut self, pane: PaneId) -> Result<(), Disconnected> {
        let side = &mut self.sides[pane.index()];
        let titles: Vec<String> = side.tabs.iter().map(|tab| tab.state.title()).collect();
        let current = (titles, side.active);
        if side.published_tabs.as_ref() == Some(&current) {
            return Ok(());
        }
        side.published_tabs = Some(current.clone());
        let (titles, active) = current;
        self.send(Update::Tabs {
            pane,
            titles,
            active,
        })
        .await
    }

    fn start_load(&mut self, pane: PaneId, index: usize, request: LoadRequest) {
        let LoadRequest { token, path } = request;
        tracing::debug!(?pane, tab = index, path = %path.display(), "loading directory");
        let tab = &mut self.sides[pane.index()].tabs[index];
        let mut stream = tab.state.fs().read_dir(&path);
        let events = self.dir_events.clone();
        let id = tab.id;
        let loader = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                if events
                    .send(TabDirEvent {
                        tab: id,
                        token,
                        event,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        if let Some(previous) = tab.loader.replace(loader) {
            previous.abort();
        }
    }

    async fn send(&self, update: Update) -> Result<(), Disconnected> {
        self.updates.send(update).await.map_err(|_| Disconnected)
    }
}
