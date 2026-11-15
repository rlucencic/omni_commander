//! Multi-selection of entries.

use omni_vfs::Entry;

/// Selected entries of a listing, by entry index. Entry indices do not
/// change when a listing is re-sorted or filtered, so neither does the
/// selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    words: Vec<u64>,
    /// Number of entries in the listing; indices at or beyond it are invalid.
    capacity: usize,
    count: usize,
}

impl Selection {
    /// An empty selection for a listing of `entries` entries.
    pub fn new(entries: usize) -> Self {
        Self {
            words: vec![0; entries.div_ceil(64)],
            capacity: entries,
            count: 0,
        }
    }

    pub fn contains(&self, index: u32) -> bool {
        let (word, bit) = split(index);
        self.words.get(word).is_some_and(|w| w & bit != 0)
    }

    /// Selects or deselects `index`; returns whether anything changed.
    /// Indices beyond the listing are ignored.
    pub fn set(&mut self, index: u32, selected: bool) -> bool {
        if index as usize >= self.capacity {
            return false;
        }
        let (word, bit) = split(index);
        let Some(w) = self.words.get_mut(word) else {
            return false;
        };
        if (*w & bit != 0) == selected {
            return false;
        }
        *w ^= bit;
        if selected {
            self.count += 1;
        } else {
            self.count -= 1;
        }
        true
    }

    pub fn toggle(&mut self, index: u32) {
        let selected = self.contains(index);
        self.set(index, !selected);
    }

    pub fn clear(&mut self) {
        self.words.fill(0);
        self.count = 0;
    }

    /// Number of selected entries.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Selected entry indices in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.words.iter().enumerate().flat_map(|(i, &word)| {
            let base = i as u32 * 64;
            (0..64u32)
                .filter(move |b| word & (1 << b) != 0)
                .map(move |b| base + b)
        })
    }
}

fn split(index: u32) -> (usize, u64) {
    ((index / 64) as usize, 1 << (index % 64))
}

/// What is selected, for the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectionSummary {
    pub dirs: usize,
    pub files: usize,
    /// Total known size of the selected non-directory entries.
    pub bytes: u64,
}

impl SelectionSummary {
    pub fn of(selection: &Selection, entries: &[Entry]) -> Self {
        selection
            .iter()
            .filter_map(|i| entries.get(i as usize))
            .fold(Self::default(), |mut summary, entry| {
                if entry.is_dir_like() {
                    summary.dirs += 1;
                } else {
                    summary.files += 1;
                    summary.bytes += entry.size.unwrap_or(0);
                }
                summary
            })
    }

    pub fn is_empty(&self) -> bool {
        self.dirs == 0 && self.files == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_vfs::EntryKind;

    #[test]
    fn set_toggle_and_count() {
        let mut s = Selection::new(130);
        assert!(s.set(0, true));
        assert!(!s.set(0, true), "already selected");
        assert!(s.set(129, true));
        s.toggle(64);
        assert_eq!(s.len(), 3);
        assert_eq!(s.iter().collect::<Vec<_>>(), [0, 64, 129]);
        s.toggle(64);
        assert!(!s.contains(64));
        assert!(!s.set(130, true), "out of range is ignored");
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.iter().count(), 0);
    }

    #[test]
    fn summary_counts_dirs_files_and_bytes() {
        let mut big = Entry::new("big", EntryKind::File);
        big.size = Some(1000);
        let entries = vec![
            Entry::new("dir", EntryKind::Directory),
            big,
            Entry::new("x", EntryKind::File),
        ];
        let mut s = Selection::new(entries.len());
        s.set(0, true);
        s.set(1, true);
        let summary = SelectionSummary::of(&s, &entries);
        assert_eq!(
            summary,
            SelectionSummary {
                dirs: 1,
                files: 1,
                bytes: 1000
            }
        );
    }
}
