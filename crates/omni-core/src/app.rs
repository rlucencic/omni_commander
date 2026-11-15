//! The application core: an actor task that owns all pane state.
//!
//! Front ends send [`Command`]s through an [`AppHandle`] and receive
//! [`Update`]s on a bounded channel. All state lives inside the actor, so there
//! is no shared mutable state and no locking; filesystem work happens on
//! background tasks whose results come back as messages.
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
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::listing::{Listing, SnapshotId};
use crate::pane::{Changes, LoadRequest, LoadToken, PaneState, PaneStatus};
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
/// [`SnapshotId`] of the listing the row was taken from.
#[derive(Debug, Clone)]
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
    Navigate {
        pane: PaneId,
        path: PathBuf,
    },
    Refresh {
        pane: PaneId,
    },
    SortBy {
        pane: PaneId,
        key: SortKey,
    },
    /// Make `pane` the active pane.
    Activate(PaneId),
    /// Make the other pane active.
    SwitchPane,
}

/// A change the front end must display.
#[derive(Debug, Clone)]
pub enum Update {
    /// New listing for a pane; place the cursor on `cursor`.
    Listing {
        pane: PaneId,
        location: PathBuf,
        listing: Arc<Listing>,
        sort: SortSpec,
        cursor: usize,
    },
    /// Loading progress or status message of a pane changed.
    Status { pane: PaneId, status: PaneStatus },
    /// The active pane changed.
    ActivePane(PaneId),
}

/// Initial state of a pane.
#[derive(Debug, Clone)]
pub struct PaneConfig {
    pub fs: Arc<dyn FileSystem>,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub left: PaneConfig,
    pub right: PaneConfig,
    pub sort: SortSpec,
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
pub fn spawn(
    runtime: &tokio::runtime::Handle,
    config: AppConfig,
) -> (AppHandle, mpsc::Receiver<Update>, JoinHandle<()>) {
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

/// Directory event tagged with the pane and load it belongs to.
#[derive(Debug)]
struct PaneDirEvent {
    pane: PaneId,
    token: LoadToken,
    event: DirEvent,
}

/// The update receiver was dropped: the front end is gone.
#[derive(Debug)]
struct Disconnected;

struct Core {
    panes: [PaneState; 2],
    active: PaneId,
    updates: mpsc::Sender<Update>,
    dir_events: mpsc::Sender<PaneDirEvent>,
    /// Task forwarding the current enumeration of each pane. Replacing it
    /// aborts the old task, which drops its stream and cancels the provider.
    loaders: [Option<JoinHandle<()>>; 2],
}

async fn run(
    config: AppConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: mpsc::Sender<Update>,
) {
    let (dir_event_tx, mut dir_events) = mpsc::channel(DIR_EVENT_CAPACITY);
    let (left, left_changes) = PaneState::new(config.left.fs, config.left.path, config.sort);
    let (right, right_changes) = PaneState::new(config.right.fs, config.right.path, config.sort);
    let mut core = Core {
        panes: [left, right],
        active: PaneId::Left,
        updates,
        dir_events: dir_event_tx,
        loaders: [None, None],
    };

    let started = async {
        core.send(Update::ActivePane(core.active)).await?;
        core.apply(PaneId::Left, left_changes).await?;
        core.apply(PaneId::Right, right_changes).await
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
    for loader in core.loaders.iter_mut().filter_map(Option::take) {
        loader.abort();
    }
    tracing::debug!("application core stopped");
}

impl Core {
    async fn handle_command(&mut self, command: Command) -> Result<(), Disconnected> {
        tracing::trace!(?command, "command");
        let (pane, changes) = match command {
            Command::Open {
                pane,
                snapshot,
                row,
            } => (pane, self.pane(pane).open(snapshot, row)),
            Command::CursorMoved {
                pane,
                snapshot,
                row,
            } => {
                self.pane(pane).cursor_moved(snapshot, row);
                return Ok(());
            }
            Command::GoUp { pane } => (pane, self.pane(pane).go_up()),
            Command::Navigate { pane, path } => (pane, self.pane(pane).navigate(path)),
            Command::Refresh { pane } => (pane, self.pane(pane).refresh()),
            Command::SortBy { pane, key } => (pane, self.pane(pane).sort_by(key)),
            Command::Activate(pane) => return self.set_active(pane).await,
            Command::SwitchPane => return self.set_active(self.active.other()).await,
        };
        self.apply(pane, changes).await
    }

    async fn handle_dir_event(&mut self, event: PaneDirEvent) -> Result<(), Disconnected> {
        let PaneDirEvent { pane, token, event } = event;
        let changes = self.pane(pane).on_dir_event(token, event, Instant::now());
        self.apply(pane, changes).await
    }

    async fn set_active(&mut self, pane: PaneId) -> Result<(), Disconnected> {
        if self.active == pane {
            return Ok(());
        }
        self.active = pane;
        self.send(Update::ActivePane(pane)).await
    }

    fn pane(&mut self, pane: PaneId) -> &mut PaneState {
        &mut self.panes[pane.index()]
    }

    /// Carries out what a pane state transition asked for.
    async fn apply(&mut self, pane: PaneId, changes: Changes) -> Result<(), Disconnected> {
        if let Some(request) = changes.load {
            self.start_load(pane, request);
        }
        let state = &self.panes[pane.index()];
        if let Some(cursor) = changes.listing {
            let update = Update::Listing {
                pane,
                location: state.location().to_path_buf(),
                listing: Arc::clone(state.listing()),
                sort: state.sort(),
                cursor,
            };
            self.send(update).await?;
        }
        if changes.status {
            let status = self.panes[pane.index()].status();
            self.send(Update::Status { pane, status }).await?;
        }
        Ok(())
    }

    fn start_load(&mut self, pane: PaneId, request: LoadRequest) {
        let LoadRequest { token, path } = request;
        tracing::debug!(?pane, path = %path.display(), "loading directory");
        let mut stream = self.panes[pane.index()].fs().read_dir(&path);
        let events = self.dir_events.clone();
        let loader = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                if events
                    .send(PaneDirEvent { pane, token, event })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        if let Some(previous) = self.loaders[pane.index()].replace(loader) {
            previous.abort();
        }
    }

    async fn send(&self, update: Update) -> Result<(), Disconnected> {
        self.updates.send(update).await.map_err(|_| Disconnected)
    }
}
