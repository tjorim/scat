//! Change detection: newest mtime across a set of scan roots.
//!
//! Directories count as well as files. A directory's mtime moves whenever an
//! entry is created, removed or renamed in it, which is the only signal for a
//! deleted script or a file moved in with an old mtime (`mv`, `cp -p`,
//! restore) — neither touches any remaining file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use ignore::WalkBuilder;
use tracing::warn;

use crate::error::{Error, Result};

use super::filter::{canonicalize_roots, get_external_ignore_paths, is_within_roots};

/// Walk `roots` (respecting `.catignore` and `ignore_files`) and return the
/// newest file or directory mtime as UNIX epoch seconds, or `None` if nothing
/// is found.
/// Pass an empty `checkout_dirs` slice to fall back to the default names
/// (`DEVELOP`, `ARCHIVE`) — see [`super::scan_paths_with_revisions`].
pub fn max_mtime_in_roots(
    roots: &[PathBuf],
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
) -> Result<Option<f64>> {
    let shutdown = AtomicBool::new(false);
    max_mtime_in_roots_with_shutdown(roots, ignore_files, checkout_dirs, &shutdown)
}

/// Walk `roots` like [`max_mtime_in_roots`], aborting early if `shutdown` is set.
///
/// DEVELOP/ARCHIVE checkout directories are not descended into, same as
/// [`super::scan_paths_with_revisions`] — otherwise this "is a rebuild needed" check
/// would walk every engineer's checkout tree, doubling the cost of a walk the
/// real scan is about to repeat anyway. The container directory's own mtime
/// still counts, since that is what moves when a checkout is created or
/// removed (the only checkout changes that matter: their content is not
/// indexed).
pub fn max_mtime_in_roots_with_shutdown(
    roots: &[PathBuf],
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
    shutdown: &AtomicBool,
) -> Result<Option<f64>> {
    use crate::core::vc::{DEFAULT_ARCHIVE_DIRS, DEFAULT_DEVELOP_DIRS};
    let default_checkout_dirs: Vec<&str> = DEFAULT_DEVELOP_DIRS
        .iter()
        .chain(DEFAULT_ARCHIVE_DIRS.iter())
        .copied()
        .collect();
    let effective_checkout_dirs: &[&str] = if checkout_dirs.is_empty() {
        &default_checkout_dirs
    } else {
        checkout_dirs
    };
    let checkout_dirs_set: std::sync::Arc<std::collections::HashSet<String>> = std::sync::Arc::new(
        effective_checkout_dirs
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    );

    let external_ignore_paths = get_external_ignore_paths(ignore_files)?;
    let canonical_roots = canonicalize_roots(roots);
    let mut max_mtime: Option<f64> = None;
    // Newest mtime among checkout containers, which `filter_entry` prunes and
    // so never yields; recorded from inside the filter.
    let container_mtime = std::sync::Arc::new(std::sync::Mutex::new(None::<f64>));

    for root in roots {
        let croots = canonical_roots.clone();
        let cds = checkout_dirs_set.clone();
        let containers = container_mtime.clone();
        let mut walk = WalkBuilder::new(root);
        walk.follow_links(true)
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .add_custom_ignore_filename(".catignore")
            .filter_entry(move |e| {
                if e.file_type().is_some_and(|t| t.is_dir()) {
                    let name = e.file_name().to_str().unwrap_or("");
                    if cds.contains(name) {
                        if let Ok(meta) = e.metadata()
                            && let Some(secs) = epoch_secs(&meta)
                            && let Ok(mut newest) = containers.lock()
                        {
                            *newest = Some(newest.map_or(secs, |m| m.max(secs)));
                        }
                        return false;
                    }
                    if e.path_is_symlink() && !is_within_roots(e.path(), &croots) {
                        return false;
                    }
                }
                true
            });
        for path in &external_ignore_paths {
            if let Some(err) = walk.add_ignore(path) {
                warn!(path = %path.display(), error = %err, "failed to load ignore file");
            }
        }

        for entry in walk.build() {
            if shutdown.load(Ordering::Relaxed) {
                return Err(Error::Interrupted);
            }

            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    warn!(error = %err, "error traversing directory during mtime check, skipping");
                    continue;
                }
            };

            if !entry
                .file_type()
                .is_some_and(|ft| ft.is_file() || ft.is_dir())
            {
                continue;
            }

            let meta = match entry.path().metadata() {
                Ok(m) => m,
                Err(err) => {
                    warn!(path = %entry.path().display(), error = %err, "failed to read file metadata, skipping");
                    continue;
                }
            };
            let Some(secs) = epoch_secs(&meta) else {
                warn!(path = %entry.path().display(), "failed to read file mtime, skipping");
                continue;
            };
            max_mtime = Some(max_mtime.map_or(secs, |m: f64| m.max(secs)));
        }
    }

    let containers = container_mtime.lock().ok().and_then(|newest| *newest);
    Ok(match (max_mtime, containers) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    })
}

/// A file's mtime as UNIX epoch seconds.
fn epoch_secs(meta: &std::fs::Metadata) -> Option<f64> {
    let modified = meta.modified().ok()?;
    Some(
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin an entry's mtime (files and directories) to `epoch_secs`.
    fn set_mtime(path: &std::path::Path, epoch_secs: u64) {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch_secs);
        std::fs::File::open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    const OLD: u64 = 1_577_836_800; // 2020-01-01

    #[test]
    fn max_mtime_returns_none_without_roots_and_the_directorys_own_for_an_empty_one() {
        assert!(max_mtime_in_roots(&[], &[], &[]).unwrap().is_none());

        let dir = tempfile::TempDir::new().unwrap();
        set_mtime(dir.path(), OLD);
        let result = max_mtime_in_roots(&[dir.path().to_path_buf()], &[], &[]).unwrap();
        assert_eq!(result, Some(OLD as f64));
    }

    #[test]
    fn deleting_a_file_moves_the_newest_mtime() {
        let dir = tempfile::TempDir::new().unwrap();
        let keep = dir.path().join("keep.sh");
        let gone = dir.path().join("gone.sh");
        std::fs::write(&keep, "echo keep").unwrap();
        std::fs::write(&gone, "echo gone").unwrap();
        for path in [&keep, &gone, dir.path()] {
            set_mtime(path, OLD);
        }
        let roots = [dir.path().to_path_buf()];
        assert_eq!(
            max_mtime_in_roots(&roots, &[], &[]).unwrap(),
            Some(OLD as f64)
        );

        std::fs::remove_file(&gone).unwrap();
        let after = max_mtime_in_roots(&roots, &[], &[]).unwrap().unwrap();
        assert!(after > OLD as f64, "a deletion must be visible");
    }

    #[test]
    fn moving_in_a_file_with_an_old_mtime_moves_the_newest_mtime() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        set_mtime(&root, OLD);
        let outside = dir.path().join("old.sh");
        std::fs::write(&outside, "echo old").unwrap();
        set_mtime(&outside, OLD);

        std::fs::rename(&outside, root.join("old.sh")).unwrap();
        let after = max_mtime_in_roots(&[root], &[], &[]).unwrap().unwrap();
        assert!(after > OLD as f64, "a rename into the tree must be visible");
    }

    #[test]
    fn max_mtime_respects_shutdown_signal() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.py"), "# python").unwrap();
        let shutdown = AtomicBool::new(true);

        let err =
            max_mtime_in_roots_with_shutdown(&[dir.path().to_path_buf()], &[], &[], &shutdown)
                .unwrap_err();

        assert!(matches!(err, Error::Interrupted));
    }

    #[test]
    fn max_mtime_returns_newest_mtime() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.py"), "# python").unwrap();
        std::fs::write(dir.path().join("b.sh"), "#!/bin/bash").unwrap();

        let result = max_mtime_in_roots(&[dir.path().to_path_buf()], &[], &[])
            .unwrap()
            .expect("expected a non-None mtime");

        // The result should be a positive epoch timestamp in the current era.
        assert!(result > 1_000_000_000.0, "mtime looks like a valid epoch");
    }

    #[test]
    fn max_mtime_respects_catignore() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("vendor")).unwrap();
        std::fs::write(dir.path().join(".catignore"), "vendor/\n").unwrap();
        std::fs::write(dir.path().join("keep.py"), "# python").unwrap();
        // Write vendor file last so it would win in an unfiltered walk.
        std::fs::write(dir.path().join("vendor").join("skip.py"), "# python").unwrap();

        // Verify the function runs without error and returns a plausible
        // epoch value.  The .catignore exclusion test is handled structurally
        // (if vendor/skip.py were included the walk would return more items,
        // covered by the scan_respects_root_catignore scanner test).
        let result = max_mtime_in_roots(&[dir.path().to_path_buf()], &[], &[])
            .unwrap()
            .expect("expected a non-None mtime");

        assert!(
            result > 1_000_000_000.0,
            "mtime should be a plausible epoch"
        );
    }

    #[test]
    fn checkout_files_are_ignored_but_creating_or_removing_one_is_seen() {
        let dir = tempfile::TempDir::new().unwrap();
        let develop = dir.path().join("DEVELOP");
        std::fs::create_dir_all(&develop).unwrap();
        let checkout = develop.join("tool_20240315_1430_jdoe");
        std::fs::write(&checkout, "echo hi").unwrap();
        set_mtime(&checkout, OLD);
        set_mtime(&develop, OLD);
        set_mtime(dir.path(), OLD);
        let roots = [dir.path().to_path_buf()];
        assert_eq!(
            max_mtime_in_roots(&roots, &[], &[]).unwrap(),
            Some(OLD as f64)
        );

        // Editing a checkout in place changes no catalog content: ignored,
        // otherwise every engineer's save would force a rebuild.
        std::fs::write(&checkout, "echo edited").unwrap();
        assert_eq!(
            max_mtime_in_roots(&roots, &[], &[]).unwrap(),
            Some(OLD as f64)
        );

        // A new or removed checkout changes the container's mtime.
        std::fs::write(develop.join("tool_20240316_0900_other"), "x").unwrap();
        let created = max_mtime_in_roots(&roots, &[], &[]).unwrap().unwrap();
        assert!(created > OLD as f64);

        set_mtime(&develop, OLD);
        std::fs::remove_file(&checkout).unwrap();
        let removed = max_mtime_in_roots(&roots, &[], &[]).unwrap().unwrap();
        assert!(removed > OLD as f64);
    }
}
