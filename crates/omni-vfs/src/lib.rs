//! Virtual filesystem (VFS) abstraction for OmniCommander.
//!
//! The application talks to every storage backend through the [`FileSystem`]
//! trait. A provider describes what it can do via [`Capabilities`], so callers
//! never assume that every backend supports the same operations.
//!
//! Design rules:
//!
//! * Names and paths keep their native representation ([`std::ffi::OsString`],
//!   [`std::path::PathBuf`]); nothing here assumes UTF-8.
//! * Long-running work never blocks the caller. Directory enumeration streams
//!   batches of entries through a bounded channel ([`DirStream`]); dropping the
//!   stream cancels the enumeration.
//! * Names coming from a provider are untrusted. Use [`child_path`] to turn an
//!   entry name into a path; it rejects names that would escape the directory.
//!
//! Currently implemented providers: [`LocalFs`].

mod entry;
mod error;
mod local;
mod provider;

pub use entry::{Attributes, Entry, EntryKind, LinkTarget};
pub use error::VfsError;
pub use local::LocalFs;
pub use provider::{Capabilities, DirEvent, DirStream, FileSystem, child_path, dir_stream};
