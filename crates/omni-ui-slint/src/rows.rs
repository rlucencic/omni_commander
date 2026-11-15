//! Slint model over an immutable [`Listing`] snapshot.
//!
//! Rows are formatted lazily: Slint only asks for the rows the `ListView`
//! actually instantiates, so a directory with 100 000 entries costs one
//! `Arc` swap here, not 100 000 UI objects.

use std::cell::RefCell;
use std::sync::Arc;

use omni_core::format::{TimeFormatter, format_attributes, format_size};
use omni_core::sort::split_extension;
use omni_core::{Listing, Row, SnapshotId, SortSpec};
use omni_vfs::{EntryKind, LinkTarget};
use slint::{Model, ModelNotify, ModelTracker, SharedString};

use crate::{EntryIcon, FileRow};

pub struct RowsModel {
    listing: RefCell<Arc<Listing>>,
    time: TimeFormatter,
    notify: ModelNotify,
}

impl RowsModel {
    pub fn new(time: TimeFormatter) -> Self {
        Self {
            listing: RefCell::new(Arc::new(Listing::new(
                Vec::new(),
                SortSpec::default(),
                false,
            ))),
            time,
            notify: ModelNotify::default(),
        }
    }

    /// Replaces all rows with `listing`.
    pub fn set_listing(&self, listing: Arc<Listing>) {
        // The borrow must end before `reset`, which may call back into us.
        *self.listing.borrow_mut() = listing;
        self.notify.reset();
    }

    /// Id of the snapshot currently displayed; rows reported back to the
    /// core are interpreted against it.
    pub fn snapshot(&self) -> SnapshotId {
        self.listing.borrow().id()
    }

    fn file_row(&self, row: Row<'_>) -> FileRow {
        let entry = match row {
            Row::Parent => {
                return FileRow {
                    name: "..".into(),
                    icon: EntryIcon::Parent,
                    ..FileRow::default()
                };
            }
            Row::Entry(entry) => entry,
        };

        let name = entry.name.to_string_lossy();
        let (stem, ext) = if entry.is_dir_like() {
            (&*name, "")
        } else {
            split_extension(&name)
        };
        let size = if entry.is_dir_like() {
            SharedString::from("<DIR>")
        } else {
            entry.size.map(format_size).unwrap_or_default().into()
        };
        let icon = match entry.kind {
            EntryKind::Directory => EntryIcon::Folder,
            EntryKind::File => EntryIcon::File,
            EntryKind::Symlink(LinkTarget::Directory) => EntryIcon::FolderLink,
            EntryKind::Symlink(LinkTarget::File | LinkTarget::Other) => EntryIcon::FileLink,
            EntryKind::Symlink(LinkTarget::Broken) => EntryIcon::BrokenLink,
            EntryKind::Other => EntryIcon::Other,
        };

        FileRow {
            name: stem.into(),
            ext: ext.into(),
            size,
            modified: entry
                .modified
                .map(|t| self.time.format(t))
                .unwrap_or_default()
                .into(),
            attributes: format_attributes(&entry.attributes).into(),
            icon,
            hidden: entry.attributes.hidden,
        }
    }
}

impl Model for RowsModel {
    type Data = FileRow;

    fn row_count(&self) -> usize {
        self.listing.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<FileRow> {
        let listing = self.listing.borrow();
        listing.row(row).map(|row| self.file_row(row))
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}
