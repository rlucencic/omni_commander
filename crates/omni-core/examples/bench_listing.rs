//! Measures listing operations on synthetic data.
//!
//! Run with: `cargo run --release -p omni-core --example bench_listing [ENTRIES]`

use std::ffi::OsStr;
use std::hint::black_box;
use std::time::{Duration, Instant, SystemTime};

use omni_core::format::{TimeFormatter, format_attributes, format_size};
use omni_core::sort::{NameKeys, sorted_order};
use omni_core::{Listing, Row, SortKey, SortSpec};
use omni_vfs::{Attributes, Entry, EntryKind};

/// Small deterministic generator so runs are comparable without a `rand`
/// dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn synthetic_entries(count: usize) -> Vec<Entry> {
    let mut rng = Lcg(42);
    let stems = [
        "IMG_2024",
        "report final",
        "Šifra-dokumenta",
        "file",
        "build.log",
        "Résumé",
        "data",
    ];
    let exts = ["jpg", "pdf", "txt", "tar.gz", "rs", "", "docx"];
    (0..count)
        .map(|i| {
            let stem = stems[rng.next() as usize % stems.len()];
            let ext = exts[rng.next() as usize % exts.len()];
            let name = if ext.is_empty() {
                format!("{stem}_{}", rng.next() % 1_000_000)
            } else {
                format!("{stem}_{}.{ext}", rng.next() % 1_000_000)
            };
            let kind = if i % 20 == 0 {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            let mut entry = Entry::new(name, kind);
            entry.size = Some(rng.next() % 10_000_000_000);
            entry.modified = Some(
                SystemTime::UNIX_EPOCH
                    + Duration::from_secs(1_500_000_000 + rng.next() % 300_000_000),
            );
            entry.attributes = Attributes {
                unix_mode: Some(0o100644),
                ..Attributes::default()
            };
            entry
        })
        .collect()
}

/// Runs `f` several times and prints the minimum and median duration; the
/// minimum is the most stable figure on a busy machine.
fn time<T>(label: &str, mut f: impl FnMut() -> T) {
    const RUNS: usize = 7;
    let mut samples: Vec<f64> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            black_box(f());
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    println!(
        "{label:<40} min {:>8.2} ms   median {:>8.2} ms",
        samples[0],
        samples[RUNS / 2]
    );
}

fn main() {
    let count = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);
    println!("{count} entries\n");

    let entries = synthetic_entries(count);
    time("clone entries (partial listing cost)", || entries.clone());
    time("build natural-sort keys", || NameKeys::new(&entries));
    let keys = NameKeys::new(&entries);
    for key in [
        SortKey::Name,
        SortKey::Extension,
        SortKey::Size,
        SortKey::Modified,
    ] {
        let spec = SortSpec {
            key,
            descending: false,
        };
        time(&format!("sort by {key:?}"), || {
            sorted_order(&entries, &keys, spec)
        });
    }
    time("Listing::new (keys + sort + totals)", || {
        Listing::new(entries.clone(), SortSpec::default(), true)
    });

    let listing = Listing::new(entries, SortSpec::default(), true);
    let last_name = match listing.row(listing.len() - 1) {
        Some(Row::Entry(e)) => e.name.clone(),
        _ => unreachable!(),
    };
    time("find last entry by name", || {
        listing.find(OsStr::new(&last_name))
    });

    let formatter = TimeFormatter::system();
    time("format 60 visible rows", || {
        for row in 1..=60 {
            if let Some(Row::Entry(e)) = listing.row(row) {
                black_box((
                    e.name.to_string_lossy(),
                    e.size.map(format_size),
                    e.modified.map(|t| formatter.format(t)),
                    format_attributes(&e.attributes),
                ));
            }
        }
    });
}
