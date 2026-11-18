//! Ownership of the indexer's scratch files next to the published catalog.
//!
//! A build writes its work-in-progress database as `<db>.wip` (with SQLite's
//! `-wal`/`-shm`/`-journal` sidecars and, after an interrupt, `<db>.wip.ckpt`)
//! in the catalog's own directory, because the final swap is a `rename(2)`
//! and cannot cross filesystems. This module makes sure those files are never
//! left behind and that only one build uses them at a time:
//!
//! - [`BuildLock`]: an exclusive `flock` on `<db>.lock` for the whole build.
//! - [`ScratchGuard`]: removes the scratch files on every exit path unless the
//!   build was interrupted and left a checkpoint to resume from.
//! - [`sweep_abandoned`]: removes leftovers of earlier scat versions and of
//!   killed dry runs.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::{debug, warn};

use crate::error::{Error, Result};
use crate::indexer::checkpoint::{ckpt_path, wip_path};

/// How old an unrecognised-owner scratch file must be before the sweep
/// removes it. A live build never runs this long; the margin only protects
/// an older scat (which takes no lock) that is still running.
const ABANDONED_AFTER: Duration = Duration::from_hours(24);

/// Prefix of the per-run directory a dry run builds in.
pub(crate) const DRY_RUN_DIR_PREFIX: &str = ".scat-dryrun-";

/// Path of the lock file guarding builds of `db_path`.
pub fn lock_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    db_path.with_file_name(name)
}

/// SQLite's sidecar files for the database at `path`.
fn sqlite_sidecars(path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    ["-wal", "-shm", "-journal"].into_iter().map(move |suffix| {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        path.with_file_name(name)
    })
}

/// Remove the SQLite database at `path` together with its sidecars.
pub(crate) fn remove_sqlite_files(path: &Path) {
    for p in std::iter::once(path.to_path_buf()).chain(sqlite_sidecars(path)) {
        remove_file_quietly(&p);
    }
}

fn remove_file_quietly(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => debug!(path = %path.display(), "removed scratch file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!(path = %path.display(), error = %e, "failed to remove scratch file"),
    }
}

/// An exclusive advisory lock held for the duration of one catalog build.
///
/// Two overlapping builds (a slow nightly job plus a manual run) would share
/// the `.wip`/`.ckpt` names and rotate copies concurrently. The lock is
/// released when this value is dropped, or by the OS if the process dies; the
/// lock file itself is left in place (removing it would race with a waiter).
#[derive(Debug)]
pub struct BuildLock {
    _file: Option<File>,
}

impl BuildLock {
    /// Take the build lock for `db_path`, failing fast if another build holds it.
    ///
    /// A filesystem that cannot lock at all (some network mounts) degrades to
    /// no locking with a warning rather than blocking builds there.
    pub fn acquire(db_path: &Path) -> Result<Self> {
        let path = lock_path(db_path);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: Some(file) }),
            Err(TryLockError::WouldBlock) => Err(Error::Validation(format!(
                "another catalog build is already running for {} (lock held on {})",
                db_path.display(),
                path.display()
            ))),
            Err(TryLockError::Error(e)) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "could not lock the catalog build; continuing without a build lock"
                );
                Ok(Self { _file: None })
            }
        }
    }
}

/// Removes a build's scratch files when dropped, unless [`keep`](Self::keep)
/// was called. Covers every error path between creating the work-in-progress
/// database and publishing it, including `?` early returns.
#[derive(Debug)]
pub struct ScratchGuard {
    wip: PathBuf,
    ckpt: PathBuf,
    armed: bool,
}

impl ScratchGuard {
    /// Guard the scratch files of a build whose published catalog is `db_path`.
    pub fn new(db_path: &Path) -> Self {
        Self {
            wip: wip_path(db_path),
            ckpt: ckpt_path(db_path),
            armed: true,
        }
    }

    /// Leave the files in place so the next run can resume from them.
    pub fn keep(&mut self) {
        self.armed = false;
    }
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        if self.armed {
            remove_sqlite_files(&self.wip);
            remove_file_quietly(&self.ckpt);
        }
    }
}

/// Whether `name` is a leftover of earlier scat versions: the
/// `.tmpXXXXXX` database they built into (any extension-less random suffix
/// of six alphanumerics) or one of its SQLite sidecars.
fn is_legacy_scratch_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(".tmp") else {
        return false;
    };
    let (random, suffix) = rest.split_at(rest.len().min(6));
    random.len() == 6
        && random.bytes().all(|b| b.is_ascii_alphanumeric())
        && matches!(suffix, "" | "-wal" | "-shm" | "-journal")
}

/// Remove abandoned scratch files from `dir` (the catalog's directory):
/// databases left by earlier scat versions that built into `.tmpXXXXXX` and
/// never cleaned up, and per-run directories of dry runs that were killed.
/// Only entries untouched for over a day are removed. Call with the build
/// lock held.
pub fn sweep_abandoned(dir: &Path) {
    sweep_abandoned_older_than(dir, ABANDONED_AFTER);
}

fn sweep_abandoned_older_than(dir: &Path, min_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_dry_run_dir = name.starts_with(DRY_RUN_DIR_PREFIX);
        if !is_dry_run_dir && !is_legacy_scratch_name(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() != is_dry_run_dir {
            continue;
        }
        let old_enough = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= min_age);
        if !old_enough {
            continue;
        }
        debug!(path = %entry.path().display(), "sweeping abandoned scratch entry");
        if is_dry_run_dir {
            let _ = std::fs::remove_dir_all(entry.path());
        } else {
            remove_file_quietly(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_names_match_only_tempfile_style_scratch_files() {
        assert!(is_legacy_scratch_name(".tmpQOeeab"));
        assert!(is_legacy_scratch_name(".tmpQOeeab-wal"));
        assert!(is_legacy_scratch_name(".tmpQOeeab-shm"));
        assert!(is_legacy_scratch_name(".tmpQOeeab-journal"));
        assert!(!is_legacy_scratch_name(".tmpQOeeab.keep"));
        assert!(!is_legacy_scratch_name(".tmpshort"));
        assert!(!is_legacy_scratch_name(".tmp"));
        assert!(!is_legacy_scratch_name("catalog.sqlite"));
        assert!(!is_legacy_scratch_name(".tmp.12345"));
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.sqlite");
        let first = BuildLock::acquire(&db).unwrap();
        let err = BuildLock::acquire(&db).unwrap_err().to_string();
        assert!(err.contains("already running"), "{err}");
        drop(first);
        BuildLock::acquire(&db).unwrap();
    }

    #[test]
    fn guard_removes_scratch_files_unless_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.sqlite");
        let files = [
            wip_path(&db),
            ckpt_path(&db),
            dir.path().join("c.sqlite.wip-wal"),
            dir.path().join("c.sqlite.wip-shm"),
        ];
        for f in &files {
            std::fs::write(f, b"x").unwrap();
        }
        let mut kept = ScratchGuard::new(&db);
        kept.keep();
        drop(kept);
        assert!(files.iter().all(|f| f.exists()));
        drop(ScratchGuard::new(&db));
        assert!(files.iter().all(|f| !f.exists()));
    }

    #[test]
    fn sweep_removes_old_leftovers_and_spares_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join(".tmpAbC123");
        let legacy_wal = dir.path().join(".tmpAbC123-wal");
        let dry = dir.path().join(".scat-dryrun-xyz");
        let unrelated = dir.path().join("catalog.sqlite");
        std::fs::write(&legacy, b"x").unwrap();
        std::fs::write(&legacy_wal, b"x").unwrap();
        std::fs::create_dir(&dry).unwrap();
        std::fs::write(dry.join("build.sqlite"), b"x").unwrap();
        std::fs::write(&unrelated, b"x").unwrap();

        // Fresh entries belong to a possibly live build.
        sweep_abandoned(dir.path());
        assert!(legacy.exists() && dry.exists());

        sweep_abandoned_older_than(dir.path(), Duration::ZERO);
        assert!(!legacy.exists() && !legacy_wal.exists() && !dry.exists());
        assert!(unrelated.exists());
    }
}
