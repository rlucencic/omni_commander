//! End-to-end tests of the application actor, without any GUI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omni_core::app::{self, AppConfig, AppHandle, Command, PaneConfig, PaneId, Update};
use omni_core::{Listing, Row, SortKey, SortSpec};
use omni_vfs::{
    Capabilities, DirEvent, DirStream, Entry, EntryKind, FileSystem, LocalFs, dir_stream,
};
use tokio::sync::mpsc;

const TIMEOUT: Duration = Duration::from_secs(5);

struct Harness {
    handle: AppHandle,
    updates: mpsc::Receiver<Update>,
    task: tokio::task::JoinHandle<()>,
}

fn start(fs: Arc<dyn FileSystem>, left: &Path, right: &Path) -> Harness {
    let config = AppConfig {
        left: PaneConfig {
            fs: Arc::clone(&fs),
            path: left.to_path_buf(),
        },
        right: PaneConfig {
            fs,
            path: right.to_path_buf(),
        },
        sort: SortSpec::default(),
    };
    let (handle, updates, task) = app::spawn(&tokio::runtime::Handle::current(), config);
    Harness {
        handle,
        updates,
        task,
    }
}

struct ListingUpdate {
    location: PathBuf,
    listing: Arc<Listing>,
    cursor: usize,
}

impl Harness {
    async fn next(&mut self) -> Update {
        tokio::time::timeout(TIMEOUT, self.updates.recv())
            .await
            .expect("timed out waiting for an update")
            .expect("core stopped")
    }

    /// Waits for the next completed listing of `pane` (skipping partial ones).
    async fn listing(&mut self, pane: PaneId) -> ListingUpdate {
        let mut last = None;
        loop {
            match self.next().await {
                Update::Listing {
                    pane: p,
                    location,
                    listing,
                    cursor,
                    ..
                } if p == pane => {
                    last = Some(ListingUpdate {
                        location,
                        listing,
                        cursor,
                    });
                }
                Update::Status { pane: p, status } if p == pane && status.loading.is_none() => {
                    if let Some(done) = last.take() {
                        return done;
                    }
                }
                _ => {}
            }
        }
    }

    async fn status_message(&mut self, pane: PaneId) -> String {
        loop {
            if let Update::Status { pane: p, status } = self.next().await
                && p == pane
                && status.loading.is_none()
                && let Some(message) = status.message
            {
                return message.text;
            }
        }
    }
}

fn row_name(listing: &Listing, row: usize) -> String {
    match listing.row(row) {
        Some(Row::Parent) => "..".into(),
        Some(Row::Entry(e)) => e.name.to_string_lossy().into_owned(),
        None => "<none>".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browses_a_real_directory_tree() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("alpha")).unwrap();
    std::fs::create_dir(dir.path().join("beta")).unwrap();
    std::fs::write(dir.path().join("beta").join("inner.txt"), b"1234").unwrap();
    std::fs::write(dir.path().join("zeta.txt"), b"z").unwrap();

    let mut h = start(Arc::new(LocalFs), dir.path(), dir.path());
    let left = h.listing(PaneId::Left).await;
    assert_eq!(left.location, dir.path());
    let names: Vec<_> = (0..left.listing.len())
        .map(|r| row_name(&left.listing, r))
        .collect();
    assert_eq!(names, ["..", "alpha", "beta", "zeta.txt"]);
    assert_eq!(left.cursor, 0);

    // Enter "beta" in the left pane; the right pane is unaffected.
    h.handle.send(Command::Open {
        pane: PaneId::Left,
        snapshot: left.listing.id(),
        row: 2,
    });
    let inner = h.listing(PaneId::Left).await;
    assert_eq!(inner.location, dir.path().join("beta"));
    assert_eq!(row_name(&inner.listing, 1), "inner.txt");

    // Back up: the cursor lands on "beta".
    h.handle.send(Command::GoUp { pane: PaneId::Left });
    let back = h.listing(PaneId::Left).await;
    assert_eq!(back.location, dir.path());
    assert_eq!(row_name(&back.listing, back.cursor), "beta");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sorting_and_refresh_keep_the_cursor_entry() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a_big"), vec![0u8; 4096]).unwrap();
    std::fs::write(dir.path().join("b_small"), b"x").unwrap();

    let mut h = start(Arc::new(LocalFs), dir.path(), dir.path());
    let first = h.listing(PaneId::Left).await;
    h.handle.send(Command::CursorMoved {
        pane: PaneId::Left,
        snapshot: first.listing.id(),
        row: 2,
    });

    // Size sorts descending first: a_big, b_small. The cursor follows b_small.
    h.handle.send(Command::SortBy {
        pane: PaneId::Left,
        key: SortKey::Size,
    });
    let sorted = loop {
        if let Update::Listing {
            pane: PaneId::Left,
            listing,
            cursor,
            sort,
            ..
        } = h.next().await
        {
            assert_eq!(sort.key, SortKey::Size);
            break (listing, cursor);
        }
    };
    assert_eq!(row_name(&sorted.0, sorted.1), "b_small");

    std::fs::write(dir.path().join("0_new"), b"").unwrap();
    h.handle.send(Command::Refresh { pane: PaneId::Left });
    let refreshed = h.listing(PaneId::Left).await;
    assert_eq!(refreshed.listing.entry_count(), 3);
    assert_eq!(row_name(&refreshed.listing, refreshed.cursor), "b_small");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn navigation_errors_are_reported_and_the_pane_stays_put() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = start(Arc::new(LocalFs), dir.path(), dir.path());
    let _ = h.listing(PaneId::Left).await;

    let missing = dir.path().join("does-not-exist");
    h.handle.send(Command::Navigate {
        pane: PaneId::Left,
        path: missing,
    });
    let message = h.status_message(PaneId::Left).await;
    assert!(message.contains("no such file or directory"), "{message}");

    // Still usable: a refresh lists the original directory.
    h.handle.send(Command::Refresh { pane: PaneId::Left });
    assert_eq!(h.listing(PaneId::Left).await.location, dir.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_pane_switching() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = start(Arc::new(LocalFs), dir.path(), dir.path());
    assert!(matches!(h.next().await, Update::ActivePane(PaneId::Left)));

    h.handle.send(Command::SwitchPane);
    h.handle.send(Command::Activate(PaneId::Right)); // already active: no update
    h.handle.send(Command::Activate(PaneId::Left));
    let mut changes = Vec::new();
    while changes.len() < 2 {
        if let Update::ActivePane(pane) = h.next().await {
            changes.push(pane);
        }
    }
    assert_eq!(changes, [PaneId::Right, PaneId::Left]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn core_stops_when_the_front_end_goes_away() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(Arc::new(LocalFs), dir.path(), dir.path());
    drop(h.handle);
    drop(h.updates);
    tokio::time::timeout(TIMEOUT, h.task)
        .await
        .expect("core did not stop")
        .unwrap();
}

/// A filesystem whose enumerations only progress when the test says so.
#[derive(Debug, Default)]
struct GatedFs {
    pending: Mutex<HashMap<PathBuf, mpsc::Sender<DirEvent>>>,
}

impl GatedFs {
    /// Waits until `path` is being enumerated, then delivers `events` to
    /// that enumeration (each enumeration is fed exactly once).
    async fn feed(&self, path: &Path, events: Vec<DirEvent>) {
        let tx = loop {
            if let Some(tx) = self.pending.lock().unwrap().remove(path) {
                break tx;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        for event in events {
            // The core may already have cancelled this enumeration; sends
            // then fail, which is exactly what a real provider sees.
            let _ = tx.send(event).await;
        }
    }
}

impl FileSystem for GatedFs {
    fn name(&self) -> &str {
        "gated"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn read_dir(&self, path: &Path) -> DirStream {
        let (tx, stream) = dir_stream(path, 8);
        self.pending.lock().unwrap().insert(path.to_path_buf(), tx);
        stream
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_superseded_listings_never_replace_newer_ones() {
    let fs = Arc::new(GatedFs::default());
    let (left_root, right_root) = (PathBuf::from("/left"), PathBuf::from("/right"));
    let mut h = start(fs.clone(), &left_root, &right_root);
    fs.feed(&left_root, vec![DirEvent::Done]).await;
    let _ = h.listing(PaneId::Left).await;
    fs.feed(&right_root, vec![DirEvent::Done]).await;
    let _ = h.listing(PaneId::Right).await;

    let slow = PathBuf::from("/slow");
    let fast = PathBuf::from("/fast");
    h.handle.send(Command::Navigate {
        pane: PaneId::Left,
        path: slow.clone(),
    });
    h.handle.send(Command::Navigate {
        pane: PaneId::Left,
        path: fast.clone(),
    });
    let file = |name: &str| DirEvent::Entries(vec![Entry::new(name, EntryKind::File)]);
    fs.feed(&fast, vec![file("f"), DirEvent::Done]).await;
    let shown = h.listing(PaneId::Left).await;
    assert_eq!(shown.location, fast);

    // The slow enumeration completes afterwards; it must not be shown.
    fs.feed(&slow, vec![file("s"), DirEvent::Done]).await;
    // Use a round trip through the other pane as a barrier.
    h.handle.send(Command::Refresh {
        pane: PaneId::Right,
    });
    fs.feed(&right_root, vec![DirEvent::Done]).await;
    loop {
        match h.next().await {
            Update::Listing {
                pane: PaneId::Left,
                location,
                ..
            } => {
                panic!("stale listing for {} was published", location.display())
            }
            Update::Listing {
                pane: PaneId::Right,
                ..
            } => break,
            _ => {}
        }
    }
}
