//! Ordering of directory entries.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::time::SystemTime;

use omni_vfs::Entry;

/// Column a listing is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortKey {
    #[default]
    Name,
    Extension,
    Size,
    Modified,
}

/// Sort column plus direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SortSpec {
    pub key: SortKey,
    pub descending: bool,
}

impl SortSpec {
    /// The spec after the user picks `key`: picking the active key reverses
    /// the direction, picking another key starts with that key's natural
    /// direction (largest / newest first for size and date).
    pub fn toggled(self, key: SortKey) -> Self {
        if self.key == key {
            Self {
                key,
                descending: !self.descending,
            }
        } else {
            Self {
                key,
                descending: matches!(key, SortKey::Size | SortKey::Modified),
            }
        }
    }
}

/// Natural-sort keys of a listing's names, computed once and reused by every
/// sort of the same entries.
///
/// All keys live in one contiguous arena: sorting compares keys far more
/// often than it touches entries, so keeping them dense is what makes
/// sorting large directories fast.
#[derive(Debug, Default)]
pub struct NameKeys {
    arena: Vec<u8>,
    spans: Vec<(u32, u32)>,
}

impl NameKeys {
    pub fn new(entries: &[Entry]) -> Self {
        let mut keys = Self {
            arena: Vec::with_capacity(entries.len() * 24),
            spans: Vec::with_capacity(entries.len()),
        };
        for entry in entries {
            keys.push(&entry.name.to_string_lossy());
        }
        keys
    }

    /// Keys of the extensions (see [`split_extension`]); directories have none.
    fn extensions(entries: &[Entry]) -> Self {
        let mut keys = Self::default();
        for entry in entries {
            let name = entry.name.to_string_lossy();
            let ext = if entry.is_dir_like() {
                ""
            } else {
                split_extension(&name).1
            };
            keys.push(ext);
        }
        keys
    }

    fn push(&mut self, text: &str) {
        let start = self.arena.len();
        push_natural_key(text, &mut self.arena);
        let to_u32 = |n: usize| u32::try_from(n).expect("sort keys exceed 4 GiB");
        self.spans
            .push((to_u32(start), to_u32(self.arena.len() - start)));
    }

    fn get(&self, index: u32) -> &[u8] {
        let (start, len) = self.spans[index as usize];
        &self.arena[start as usize..(start + len) as usize]
    }
}

/// Returns the indices of `entries` in display order. `keys` must have been
/// built from the same `entries`.
///
/// Directories (including links to directories) always come first, whatever
/// the direction; ties are broken by name and finally by raw bytes so the
/// order is total and deterministic.
pub fn sorted_order(entries: &[Entry], keys: &NameKeys, spec: SortSpec) -> Vec<u32> {
    debug_assert_eq!(entries.len(), keys.spans.len());

    /// What the sort moves around: 24 bytes, so comparisons stay in cache.
    /// `(primary, secondary)` decide almost every comparison; the key arena
    /// is consulted only when both tie.
    struct Record {
        is_file: bool,
        primary: u64,
        secondary: u64,
        index: u32,
    }

    let ext_ranks = (spec.key == SortKey::Extension).then(|| extension_ranks(entries));
    let mut records: Vec<Record> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let index = u32::try_from(i).expect("listing exceeds u32::MAX entries");
            let name = keys.get(index);
            let (primary, secondary) = match spec.key {
                // Bytes 0..8 and 8..16 of the key.
                SortKey::Name => (key_prefix(name), key_prefix(name.get(8..).unwrap_or(&[]))),
                SortKey::Extension => {
                    let ranks = ext_ranks.as_ref().expect("computed for extension sort");
                    (u64::from(ranks[i]), key_prefix(name))
                }
                SortKey::Size => (
                    entry.size.map_or(0, |s| s.saturating_add(1)),
                    key_prefix(name),
                ),
                SortKey::Modified => (time_rank(entry.modified), key_prefix(name)),
            };
            Record {
                is_file: !entry.is_dir_like(),
                primary,
                secondary,
                index,
            }
        })
        .collect();

    records.sort_unstable_by(|a, b| {
        // `false < true`: directories first, in both directions.
        a.is_file.cmp(&b.is_file).then_with(|| {
            let ordering = (a.primary, a.secondary)
                .cmp(&(b.primary, b.secondary))
                .then_with(|| keys.get(a.index).cmp(keys.get(b.index)))
                .then_with(|| {
                    let (x, y) = (
                        &entries[a.index as usize].name,
                        &entries[b.index as usize].name,
                    );
                    x.as_encoded_bytes().cmp(y.as_encoded_bytes())
                });
            if spec.descending {
                ordering.reverse()
            } else {
                ordering
            }
        })
    });
    records.into_iter().map(|r| r.index).collect()
}

/// Dense ranks of the entries' extensions: equal extensions get equal ranks,
/// and rank order is natural order. Directories have the empty extension.
fn extension_ranks(entries: &[Entry]) -> Vec<u32> {
    let keys = NameKeys::extensions(entries);
    let count = u32::try_from(entries.len()).expect("listing exceeds u32::MAX entries");
    let mut ranks: HashMap<&[u8], u32> = HashMap::new();
    for i in 0..count {
        ranks.entry(keys.get(i)).or_insert(0);
    }
    let mut distinct: Vec<&[u8]> = ranks.keys().copied().collect();
    distinct.sort_unstable();
    for (rank, key) in distinct.into_iter().enumerate() {
        ranks.insert(key, u32::try_from(rank).expect("fewer ranks than entries"));
    }
    (0..count).map(|i| ranks[keys.get(i)]).collect()
}

/// First eight key bytes as a big-endian integer, zero-padded: comparing
/// prefixes is consistent with comparing whole keys, just coarser.
fn key_prefix(key: &[u8]) -> u64 {
    let mut prefix = [0u8; 8];
    let len = key.len().min(8);
    prefix[..len].copy_from_slice(&key[..len]);
    u64::from_be_bytes(prefix)
}

/// Orders unknown times first, then by time; times before 1970 tie.
fn time_rank(time: Option<SystemTime>) -> u64 {
    match time {
        None => 0,
        Some(t) => t.duration_since(SystemTime::UNIX_EPOCH).map_or(1, |d| {
            u64::try_from(d.as_nanos())
                .unwrap_or(u64::MAX - 1)
                .saturating_add(1)
        }),
    }
}

/// Splits a (display) name into stem and extension, without the dot. A
/// leading dot does not start an extension (`.bashrc` has none), and neither
/// does a trailing one.
pub fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => (&name[..dot], &name[dot + 1..]),
        _ => (name, ""),
    }
}

/// Case-insensitive "natural" comparison: digit runs compare by numeric
/// value, so `file2` sorts before `file10`. Equal keys fall back to the raw
/// bytes, so distinct names never compare equal.
///
/// This is a locale-independent approximation; full Unicode collation is a
/// later refinement.
pub fn natural_cmp(a: &OsStr, b: &OsStr) -> Ordering {
    let (mut ka, mut kb) = (Vec::new(), Vec::new());
    push_natural_key(&a.to_string_lossy(), &mut ka);
    push_natural_key(&b.to_string_lossy(), &mut kb);
    ka.cmp(&kb)
        .then_with(|| a.as_encoded_bytes().cmp(b.as_encoded_bytes()))
}

/// Appends a byte key whose lexicographic order is the natural order of
/// `text`:
///
/// * other characters become their lower-case UTF-8 bytes;
/// * a digit run becomes `[length marker][digits without leading zeros]
///   [number of leading zeros]`. The marker orders by digit count and always
///   lies in `b'0'..=b'9'`, so a number compares against a non-digit exactly
///   like its first digit would.
fn push_natural_key(text: &str, out: &mut Vec<u8>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &bytes[start..i];
            let zeros = run.iter().take_while(|&&c| c == b'0').count();
            let digits = &run[zeros..];
            // Lengths up to 8 encode as b'0' + len; longer runs as b'9'
            // followed by the length, which keeps the marker order-preserving.
            if digits.len() <= 8 {
                out.push(b'0' + digits.len() as u8);
            } else {
                out.push(b'9');
                let len = u32::try_from(digits.len()).unwrap_or(u32::MAX);
                out.extend_from_slice(&len.to_be_bytes());
            }
            out.extend_from_slice(digits);
            out.push(u8::try_from(zeros).unwrap_or(u8::MAX));
        } else if b.is_ascii() {
            out.push(b.to_ascii_lowercase());
            i += 1;
        } else {
            let c = text[i..]
                .chars()
                .next()
                .expect("index is at a char boundary");
            for lower in c.to_lowercase() {
                out.extend_from_slice(lower.encode_utf8(&mut [0; 4]).as_bytes());
            }
            i += c.len_utf8();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_vfs::{EntryKind, LinkTarget};
    use std::time::{Duration, SystemTime};

    fn file(name: &str, size: u64, age_secs: u64) -> Entry {
        let mut e = Entry::new(name, EntryKind::File);
        e.size = Some(size);
        e.modified = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs));
        e
    }

    fn dir(name: &str) -> Entry {
        Entry::new(name, EntryKind::Directory)
    }

    fn names(entries: &[Entry], spec: SortSpec) -> Vec<String> {
        sorted_order(entries, &NameKeys::new(entries), spec)
            .into_iter()
            .map(|i| entries[i as usize].name.to_string_lossy().into_owned())
            .collect()
    }

    fn spec(key: SortKey, descending: bool) -> SortSpec {
        SortSpec { key, descending }
    }

    #[test]
    fn natural_order_handles_numbers_and_case() {
        let mut v = vec!["file10", "File2", "file1", "file02", "a", "B", "file2"];
        v.sort_by(|a, b| natural_cmp(OsStr::new(a), OsStr::new(b)));
        assert_eq!(v, ["a", "B", "file1", "File2", "file2", "file02", "file10"]);
    }

    #[test]
    fn natural_order_handles_huge_numbers_and_unicode() {
        let long = "x99999999999999999999999999";
        let longer = "x100000000000000000000000000";
        assert_eq!(
            natural_cmp(OsStr::new(long), OsStr::new(longer)),
            Ordering::Less
        );
        assert_eq!(
            natural_cmp(OsStr::new("Ärger"), OsStr::new("ärger2")),
            Ordering::Less
        );
        assert_eq!(
            natural_cmp(OsStr::new("Žaba"), OsStr::new("žaba")),
            Ordering::Less
        );
        assert_ne!(
            natural_cmp(OsStr::new("Žaba"), OsStr::new("žaba")),
            Ordering::Equal
        );
    }

    #[test]
    fn directories_come_first_in_both_directions() {
        let entries = vec![
            file("b.txt", 1, 0),
            dir("zdir"),
            file("a.txt", 2, 0),
            dir("adir"),
        ];
        assert_eq!(
            names(&entries, spec(SortKey::Name, false)),
            ["adir", "zdir", "a.txt", "b.txt"]
        );
        assert_eq!(
            names(&entries, spec(SortKey::Name, true)),
            ["zdir", "adir", "b.txt", "a.txt"]
        );
    }

    #[test]
    fn links_to_directories_sort_with_directories() {
        let entries = vec![
            file("a", 1, 0),
            Entry::new("link", EntryKind::Symlink(LinkTarget::Directory)),
            Entry::new("broken", EntryKind::Symlink(LinkTarget::Broken)),
        ];
        assert_eq!(
            names(&entries, SortSpec::default()),
            ["link", "a", "broken"]
        );
    }

    #[test]
    fn sorts_by_size_date_and_extension() {
        let entries = vec![
            file("small.rs", 10, 50),
            file("big.txt", 1000, 10),
            file("mid.md", 100, 30),
            file("noext", 100, 20),
        ];
        assert_eq!(
            names(&entries, spec(SortKey::Size, true)),
            ["big.txt", "noext", "mid.md", "small.rs"]
        );
        assert_eq!(
            names(&entries, spec(SortKey::Modified, true)),
            ["big.txt", "noext", "mid.md", "small.rs"]
        );
        assert_eq!(
            names(&entries, spec(SortKey::Extension, false)),
            ["noext", "mid.md", "small.rs", "big.txt"]
        );
    }

    #[test]
    fn toggling_sort_keys() {
        let s = SortSpec::default();
        assert_eq!(s.toggled(SortKey::Name), spec(SortKey::Name, true));
        assert_eq!(s.toggled(SortKey::Size), spec(SortKey::Size, true));
        assert_eq!(
            s.toggled(SortKey::Extension),
            spec(SortKey::Extension, false)
        );
        assert_eq!(
            s.toggled(SortKey::Size).toggled(SortKey::Size),
            spec(SortKey::Size, false)
        );
    }

    #[test]
    fn extension_rules() {
        let split = split_extension;
        assert_eq!(split("archive.tar.gz"), ("archive.tar", "gz"));
        assert_eq!(split(".bashrc"), (".bashrc", ""));
        assert_eq!(split(".config.toml"), (".config", "toml"));
        assert_eq!(split("trailing."), ("trailing.", ""));
        assert_eq!(split("plain"), ("plain", ""));
    }

    /// The straightforward character-by-character definition of natural
    /// order; the key encoding must agree with it.
    fn natural_cmp_str(a: &str, b: &str) -> Ordering {
        let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
        loop {
            match (a.first(), b.first()) {
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                    let (na, ra) = split_digits(a);
                    let (nb, rb) = split_digits(b);
                    let ordering = cmp_digit_runs(na, nb);
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                    (a, b) = (ra, rb);
                }
                (Some(x), Some(y)) if x.is_ascii() && y.is_ascii() => {
                    let ordering = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                    (a, b) = (&a[1..], &b[1..]);
                }
                _ => {
                    // At least one side starts a non-ASCII character: compare
                    // one full character, lower-cased.
                    let ca = first_char(a);
                    let cb = first_char(b);
                    let ordering = ca.to_lowercase().cmp(cb.to_lowercase());
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                    (a, b) = (&a[ca.len_utf8()..], &b[cb.len_utf8()..]);
                }
            }
        }
    }

    fn first_char(bytes: &[u8]) -> char {
        // `bytes` always starts at a character boundary of a valid `str`.
        let len = match bytes[0] {
            b if b < 0x80 => 1,
            b if b >= 0xF0 => 4,
            b if b >= 0xE0 => 3,
            _ => 2,
        };
        std::str::from_utf8(&bytes[..len.min(bytes.len())])
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or(char::REPLACEMENT_CHARACTER)
    }

    fn split_digits(bytes: &[u8]) -> (&[u8], &[u8]) {
        let end = bytes
            .iter()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(bytes.len());
        bytes.split_at(end)
    }

    /// Compares two runs of ASCII digits by numeric value without parsing (no
    /// overflow for absurdly long runs). Equal values with different leading
    /// zeros compare by run length so the result stays total.
    fn cmp_digit_runs(a: &[u8], b: &[u8]) -> Ordering {
        let trim = |s: &[u8]| -> usize { s.iter().take_while(|&&c| c == b'0').count() };
        let (ta, tb) = (&a[trim(a)..], &b[trim(b)..]);
        ta.len()
            .cmp(&tb.len())
            .then_with(|| ta.cmp(tb))
            .then_with(|| a.len().cmp(&b.len()))
    }

    #[test]
    fn keys_agree_with_reference_comparison() {
        let alphabet: Vec<char> = "a0b19Z_-. éÉž".chars().collect();
        let mut seed = 7u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 33) as usize
        };
        let mut random_name = || -> String {
            let len = next() % 12;
            (0..len)
                .map(|_| alphabet[next() % alphabet.len()])
                .collect()
        };
        for _ in 0..20_000 {
            let (a, b) = (random_name(), random_name());
            let (mut ka, mut kb) = (Vec::new(), Vec::new());
            push_natural_key(&a, &mut ka);
            push_natural_key(&b, &mut kb);
            assert_eq!(ka.cmp(&kb), natural_cmp_str(&a, &b), "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn long_numbers_and_leading_zeros() {
        let mut v = vec!["n123456789012", "n99999999", "n007", "n7", "n0", "n00"];
        v.sort_by(|a, b| natural_cmp(OsStr::new(a), OsStr::new(b)));
        assert_eq!(v, ["n0", "n00", "n7", "n007", "n99999999", "n123456789012"]);
    }
}
