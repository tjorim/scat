//! Checkpoint support for interruptible indexing.
//!
//! Two files live alongside the in-progress WIP database:
//!
//! ```text
//! catalog.db.wip          ← partial SQLite DB (built by atomic.rs)
//! catalog.db.wip.ckpt     ← JSON: { "indexed": [...paths...] }
//! ```
//!
//! On startup `build_index` looks for both files. If found, it skips the
//! already-indexed paths and continues from where it left off.  If the WIP DB
//! is corrupt the pair is deleted and a fresh run starts.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::Result;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Persisted state between interrupted indexing runs.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Physical paths that have already been written to the WIP database.
    pub indexed: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Path of the WIP database alongside `db_path`.
pub fn wip_path(db_path: &Path) -> PathBuf {
    let mut p = db_path.to_path_buf();
    let name = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    p.set_file_name(format!("{name}.wip"));
    p
}

/// Path of the checkpoint JSON file alongside `db_path`.
pub fn ckpt_path(db_path: &Path) -> PathBuf {
    let wip = wip_path(db_path);
    let name = wip
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut p = wip.clone();
    p.set_file_name(format!("{name}.ckpt"));
    p
}

// ---------------------------------------------------------------------------
// Read / write / delete
// ---------------------------------------------------------------------------

/// Write (overwrite) the checkpoint file.
pub fn write_checkpoint(db_path: &Path, ckpt: &Checkpoint) -> Result<()> {
    let path = ckpt_path(db_path);
    let json = serde_json::to_string(ckpt)?;
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, &path)?;
    debug!(path = %path.display(), indexed = ckpt.indexed.len(), "wrote checkpoint");
    Ok(())
}

/// Read a checkpoint file. Returns `None` if the file does not exist.
pub fn read_checkpoint(db_path: &Path) -> Result<Option<Checkpoint>> {
    let path = ckpt_path(db_path);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let ckpt: Checkpoint = serde_json::from_str(&raw)?;
    debug!(path = %path.display(), indexed = ckpt.indexed.len(), "read checkpoint");
    Ok(Some(ckpt))
}

/// Delete both the WIP database and the checkpoint file.
pub fn delete_wip_pair(db_path: &Path) {
    for p in &[wip_path(db_path), ckpt_path(db_path)] {
        if p.exists() {
            let _ = std::fs::remove_file(p);
            debug!(path = %p.display(), "deleted wip/ckpt file");
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_roundtrip_write_read_skipped_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("catalog.db");

        let ckpt = Checkpoint {
            indexed: [
                "/catalog/scripts/a.py".to_string(),
                "/catalog/scripts/b.sh".to_string(),
                "/catalog/scripts/c.py".to_string(),
            ]
            .into_iter()
            .collect(),
        };

        write_checkpoint(&db_path, &ckpt).unwrap();

        // File must exist
        assert!(ckpt_path(&db_path).exists());

        let loaded = read_checkpoint(&db_path)
            .unwrap()
            .expect("checkpoint must be present");
        assert_eq!(loaded.indexed.len(), 3);

        assert!(loaded.indexed.contains("/catalog/scripts/a.py"));
        assert!(loaded.indexed.contains("/catalog/scripts/b.sh"));
        assert!(loaded.indexed.contains("/catalog/scripts/c.py"));
        assert!(!loaded.indexed.contains("/catalog/scripts/d.py"));
    }

    #[test]
    fn read_checkpoint_returns_none_when_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("catalog.db");
        assert!(read_checkpoint(&db_path).unwrap().is_none());
    }

    #[test]
    fn delete_wip_pair_removes_both_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("catalog.db");

        // Create dummy files
        std::fs::write(wip_path(&db_path), b"wip").unwrap();
        std::fs::write(ckpt_path(&db_path), b"{}").unwrap();

        delete_wip_pair(&db_path);

        assert!(!wip_path(&db_path).exists());
        assert!(!ckpt_path(&db_path).exists());
    }

    #[test]
    fn wip_path_and_ckpt_path_are_siblings_of_db() {
        let db_path = Path::new("/some/dir/catalog.db");
        let wip = wip_path(db_path);
        let ckpt = ckpt_path(db_path);
        assert_eq!(wip, Path::new("/some/dir/catalog.db.wip"));
        assert_eq!(ckpt, Path::new("/some/dir/catalog.db.wip.ckpt"));
    }
}
