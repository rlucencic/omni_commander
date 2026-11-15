//! Human-readable presentation of entry metadata.
//!
//! Kept free of any UI toolkit so it can be unit-tested and reused by other
//! front ends.

use std::fmt::Write as _;
use std::time::SystemTime;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use omni_vfs::Attributes;

use crate::listing::Totals;

/// Formats a byte count with binary units: `512 B`, `1.5 KiB`, `23 MiB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    // One decimal for small values, none once the number is long enough.
    if value < 100.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Formats an integer with space-separated groups of three digits (the
/// locale-neutral SI convention): `12 345`.
pub fn format_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// Formats timestamps in a fixed time zone (normally the system's).
#[derive(Debug, Clone)]
pub struct TimeFormatter {
    tz: TimeZone,
}

impl TimeFormatter {
    /// Uses the system time zone, falling back to UTC if it cannot be found.
    pub fn system() -> Self {
        Self {
            tz: TimeZone::system(),
        }
    }

    pub fn with_time_zone(tz: TimeZone) -> Self {
        Self { tz }
    }

    /// `YYYY-MM-DD HH:MM`, or an empty string for times outside jiff's range.
    pub fn format(&self, time: SystemTime) -> String {
        let Ok(ts) = Timestamp::try_from(time) else {
            return String::new();
        };
        let dt = self.tz.to_datetime(ts);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute()
        )
    }
}

/// Formats attributes the way the platform's tools do: `drwxr-xr-x` for Unix
/// permissions, `RHSA`-style flags for Windows.
pub fn format_attributes(attributes: &Attributes) -> String {
    if let Some(mode) = attributes.unix_mode {
        return unix_mode_string(mode);
    }
    if let Some(flags) = attributes.windows_attributes {
        return windows_attribute_string(flags);
    }
    if attributes.readonly {
        "r".into()
    } else {
        String::new()
    }
}

fn unix_mode_string(mode: u32) -> String {
    let file_type = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o020000 => 'c',
        0o060000 => 'b',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(file_type);
    // (read bit, write bit, exec bit, special bit, special char when exec set)
    let triplets = [
        (0o400, 0o200, 0o100, 0o4000, 's'),
        (0o040, 0o020, 0o010, 0o2000, 's'),
        (0o004, 0o002, 0o001, 0o1000, 't'),
    ];
    for (r, w, x, special, special_char) in triplets {
        s.push(if mode & r != 0 { 'r' } else { '-' });
        s.push(if mode & w != 0 { 'w' } else { '-' });
        s.push(match (mode & x != 0, mode & special != 0) {
            (true, true) => special_char,
            (false, true) => special_char.to_ascii_uppercase(),
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    s
}

fn windows_attribute_string(flags: u32) -> String {
    const FLAGS: [(u32, char); 4] = [(0x1, 'R'), (0x2, 'H'), (0x4, 'S'), (0x20, 'A')];
    FLAGS
        .iter()
        .map(|&(bit, c)| if flags & bit != 0 { c } else { '-' })
        .collect()
}

/// One-line summary of a listing: `3 folders, 12 files, 1.2 MiB`.
pub fn format_totals(totals: &Totals) -> String {
    let mut out = String::new();
    let plural = |n: usize, one: &str, many: &str| {
        format!("{} {}", format_count(n), if n == 1 { one } else { many })
    };
    out.push_str(&plural(totals.dirs, "folder", "folders"));
    out.push_str(", ");
    out.push_str(&plural(totals.files, "file", "files"));
    if totals.files > 0 {
        let _ = write!(out, ", {}", format_size(totals.bytes));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(150 * 1024 * 1024), "150 MiB");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(format_size(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn counts() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1 000");
        assert_eq!(format_count(1234567), "1 234 567");
    }

    #[test]
    fn times_in_fixed_zone() {
        let f = TimeFormatter::with_time_zone(TimeZone::UTC);
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(f.format(t), "2023-11-14 22:13");
        let f = TimeFormatter::with_time_zone(TimeZone::fixed(jiff::tz::offset(1)));
        assert_eq!(f.format(t), "2023-11-14 23:13");
    }

    #[test]
    fn unix_modes() {
        let attrs = |mode| Attributes {
            unix_mode: Some(mode),
            ..Default::default()
        };
        assert_eq!(format_attributes(&attrs(0o040755)), "drwxr-xr-x");
        assert_eq!(format_attributes(&attrs(0o100644)), "-rw-r--r--");
        assert_eq!(format_attributes(&attrs(0o104755)), "-rwsr-xr-x");
        assert_eq!(format_attributes(&attrs(0o041777)), "drwxrwxrwt");
        assert_eq!(format_attributes(&attrs(0o101600)), "-rw------T");
    }

    #[test]
    fn windows_flags() {
        let attrs = Attributes {
            windows_attributes: Some(0x1 | 0x20),
            ..Default::default()
        };
        assert_eq!(format_attributes(&attrs), "R--A");
    }

    #[test]
    fn totals_line() {
        let t = Totals {
            dirs: 1,
            files: 1234,
            bytes: 2048,
        };
        assert_eq!(format_totals(&t), "1 folder, 1 234 files, 2.0 KiB");
        assert_eq!(format_totals(&Totals::default()), "0 folders, 0 files");
    }
}
