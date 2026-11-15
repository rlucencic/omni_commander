use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Errors reported by filesystem providers.
///
/// Messages lead with the reason and end with the path: paths can be long,
/// and a truncated message should still say what went wrong.
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    #[error("no such file or directory: {}", .path.display())]
    NotFound { path: PathBuf },

    #[error("permission denied: {}", .path.display())]
    PermissionDenied { path: PathBuf },

    #[error("not a directory: {}", .path.display())]
    NotADirectory { path: PathBuf },

    /// A provider returned a name that is not a single, plain path component
    /// (for example `..` or `a/b`). Such names are refused to prevent path
    /// traversal.
    #[error("refusing unsafe entry name {:?}", .name)]
    InvalidName { name: OsString },

    /// The enumeration ended without reporting completion, e.g. because the
    /// worker died.
    #[error("listing was interrupted: {}", .path.display())]
    Interrupted { path: PathBuf },

    /// Some entries of `dir` were listed without metadata.
    #[error("metadata unavailable for {count} entries in {} ({first})", .dir.display())]
    MetadataUnavailable {
        dir: PathBuf,
        count: usize,
        #[source]
        first: Box<VfsError>,
    },

    #[error("{source}: {}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl VfsError {
    /// Classifies an I/O error that occurred while accessing `path`.
    pub fn from_io(path: impl AsRef<Path>, source: io::Error) -> Self {
        let path = path.as_ref().to_path_buf();
        match source.kind() {
            io::ErrorKind::NotFound => Self::NotFound { path },
            io::ErrorKind::PermissionDenied => Self::PermissionDenied { path },
            io::ErrorKind::NotADirectory => Self::NotADirectory { path },
            _ => Self::Io { path, source },
        }
    }

    /// True for errors that mean "the location does not exist".
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }
}
