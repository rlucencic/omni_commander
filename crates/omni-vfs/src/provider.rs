use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use tokio::sync::mpsc;

use crate::{Entry, VfsError};

/// Operations a provider supports. Callers check these instead of assuming
/// every backend behaves like a local disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Capabilities {
    /// Symbolic links are reported and can be followed.
    pub symlinks: bool,
    /// Entries carry Unix permission bits.
    pub unix_permissions: bool,
    /// Entries carry Windows attribute flags.
    pub windows_attributes: bool,
    /// Rename within the same filesystem is atomic.
    pub atomic_rename: bool,
    /// Files support random-access (seekable) reads.
    pub random_access: bool,
}

/// A filesystem provider (local disk, and later SFTP, archives, ...).
///
/// Implementations must be cheap to share (`Arc<dyn FileSystem>`) and must not
/// block the calling thread: slow work happens on background tasks.
pub trait FileSystem: Send + Sync + fmt::Debug {
    /// Short identifier for logs and diagnostics, e.g. `"local"`.
    fn name(&self) -> &str;

    fn capabilities(&self) -> Capabilities;

    /// Starts enumerating the directory at `path` and returns immediately.
    ///
    /// Entries arrive in batches through the returned stream, which ends with
    /// [`DirEvent::Done`] or [`DirEvent::Failed`]. Dropping the stream cancels
    /// the enumeration. Must be called from within a Tokio runtime.
    fn read_dir(&self, path: &Path) -> DirStream;
}

/// One step of a directory enumeration.
#[derive(Debug)]
pub enum DirEvent {
    /// More entries. Batches are never empty.
    Entries(Vec<Entry>),
    /// A problem with a single entry; the enumeration continues.
    Warning(VfsError),
    /// The enumeration finished successfully.
    Done,
    /// The directory could not be enumerated. No further events follow.
    Failed(VfsError),
}

impl DirEvent {
    /// True for the last event of a stream.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_))
    }
}

/// Receiving side of a directory enumeration. See [`FileSystem::read_dir`].
#[derive(Debug)]
pub struct DirStream {
    path: PathBuf,
    rx: mpsc::Receiver<DirEvent>,
    finished: bool,
}

/// Creates a bounded channel for a directory enumeration of `path`.
///
/// Providers push [`DirEvent`]s into the sender; when the [`DirStream`] is
/// dropped, sends fail and the provider should stop.
pub fn dir_stream(
    path: impl Into<PathBuf>,
    capacity: usize,
) -> (mpsc::Sender<DirEvent>, DirStream) {
    let (tx, rx) = mpsc::channel(capacity);
    let stream = DirStream {
        path: path.into(),
        rx,
        finished: false,
    };
    (tx, stream)
}

impl DirStream {
    /// The directory being enumerated.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Waits for the next event. Returns `None` after the terminal event.
    ///
    /// If the provider stops without sending a terminal event, this yields
    /// [`DirEvent::Failed`] with [`VfsError::Interrupted`] so that a partial
    /// listing is never mistaken for a complete one.
    pub async fn next(&mut self) -> Option<DirEvent> {
        if self.finished {
            return None;
        }
        let event = self.rx.recv().await.unwrap_or_else(|| {
            DirEvent::Failed(VfsError::Interrupted {
                path: self.path.clone(),
            })
        });
        self.finished = event.is_terminal();
        Some(event)
    }
}

/// Joins an untrusted entry `name` onto `dir`.
///
/// Fails unless `name` is exactly one normal path component, so names such as
/// `..`, `/etc`, `a/b` or `C:\x` can never escape `dir`.
pub fn child_path(dir: &Path, name: &OsStr) -> Result<PathBuf, VfsError> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component == name => Ok(dir.join(name)),
        _ => Err(VfsError::InvalidName {
            name: name.to_os_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_path_accepts_plain_names() {
        let dir = Path::new("base");
        assert_eq!(
            child_path(dir, OsStr::new("file.txt")).unwrap(),
            dir.join("file.txt")
        );
        assert_eq!(
            child_path(dir, OsStr::new(".hidden")).unwrap(),
            dir.join(".hidden")
        );
        assert_eq!(child_path(dir, OsStr::new("...")).unwrap(), dir.join("..."));
    }

    #[test]
    fn child_path_rejects_traversal() {
        let dir = Path::new("base");
        for bad in ["", ".", "..", "a/b", "/etc", "../x", "a/"] {
            assert!(
                matches!(
                    child_path(dir, OsStr::new(bad)),
                    Err(VfsError::InvalidName { .. })
                ),
                "{bad:?} should be rejected"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn child_path_rejects_windows_prefixes() {
        let dir = Path::new("base");
        for bad in [r"C:\x", r"a\b", r"\\server\share", "C:"] {
            assert!(
                child_path(dir, OsStr::new(bad)).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[tokio::test]
    async fn stream_without_terminal_event_reports_interruption() {
        let (tx, mut stream) = dir_stream("somewhere", 4);
        tx.send(DirEvent::Entries(vec![Entry::new(
            "a",
            crate::EntryKind::File,
        )]))
        .await
        .unwrap();
        drop(tx);
        assert!(matches!(stream.next().await, Some(DirEvent::Entries(_))));
        assert!(matches!(
            stream.next().await,
            Some(DirEvent::Failed(VfsError::Interrupted { .. }))
        ));
        assert!(stream.next().await.is_none());
    }
}
