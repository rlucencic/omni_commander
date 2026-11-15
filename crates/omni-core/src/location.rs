//! Locations as the user sees them: typed paths and breadcrumbs.
//!
//! Pure functions, so the rules can be tested without a filesystem.

use std::path::{Component, MAIN_SEPARATOR, MAIN_SEPARATOR_STR, Path, PathBuf, is_separator};

/// One clickable segment of a location bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    /// The location this segment leads to.
    pub path: PathBuf,
}

/// Splits `path` into breadcrumbs, root first. A Windows drive or UNC prefix
/// and the root that follows it form a single crumb (`C:\`).
pub fn breadcrumbs(path: &Path) -> Vec<Crumb> {
    let mut crumbs: Vec<Crumb> = Vec::new();
    let mut current = PathBuf::new();
    let mut after_prefix = false;
    for component in path.components() {
        current.push(component.as_os_str());
        match component {
            Component::RootDir if after_prefix => {
                let last = crumbs.last_mut().expect("a prefix crumb precedes the root");
                last.label.push(MAIN_SEPARATOR);
                last.path.clone_from(&current);
            }
            Component::RootDir => crumbs.push(Crumb {
                label: MAIN_SEPARATOR_STR.to_string(),
                path: current.clone(),
            }),
            other => crumbs.push(Crumb {
                label: other.as_os_str().to_string_lossy().into_owned(),
                path: current.clone(),
            }),
        }
        after_prefix = matches!(component, Component::Prefix(_));
    }
    crumbs
}

/// Resolves text typed into the location bar to an absolute, normalized path.
///
/// * Surrounding whitespace and one pair of matching quotes are ignored, so
///   pasted `"/some path"` works.
/// * `~` and `~/…` expand to `home` (`~user` is not supported and is taken
///   literally).
/// * Relative input is resolved against `current`.
///
/// Returns `None` for blank input.
pub fn resolve_input(input: &str, current: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let input = strip_quotes(input.trim());
    if input.is_empty() {
        return None;
    }
    let expanded = match (input.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(is_separator) => {
            home.join(rest.trim_start_matches(is_separator))
        }
        _ => PathBuf::from(input),
    };
    // `join` keeps `expanded` alone when it is absolute.
    Some(normalize(&current.join(expanded)))
}

fn strip_quotes(s: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(quote).and_then(|r| r.strip_suffix(quote)) {
            return inner.trim();
        }
    }
    s
}

/// Removes `.` components and resolves `..` lexically, like a shell's `cd`,
/// never climbing above the root. A drive prefix without a root (`C:`) gets
/// one, since a file manager has no per-drive current directory.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(component.as_os_str());
                }
            }
            Component::Prefix(_) => {
                out.push(component.as_os_str());
                if components.peek() != Some(&Component::RootDir) {
                    out.push(MAIN_SEPARATOR_STR);
                }
            }
            Component::RootDir | Component::Normal(_) => out.push(component.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(path: &str) -> Vec<String> {
        breadcrumbs(Path::new(path))
            .into_iter()
            .map(|c| c.label)
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn unix_breadcrumbs() {
        assert_eq!(labels("/"), ["/"]);
        assert_eq!(labels("/home/user"), ["/", "home", "user"]);
        let crumbs = breadcrumbs(Path::new("/home/user"));
        assert_eq!(crumbs[1].path, Path::new("/home"));
        assert_eq!(crumbs[2].path, Path::new("/home/user"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_breadcrumbs() {
        assert_eq!(labels(r"C:\Users\me"), [r"C:\", "Users", "me"]);
        assert_eq!(
            breadcrumbs(Path::new(r"C:\Users"))[0].path,
            Path::new(r"C:\")
        );
        assert_eq!(labels(r"\\server\share\dir"), [r"\\server\share\", "dir"]);
    }

    #[cfg(unix)]
    #[test]
    fn resolves_typed_locations() {
        let cwd = Path::new("/home/user/projects");
        let home = Some(Path::new("/home/user"));
        let resolve = |s| resolve_input(s, cwd, home).map(|p| p.display().to_string());

        assert_eq!(resolve("/etc"), Some("/etc".into()));
        assert_eq!(resolve("  /etc/  "), Some("/etc".into()));
        assert_eq!(resolve("src"), Some("/home/user/projects/src".into()));
        assert_eq!(resolve("../docs/./a"), Some("/home/user/docs/a".into()));
        assert_eq!(resolve("~"), Some("/home/user".into()));
        assert_eq!(resolve("~/Music"), Some("/home/user/Music".into()));
        assert_eq!(resolve("~other"), Some("/home/user/projects/~other".into()));
        assert_eq!(
            resolve("\"/tmp/with space\""),
            Some("/tmp/with space".into())
        );
        assert_eq!(resolve("/../../.."), Some("/".into()));
        assert_eq!(resolve("   "), None);
        // Without a known home directory, `~` is just a name.
        assert_eq!(
            resolve_input("~", cwd, None).unwrap(),
            Path::new("/home/user/projects/~")
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolves_windows_locations() {
        let cwd = Path::new(r"C:\Users\me");
        assert_eq!(resolve_input("D:", cwd, None).unwrap(), Path::new(r"D:\"));
        assert_eq!(
            resolve_input(r"..\you", cwd, None).unwrap(),
            Path::new(r"C:\Users\you")
        );
        assert_eq!(
            resolve_input("/Windows", cwd, None).unwrap(),
            Path::new(r"C:\Windows")
        );
    }

    #[test]
    fn normalize_keeps_leading_parent_of_relative_paths() {
        assert_eq!(normalize(Path::new("../a/./b/..")), Path::new("../a"));
    }
}
