use std::ffi::OsString;
use std::time::SystemTime;

/// A single item inside a directory listing.
///
/// For symbolic links, `size` and the timestamps describe the link *target*
/// when it could be resolved, and the link itself otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// File name exactly as reported by the provider. Not necessarily UTF-8.
    pub name: OsString,
    pub kind: EntryKind,
    /// Size in bytes, if known.
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub attributes: Attributes,
}

impl Entry {
    /// Creates an entry with no metadata besides its name and kind.
    pub fn new(name: impl Into<OsString>, kind: EntryKind) -> Self {
        Self {
            name: name.into(),
            kind,
            size: None,
            modified: None,
            created: None,
            attributes: Attributes::default(),
        }
    }

    /// True if the entry can be entered like a directory (a directory or a
    /// symbolic link that points to one).
    pub fn is_dir_like(&self) -> bool {
        self.kind.is_dir_like()
    }
}

/// What kind of object an [`Entry`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Directory,
    Symlink(LinkTarget),
    /// Sockets, FIFOs, device nodes, or anything the provider cannot classify.
    Other,
}

impl EntryKind {
    pub fn is_dir_like(self) -> bool {
        matches!(self, Self::Directory | Self::Symlink(LinkTarget::Directory))
    }
}

/// What a symbolic link resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkTarget {
    File,
    Directory,
    Other,
    /// The target does not exist or cannot be accessed.
    Broken,
}

/// Platform-dependent attributes. Fields a provider cannot supply stay `None`
/// or `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attributes {
    /// Unix `st_mode` (file type and permission bits).
    pub unix_mode: Option<u32>,
    /// Windows `FILE_ATTRIBUTE_*` flags.
    pub windows_attributes: Option<u32>,
    pub readonly: bool,
    /// Hidden according to platform conventions (dot-files on Unix, the
    /// hidden attribute on Windows, the `UF_HIDDEN` flag on macOS).
    pub hidden: bool,
}
