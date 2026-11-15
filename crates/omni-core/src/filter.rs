//! Quick filter: which entries a pane shows.

use std::ffi::OsStr;

/// A case-insensitive name filter.
///
/// Plain text matches anywhere in a name. Text containing `*` or `?` is a
/// wildcard pattern for the whole name. Alternatives are separated by `;`,
/// so `*.jpg;*.png` matches either extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    text: String,
    alternatives: Vec<Pattern>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    Contains(String),
    Wildcard(Vec<char>),
}

impl Filter {
    /// Parses `text`; returns `None` if it contains no pattern at all.
    pub fn new(text: &str) -> Option<Self> {
        let alternatives: Vec<Pattern> = text
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| {
                let lower = part.to_lowercase();
                if lower.contains(['*', '?']) {
                    Pattern::Wildcard(lower.chars().collect())
                } else {
                    Pattern::Contains(lower)
                }
            })
            .collect();
        (!alternatives.is_empty()).then(|| Self {
            text: text.to_string(),
            alternatives,
        })
    }

    /// The text the filter was created from.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn matches(&self, name: &OsStr) -> bool {
        let name = name.to_string_lossy().to_lowercase();
        self.alternatives.iter().any(|pattern| match pattern {
            Pattern::Contains(part) => name.contains(part.as_str()),
            Pattern::Wildcard(pattern) => {
                let name: Vec<char> = name.chars().collect();
                wildcard_match(pattern, &name)
            }
        })
    }
}

/// `*` matches any run of characters, `?` exactly one. Linear-time greedy
/// matching with backtracking to the last `*`.
fn wildcard_match(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut last_star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            last_star = Some((p, t));
            p += 1;
        } else if let Some((star, matched)) = last_star {
            // Let the last `*` swallow one more character and retry.
            p = star + 1;
            t = matched + 1;
            last_star = Some((star, matched + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(filter: &str, name: &str) -> bool {
        Filter::new(filter).unwrap().matches(OsStr::new(name))
    }

    #[test]
    fn plain_text_matches_anywhere_ignoring_case() {
        assert!(matches("doc", "Documents"));
        assert!(matches("MENT", "documents"));
        assert!(matches("č", "Račun.pdf"));
        assert!(!matches("xyz", "Documents"));
    }

    #[test]
    fn wildcards_match_whole_names() {
        assert!(matches("*.jpg", "Photo.JPG"));
        assert!(!matches("*.jpg", "photo.jpg.txt"));
        assert!(matches("report-?.pdf", "report-1.pdf"));
        assert!(!matches("report-?.pdf", "report-10.pdf"));
        assert!(matches("a*b*c", "aXXbYYc"));
        assert!(!matches("a*b*c", "aXXbYY"));
        assert!(matches("*", ""));
        assert!(matches("**a", "bba"));
    }

    #[test]
    fn alternatives() {
        assert!(matches("*.jpg; *.png", "x.png"));
        assert!(matches("*.jpg;notes", "my notes.txt"));
        assert!(!matches("*.jpg;*.png", "x.gif"));
    }

    #[test]
    fn blank_filters_are_none() {
        assert!(Filter::new("").is_none());
        assert!(Filter::new(" ; ;").is_none());
        assert_eq!(Filter::new(" doc ").unwrap().text(), " doc ");
    }
}
