use std::path::Path;

use crate::error::Result;
use tracing::debug;

// ---------------------------------------------------------------------------
// Atomic swap
// ---------------------------------------------------------------------------

/// Replace `dst` with `src` atomically.
///
/// `std::fs::rename` maps to POSIX `rename(2)`, which is unconditionally
/// atomic and, crucially, succeeds even while readers hold `dst` open: they
/// keep reading the old inode until they close it, and every process that
/// opens the path afterwards sees the new build. No swap-time coordination
/// with readers is needed, and none exists.
pub fn atomic_swap(src: &Path, dst: &Path) -> Result<()> {
    debug!(src = %src.display(), dst = %dst.display(), "performing atomic database swap");
    std::fs::rename(src, dst)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Historical copy rotation
// ---------------------------------------------------------------------------

/// Rotate historical copies of `db_path`.
///
/// Copies are named `<db_path>.1`, `<db_path>.2`, … `<db_path>.N`.
/// The oldest copy that would exceed `keep_copies` is deleted first,
/// then existing copies are shifted up, and the live DB is hard-linked
/// to `<db_path>.1`.  Falls back to `fs::copy` if hard-link fails (e.g.
/// the share is mounted with hard links disabled).
pub fn rotate_copies(db_path: &Path, keep_copies: usize) -> Result<()> {
    if keep_copies == 0 {
        return Ok(());
    }

    debug!(
        db_path = %db_path.display(),
        keep_copies,
        "rotating database copies"
    );

    // Delete the oldest copy that would be pushed beyond the limit.
    let oldest = format!("{}.{}", db_path.display(), keep_copies);
    let oldest_path = Path::new(&oldest);
    if oldest_path.exists() {
        std::fs::remove_file(oldest_path)?;
    }

    // Shift existing copies up by one.
    for i in (1..keep_copies).rev() {
        let src_name = format!("{}.{}", db_path.display(), i);
        let dst_name = format!("{}.{}", db_path.display(), i + 1);
        let src = Path::new(&src_name);
        if src.exists() {
            std::fs::rename(src, Path::new(&dst_name))?;
        }
    }

    // Link live DB → copy .1
    let copy1 = format!("{}.1", db_path.display());
    std::fs::hard_link(db_path, &copy1).or_else(|_| {
        // Fallback: copy instead of hard-link.
        std::fs::copy(db_path, &copy1).map(|_| ())
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn swap_replaces_existing_file() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.db");
        let dst = dir.path().join("dst.db");
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        atomic_swap(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"new");
        assert!(!src.exists());
    }

    #[test]
    fn swap_when_dst_missing() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src.db");
        let dst = dir.path().join("dst.db");
        fs::write(&src, b"content").unwrap();
        atomic_swap(&src, &dst).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"content");
    }

    #[test]
    fn rotate_creates_copy_one() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("scripts.sqlite");
        fs::write(&db, b"live").unwrap();
        rotate_copies(&db, 3).unwrap();
        let copy1 = dir.path().join("scripts.sqlite.1");
        assert!(copy1.exists());
        assert_eq!(fs::read(&copy1).unwrap(), b"live");
    }

    #[test]
    fn rotate_keeps_at_most_n() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("scripts.sqlite");

        // Create existing copies .1 .2 .3
        for i in 1..=3 {
            fs::write(
                dir.path().join(format!("scripts.sqlite.{i}")),
                format!("v{i}"),
            )
            .unwrap();
        }
        fs::write(&db, b"live").unwrap();

        rotate_copies(&db, 3).unwrap();

        // .3 should exist (shifted from .2), .4 should NOT.
        assert!(dir.path().join("scripts.sqlite.3").exists());
        assert!(!dir.path().join("scripts.sqlite.4").exists());
    }
}
