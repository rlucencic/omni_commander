//! Immutable directory snapshots shared between the core and the UI.
//!
//! A [`Listing`] pairs a shared, never-mutated entry vector with a display
//! order. Re-sorting creates a new listing that shares the entries and only
//! owns a new `Vec<u32>` of indices, so the UI can keep rendering the old
//! snapshot while a new one is built and no entry is ever cloned for it.

use std::ffi::OsStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omni_vfs::Entry;

use crate::sort::{NameKeys, SortSpec, sorted_order};

/// Identifies one published listing. Commands that refer to a row carry the
/// id of the listing the row was taken from, so stale rows are detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotId(u64);

impl SnapshotId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Aggregate counts over a listing (the parent row is not counted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    /// Directories and links to directories.
    pub dirs: usize,
    /// Everything else.
    pub files: usize,
    /// Sum of the known sizes of all non-directory entries.
    pub bytes: u64,
}

/// What a row of a listing shows.
#[derive(Debug, Clone, Copy)]
pub enum Row<'a> {
    /// The synthetic `..` row leading to the parent directory.
    Parent,
    Entry(&'a Entry),
}

/// A sorted, immutable view of a directory's entries.
#[derive(Debug)]
pub struct Listing {
    id: SnapshotId,
    entries: Arc<Vec<Entry>>,
    /// Sort keys of `entries`, shared by every re-sorted listing.
    keys: Arc<NameKeys>,
    order: Vec<u32>,
    has_parent: bool,
    sort: SortSpec,
    totals: Totals,
}

impl Listing {
    /// Sorts `entries` with `sort`. With `has_parent`, row 0 is the `..` row.
    pub fn new(entries: Vec<Entry>, sort: SortSpec, has_parent: bool) -> Self {
        let totals = totals(&entries);
        let keys = NameKeys::new(&entries);
        let order = sorted_order(&entries, &keys, sort);
        Self {
            id: SnapshotId::next(),
            entries: Arc::new(entries),
            keys: Arc::new(keys),
            order,
            has_parent,
            sort,
            totals,
        }
    }

    /// A listing with no entries (but possibly a `..` row).
    pub fn empty(has_parent: bool) -> Self {
        Self::new(Vec::new(), SortSpec::default(), has_parent)
    }

    /// The same entries in a different order, under a new id.
    pub fn resorted(&self, sort: SortSpec) -> Self {
        Self {
            id: SnapshotId::next(),
            entries: Arc::clone(&self.entries),
            keys: Arc::clone(&self.keys),
            order: sorted_order(&self.entries, &self.keys, sort),
            has_parent: self.has_parent,
            sort,
            totals: self.totals,
        }
    }

    pub fn id(&self) -> SnapshotId {
        self.id
    }

    pub fn sort(&self) -> SortSpec {
        self.sort
    }

    pub fn totals(&self) -> Totals {
        self.totals
    }

    pub fn has_parent(&self) -> bool {
        self.has_parent
    }

    /// Number of rows, including the `..` row.
    pub fn len(&self) -> usize {
        self.order.len() + usize::from(self.has_parent)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of real entries (excluding `..`).
    pub fn entry_count(&self) -> usize {
        self.order.len()
    }

    pub fn row(&self, row: usize) -> Option<Row<'_>> {
        let index = match (self.has_parent, row) {
            (true, 0) => return Some(Row::Parent),
            (true, row) => row - 1,
            (false, row) => row,
        };
        self.order
            .get(index)
            .map(|&i| Row::Entry(&self.entries[i as usize]))
    }

    /// The row showing the entry called `name`, if any.
    pub fn find(&self, name: &OsStr) -> Option<usize> {
        let offset = usize::from(self.has_parent);
        self.order
            .iter()
            .position(|&i| self.entries[i as usize].name == name)
            .map(|pos| pos + offset)
    }
}

fn totals(entries: &[Entry]) -> Totals {
    entries.iter().fold(Totals::default(), |mut t, e| {
        if e.is_dir_like() {
            t.dirs += 1;
        } else {
            t.files += 1;
            t.bytes += e.size.unwrap_or(0);
        }
        t
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::SortKey;
    use omni_vfs::EntryKind;

    fn sample() -> Vec<Entry> {
        let mut a = Entry::new("a.txt", EntryKind::File);
        a.size = Some(10);
        let mut b = Entry::new("b.txt", EntryKind::File);
        b.size = Some(5);
        vec![b, Entry::new("dir", EntryKind::Directory), a]
    }

    fn row_name(l: &Listing, row: usize) -> String {
        match l.row(row) {
            Some(Row::Parent) => "..".into(),
            Some(Row::Entry(e)) => e.name.to_string_lossy().into_owned(),
            None => "<none>".into(),
        }
    }

    #[test]
    fn rows_include_parent_and_follow_sort_order() {
        let l = Listing::new(sample(), SortSpec::default(), true);
        assert_eq!(l.len(), 4);
        assert_eq!(l.entry_count(), 3);
        let rows: Vec<_> = (0..5).map(|i| row_name(&l, i)).collect();
        assert_eq!(rows, ["..", "dir", "a.txt", "b.txt", "<none>"]);
        assert_eq!(l.find(OsStr::new("b.txt")), Some(3));
        assert_eq!(l.find(OsStr::new("missing")), None);
    }

    #[test]
    fn rows_without_parent() {
        let l = Listing::new(sample(), SortSpec::default(), false);
        assert_eq!(row_name(&l, 0), "dir");
        assert_eq!(l.find(OsStr::new("dir")), Some(0));
    }

    #[test]
    fn totals_count_dirs_files_and_bytes() {
        let l = Listing::new(sample(), SortSpec::default(), true);
        assert_eq!(
            l.totals(),
            Totals {
                dirs: 1,
                files: 2,
                bytes: 15
            }
        );
    }

    #[test]
    fn resorting_shares_entries_and_changes_id() {
        let l = Listing::new(sample(), SortSpec::default(), false);
        let size = SortSpec {
            key: SortKey::Size,
            descending: true,
        };
        let r = l.resorted(size);
        assert_ne!(l.id(), r.id());
        assert!(Arc::ptr_eq(&l.entries, &r.entries));
        assert_eq!(row_name(&r, 1), "a.txt");
        assert_eq!(r.sort(), size);
    }
}
