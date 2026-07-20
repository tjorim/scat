//! Live multi-line tree view of the directory the scan is currently walking.
//!
//! [`ScanTreeView`] renders one line per ancestor level (scan root -> ... ->
//! the folder currently being visited), each listing that level's sibling
//! entries with the active one bracketed -- similar to windirstat's live
//! traversal display. It's built on the same [`indicatif::MultiProgress`]
//! the scan's existing single-line spinner already draws into, and is only
//! constructed for interactive (`stderr_is_tty()`) manual runs -- unattended
//! cron builds never pay for it.

use std::path::{Path, PathBuf};

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Ancestor levels shown at once; the deepest levels win once the walk goes
/// past this depth.
const MAX_VISIBLE_LEVELS: usize = 6;
/// Sibling names shown per level before windowing around the active entry.
const MAX_SIBLINGS_PER_LEVEL: usize = 8;

pub(crate) struct ScanTreeView {
    multi: MultiProgress,
    levels: Vec<ProgressBar>,
}

impl ScanTreeView {
    pub(crate) fn new(multi: MultiProgress) -> Self {
        Self {
            multi,
            levels: Vec::new(),
        }
    }

    /// Update the tree to reflect the walk currently visiting `entry_path`
    /// (a file or directory) under scan `root`.
    pub(crate) fn update(&mut self, root: &Path, entry_path: &Path) {
        let rows = tree_rows(root, entry_path);
        let visible = &rows[rows.len().saturating_sub(MAX_VISIBLE_LEVELS)..];

        while self.levels.len() > visible.len() {
            if let Some(pb) = self.levels.pop() {
                self.multi.remove(&pb);
            }
        }
        while self.levels.len() < visible.len() {
            let pb = ProgressBar::new_spinner();
            pb.set_style(
                ProgressStyle::with_template("{msg}")
                    .unwrap_or_else(|_| ProgressStyle::default_spinner()),
            );
            self.levels.push(self.multi.add(pb));
        }

        for (pb, (depth, line)) in self.levels.iter().zip(visible.iter()) {
            pb.set_message(format!("{}{line}", "  ".repeat(*depth)));
        }
    }

    /// Remove all level lines, leaving the view empty (safe to update again,
    /// though callers only do this at teardown). The caller still owns and
    /// clears the main scan spinner separately.
    pub(crate) fn clear(&mut self) {
        for pb in self.levels.drain(..) {
            self.multi.remove(&pb);
        }
    }
}

/// One display row per ancestor level from `root` down to the directory
/// containing `entry_path`, each listing that directory's sibling entries
/// with the active child bracketed. Empty when `entry_path` isn't under
/// `root` (for example the root entry itself, which has nothing to show yet).
fn tree_rows(root: &Path, entry_path: &Path) -> Vec<(usize, String)> {
    if entry_path == root {
        return Vec::new();
    }
    let Some(parent) = entry_path.parent() else {
        return Vec::new();
    };
    let Ok(relative) = parent.strip_prefix(root) else {
        return Vec::new();
    };

    // Root-to-leaf ancestor directories: [root, root/c1, root/c1/c2, ..., parent].
    let mut ancestors: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut cur = root.to_path_buf();
    for component in relative.components() {
        cur = cur.join(component);
        ancestors.push(cur.clone());
    }

    let mut rows = Vec::with_capacity(ancestors.len());
    for depth in 0..ancestors.len() {
        let active_name = if depth + 1 < ancestors.len() {
            ancestors[depth + 1].file_name()
        } else {
            entry_path.file_name()
        };
        let Some(active_name) = active_name.and_then(|n| n.to_str()) else {
            continue;
        };
        rows.push((depth, sibling_line(&ancestors[depth], active_name)));
    }
    rows
}

/// The sibling entries of `dir`, with `active_name` bracketed, windowed
/// around the active entry when there are too many to show at once.
fn sibling_line(dir: &Path, active_name: &str) -> String {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();

    let active_idx = names.iter().position(|n| n == active_name);

    let window: Vec<&str> = match active_idx {
        Some(idx) if names.len() > MAX_SIBLINGS_PER_LEVEL => {
            let half = MAX_SIBLINGS_PER_LEVEL / 2;
            let start = idx.saturating_sub(half);
            let end = (start + MAX_SIBLINGS_PER_LEVEL).min(names.len());
            let start = end.saturating_sub(MAX_SIBLINGS_PER_LEVEL);
            names[start..end].iter().map(String::as_str).collect()
        }
        _ => names.iter().map(String::as_str).collect(),
    };

    let rendered: Vec<String> = window
        .iter()
        .map(|name| {
            if *name == active_name {
                format!("[{name}]")
            } else {
                (*name).to_string()
            }
        })
        .collect();

    if active_idx.is_none() {
        // Raced by a concurrent delete, or otherwise not a listed entry of
        // its own parent; name it directly so the row isn't empty.
        format!("[{active_name}]  {}", rendered.join("  "))
    } else {
        rendered.join("  ")
    }
}

#[cfg(test)]
mod tests {
    use super::{sibling_line, tree_rows};
    use std::path::Path;

    #[test]
    fn tree_rows_empty_when_entry_is_root() {
        let root = Path::new("/scan");
        assert!(tree_rows(root, root).is_empty());
    }

    #[test]
    fn tree_rows_empty_when_entry_outside_root() {
        let root = Path::new("/scan");
        assert!(tree_rows(root, Path::new("/other/file.py")).is_empty());
    }

    #[test]
    fn tree_rows_one_row_per_ancestor_level() {
        let root = Path::new("/scan");
        let entry = Path::new("/scan/a/b/file.py");
        let rows = tree_rows(root, entry);
        // depth 0: /scan (active "a"), depth 1: /scan/a (active "b"),
        // depth 2: /scan/a/b (active "file.py").
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, 0);
        assert_eq!(rows[1].0, 1);
        assert_eq!(rows[2].0, 2);
    }

    #[test]
    fn tree_rows_top_level_file_is_one_row() {
        let root = Path::new("/scan");
        let entry = Path::new("/scan/file.py");
        let rows = tree_rows(root, entry);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, 0);
    }

    #[test]
    fn sibling_line_brackets_the_active_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "").unwrap();
        std::fs::write(dir.path().join("b.py"), "").unwrap();
        std::fs::write(dir.path().join("c.py"), "").unwrap();

        let line = sibling_line(dir.path(), "b.py");
        assert_eq!(line, "a.py  [b.py]  c.py");
    }

    #[test]
    fn sibling_line_windows_around_active_entry_when_too_many_siblings() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::write(dir.path().join(format!("f{i:02}.py")), "").unwrap();
        }

        let line = sibling_line(dir.path(), "f19.py");
        assert!(line.contains("[f19.py]"), "line: {line}");
        // Windowed to MAX_SIBLINGS_PER_LEVEL entries, not all 20.
        assert_eq!(line.split("  ").count(), super::MAX_SIBLINGS_PER_LEVEL);
    }

    #[test]
    fn sibling_line_names_active_entry_when_missing_from_listing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "").unwrap();

        let line = sibling_line(dir.path(), "ghost.py");
        assert!(line.starts_with("[ghost.py]"), "line: {line}");
    }
}
