//! Provider for the local filesystem.

use std::ffi::OsStr;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tokio::sync::mpsc;

use crate::{
    Attributes, Capabilities, DirEvent, DirStream, Entry, EntryKind, FileSystem, LinkTarget,
    VfsError, dir_stream,
};

/// Entries per batch sent to the consumer. Large enough to keep channel
/// overhead negligible, small enough that the first rows show up quickly.
const BATCH_SIZE: usize = 512;

/// Batches buffered between the enumeration thread and the consumer. Bounds
/// memory use when the consumer is slower than the disk.
const CHANNEL_CAPACITY: usize = 4;

/// The local filesystem, accessed through `std::fs` on Tokio's blocking pool.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFs;

impl LocalFs {
    pub fn new() -> Self {
        Self
    }
}

impl FileSystem for LocalFs {
    fn name(&self) -> &str {
        "local"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            symlinks: true,
            unix_permissions: cfg!(unix),
            windows_attributes: cfg!(windows),
            atomic_rename: true,
            random_access: true,
        }
    }

    fn read_dir(&self, path: &Path) -> DirStream {
        let (tx, stream) = dir_stream(path, CHANNEL_CAPACITY);
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || enumerate(&path, &tx));
        stream
    }
}

/// Runs on a blocking thread. Returns early when the receiver is dropped.
fn enumerate(dir: &Path, tx: &mpsc::Sender<DirEvent>) {
    let started = Instant::now();
    let reader = match fs::read_dir(dir) {
        Ok(reader) => reader,
        Err(err) => {
            let err = VfsError::from_io(dir, err);
            tracing::debug!(%err, "read_dir failed");
            // Ignoring a send error is fine: it only means nobody is listening.
            let _ = tx.blocking_send(DirEvent::Failed(err));
            return;
        }
    };

    let mut batch = Vec::with_capacity(BATCH_SIZE);
    let mut total = 0usize;
    let mut metadata_failures = 0usize;
    let mut first_metadata_error: Option<(PathBuf, io::Error)> = None;

    for item in reader {
        let dirent = match item {
            Ok(dirent) => dirent,
            Err(err) => {
                // The directory stream itself failed (e.g. an I/O error on a
                // dying disk). Report it and finish with what we have; std
                // ends the iteration after such an error anyway.
                tracing::warn!(dir = %dir.display(), %err, "directory iteration failed");
                if tx
                    .blocking_send(DirEvent::Warning(VfsError::from_io(dir, err)))
                    .is_err()
                {
                    return;
                }
                break;
            }
        };

        let (entry, error) = read_entry(&dirent);
        if let Some(err) = error {
            metadata_failures += 1;
            first_metadata_error.get_or_insert_with(|| (dirent.path(), err));
        }
        batch.push(entry);

        if batch.len() == BATCH_SIZE {
            total += batch.len();
            let full = std::mem::replace(&mut batch, Vec::with_capacity(BATCH_SIZE));
            if tx.blocking_send(DirEvent::Entries(full)).is_err() {
                tracing::debug!(dir = %dir.display(), total, "enumeration cancelled");
                return;
            }
        }
    }

    total += batch.len();
    if !batch.is_empty() && tx.blocking_send(DirEvent::Entries(batch)).is_err() {
        return;
    }
    if let Some((path, source)) = first_metadata_error {
        // One summary instead of one warning per entry: a directory without
        // search permission fails `stat` for every single child.
        let warning = VfsError::MetadataUnavailable {
            dir: dir.to_path_buf(),
            count: metadata_failures,
            first: Box::new(VfsError::from_io(path, source)),
        };
        if tx.blocking_send(DirEvent::Warning(warning)).is_err() {
            return;
        }
    }
    tracing::debug!(
        dir = %dir.display(),
        total,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        "enumeration finished"
    );
    let _ = tx.blocking_send(DirEvent::Done);
}

/// Builds an [`Entry`] from a directory entry. Metadata problems never drop
/// the entry; they are returned alongside it so the caller can report them.
fn read_entry(dirent: &fs::DirEntry) -> (Entry, Option<io::Error>) {
    let name = dirent.file_name();
    // `DirEntry::metadata` does not follow symlinks.
    let meta = match dirent.metadata() {
        Ok(meta) => meta,
        Err(err) => {
            let kind = dirent
                .file_type()
                .map(|ft| kind_from_file_type(&ft))
                .unwrap_or(EntryKind::Other);
            let mut entry = Entry::new(name, kind);
            entry.attributes.hidden = is_dot_file(&entry.name);
            return (entry, Some(err));
        }
    };

    if meta.file_type().is_symlink() {
        // Describe what the link points to, but keep the link's own attributes.
        return match fs::metadata(dirent.path()) {
            Ok(target) => {
                let kind = EntryKind::Symlink(link_target(&target));
                (build_entry(name, kind, &target, &meta), None)
            }
            Err(_) => {
                let kind = EntryKind::Symlink(LinkTarget::Broken);
                (build_entry(name, kind, &meta, &meta), None)
            }
        };
    }

    let kind = kind_from_file_type(&meta.file_type());
    (build_entry(name, kind, &meta, &meta), None)
}

fn build_entry(
    name: std::ffi::OsString,
    kind: EntryKind,
    data: &Metadata,
    own: &Metadata,
) -> Entry {
    let attributes = platform_attributes(&name, own);
    Entry {
        // Directory sizes reported by `stat` are allocation sizes, not
        // content sizes; showing them would be misleading.
        size: (!kind.is_dir_like()).then_some(data.len()),
        modified: data.modified().ok(),
        created: data.created().ok(),
        attributes,
        name,
        kind,
    }
}

fn kind_from_file_type(ft: &fs::FileType) -> EntryKind {
    if ft.is_dir() {
        EntryKind::Directory
    } else if ft.is_file() {
        EntryKind::File
    } else if ft.is_symlink() {
        EntryKind::Symlink(LinkTarget::Other)
    } else {
        EntryKind::Other
    }
}

fn link_target(target: &Metadata) -> LinkTarget {
    if target.is_dir() {
        LinkTarget::Directory
    } else if target.is_file() {
        LinkTarget::File
    } else {
        LinkTarget::Other
    }
}

fn is_dot_file(name: &OsStr) -> bool {
    cfg!(unix) && name.as_encoded_bytes().first() == Some(&b'.')
}

#[cfg(unix)]
fn platform_attributes(name: &OsStr, meta: &Metadata) -> Attributes {
    use std::os::unix::fs::MetadataExt;

    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut hidden = is_dot_file(name);
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        const UF_HIDDEN: u32 = 0x8000;
        hidden |= meta.st_flags() & UF_HIDDEN != 0;
    }

    Attributes {
        unix_mode: Some(meta.mode()),
        windows_attributes: None,
        readonly: meta.permissions().readonly(),
        hidden,
    }
}

#[cfg(windows)]
fn platform_attributes(_name: &OsStr, meta: &Metadata) -> Attributes {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    let flags = meta.file_attributes();
    Attributes {
        unix_mode: None,
        windows_attributes: Some(flags),
        readonly: flags & FILE_ATTRIBUTE_READONLY != 0,
        hidden: flags & FILE_ATTRIBUTE_HIDDEN != 0,
    }
}

#[cfg(not(any(unix, windows)))]
fn platform_attributes(_name: &OsStr, meta: &Metadata) -> Attributes {
    Attributes {
        readonly: meta.permissions().readonly(),
        ..Attributes::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Listing {
        entries: Vec<Entry>,
        warnings: Vec<VfsError>,
        batches: usize,
        result: Result<(), VfsError>,
    }

    async fn list(path: &Path) -> Listing {
        let mut stream = LocalFs.read_dir(path);
        let mut listing = Listing {
            entries: Vec::new(),
            warnings: Vec::new(),
            batches: 0,
            result: Ok(()),
        };
        while let Some(event) = stream.next().await {
            match event {
                DirEvent::Entries(batch) => {
                    assert!(!batch.is_empty());
                    listing.batches += 1;
                    listing.entries.extend(batch);
                }
                DirEvent::Warning(err) => listing.warnings.push(err),
                DirEvent::Done => {}
                DirEvent::Failed(err) => listing.result = Err(err),
            }
        }
        listing.entries.sort_by(|a, b| a.name.cmp(&b.name));
        listing
    }

    fn find<'a>(entries: &'a [Entry], name: &str) -> &'a Entry {
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} not listed"))
    }

    #[tokio::test]
    async fn lists_files_and_directories_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join(".hidden"), b"").unwrap();

        let listing = list(dir.path()).await;
        listing.result.unwrap();
        assert!(listing.warnings.is_empty());
        assert_eq!(listing.entries.len(), 3);

        let file = find(&listing.entries, "a.txt");
        assert_eq!(file.kind, EntryKind::File);
        assert_eq!(file.size, Some(5));
        assert!(file.modified.is_some());
        assert!(!file.attributes.hidden);

        let sub = find(&listing.entries, "sub");
        assert_eq!(sub.kind, EntryKind::Directory);
        assert_eq!(sub.size, None);

        let hidden = find(&listing.entries, ".hidden");
        assert_eq!(hidden.attributes.hidden, cfg!(unix));
    }

    #[tokio::test]
    async fn large_directories_arrive_in_several_batches() {
        let dir = tempfile::tempdir().unwrap();
        let count = BATCH_SIZE * 2 + 7;
        for i in 0..count {
            fs::write(dir.path().join(format!("f{i}")), b"").unwrap();
        }
        let listing = list(dir.path()).await;
        listing.result.unwrap();
        assert_eq!(listing.entries.len(), count);
        assert_eq!(listing.batches, 3);
    }

    #[tokio::test]
    async fn missing_directory_fails_with_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let listing = list(&dir.path().join("nope")).await;
        assert!(matches!(listing.result, Err(VfsError::NotFound { .. })));
    }

    #[tokio::test]
    async fn listing_a_file_fails_with_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, b"x").unwrap();
        let listing = list(&file).await;
        assert!(
            matches!(listing.result, Err(VfsError::NotADirectory { .. })),
            "{:?}",
            listing.result
        );
    }

    #[tokio::test]
    async fn dropping_the_stream_cancels_enumeration() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..BATCH_SIZE * (CHANNEL_CAPACITY + 4) {
            fs::write(dir.path().join(format!("f{i}")), b"").unwrap();
        }
        let mut stream = LocalFs.read_dir(dir.path());
        assert!(matches!(stream.next().await, Some(DirEvent::Entries(_))));
        // The producer is now blocked on a full channel; dropping the stream
        // must make its next send fail so the blocking thread exits. If it
        // did not, the runtime shutdown at the end of the test would hang.
        drop(stream);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_report_their_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real_dir")).unwrap();
        fs::write(dir.path().join("real_file"), b"12345678").unwrap();
        symlink("real_dir", dir.path().join("link_dir")).unwrap();
        symlink("real_file", dir.path().join("link_file")).unwrap();
        symlink("missing", dir.path().join("link_broken")).unwrap();

        let listing = list(dir.path()).await;
        listing.result.unwrap();

        let link_dir = find(&listing.entries, "link_dir");
        assert_eq!(link_dir.kind, EntryKind::Symlink(LinkTarget::Directory));
        assert!(link_dir.is_dir_like());

        let link_file = find(&listing.entries, "link_file");
        assert_eq!(link_file.kind, EntryKind::Symlink(LinkTarget::File));
        assert_eq!(link_file.size, Some(8), "size should come from the target");

        let broken = find(&listing.entries, "link_broken");
        assert_eq!(broken.kind, EntryKind::Symlink(LinkTarget::Broken));
        assert!(!broken.is_dir_like());
    }

    /// Linux filesystems accept arbitrary bytes in names; they must survive
    /// the round trip unchanged.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn non_utf8_names_are_preserved() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let raw = OsString::from_vec(vec![b'b', b'a', b'd', 0xff, 0xfe]);
        fs::write(dir.path().join(&raw), b"").unwrap();

        let listing = list(dir.path()).await;
        listing.result.unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, raw);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_directory_fails_with_permission_denied() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            // Running as root (e.g. in some containers): permissions are not enforced.
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }
        let listing = list(&locked).await;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            listing.result,
            Err(VfsError::PermissionDenied { .. })
        ));
    }

    /// A directory that can be read but not searched (`r--`) lists names but
    /// cannot `stat` its children. The names must still be listed.
    #[cfg(unix)]
    #[tokio::test]
    async fn entries_without_metadata_are_kept_and_reported_once() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let limited = dir.path().join("limited");
        fs::create_dir(&limited).unwrap();
        for name in ["one", "two", "three"] {
            fs::write(limited.join(name), b"x").unwrap();
        }
        fs::set_permissions(&limited, fs::Permissions::from_mode(0o444)).unwrap();
        if fs::metadata(limited.join("one")).is_ok() {
            fs::set_permissions(&limited, fs::Permissions::from_mode(0o755)).unwrap();
            return; // root
        }
        let listing = list(&limited).await;
        fs::set_permissions(&limited, fs::Permissions::from_mode(0o755)).unwrap();

        listing.result.unwrap();
        assert_eq!(listing.entries.len(), 3);
        assert!(listing.entries.iter().all(|e| e.size.is_none()));
        assert_eq!(listing.warnings.len(), 1);
        assert!(matches!(
            listing.warnings[0],
            VfsError::MetadataUnavailable { count: 3, .. }
        ));
    }
}
