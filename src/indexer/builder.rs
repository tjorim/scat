use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::Connection;
use tempfile::NamedTempFile;
use tracing::debug;

use crate::core::db::create_db;
use crate::core::vc::{VcConfig, load_vc_config};
use crate::error::{Error, Result};
use crate::indexer::atomic::{atomic_swap, rotate_copies};
use crate::indexer::checkpoint::{Checkpoint, delete_wip_pair, read_checkpoint, wip_path};
use crate::indexer::treesitter_deps::TreeSitterExtractor;

mod pipeline;
mod resolve;

use self::pipeline::populate;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug)]
/// Summary of one indexing run.
pub struct IndexResult {
    /// Number of scripts inserted/updated.
    pub scripts_indexed: usize,
    /// Number of dependency edges recorded.
    pub dependencies_indexed: usize,
    /// Output database path (tmp path for dry-run).
    pub db_path: PathBuf,
    /// Whether indexing was executed in dry-run mode.
    pub dry_run: bool,
    /// Non-fatal per-file processing errors.
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, Default)]
/// Options controlling index build behavior.
pub struct BuildOptions {
    /// Prefix prepended to logical paths.
    pub logical_prefix: String,
    /// Number of head lines to read for language/shebang detection.
    pub head_lines: usize,
    /// Additional ignore files applied during scanning.
    pub ignore_files: Vec<PathBuf>,
    /// Number of rotated historical copies to retain.
    pub keep_copies: usize,
    /// When true, build into temp DB without replacing the live DB.
    pub dry_run: bool,
    /// Optional vc configuration override.
    pub vc_config: Option<VcConfig>,
    /// Suppress the progress bar (final summary is still printed).
    pub quiet: bool,
    /// Ignore any existing checkpoint and start fresh.
    pub no_resume: bool,
    /// Shared shutdown flag for Ctrl-C handling. If None, a local handler is registered.
    pub shutdown: Option<Arc<AtomicBool>>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a catalog index from scan roots into the target SQLite database.
pub fn build_index(
    scan_roots: &[PathBuf],
    db_path: &Path,
    opts: BuildOptions,
) -> Result<IndexResult> {
    debug!(
        db_path = %db_path.display(),
        scan_root_count = scan_roots.len(),
        dry_run = opts.dry_run,
        keep_copies = opts.keep_copies,
        "starting index build"
    );
    if db_path.exists() && !db_path.is_file() {
        let found_kind = if db_path.is_dir() {
            "directory"
        } else {
            "non-file"
        };
        return Err(Error::Validation(format!(
            "database path must be a file, found {found_kind}: {}",
            db_path.display()
        )));
    }
    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let vc_config = match opts.vc_config {
        Some(c) => c,
        None => load_vc_config(None)?,
    };

    let head_lines = if opts.head_lines == 0 {
        10
    } else {
        opts.head_lines
    };

    // Determine whether progress output is enabled.
    let use_progress = !opts.quiet && stderr_is_tty();

    // -----------------------------------------------------------------------
    // Resume logic: look for a .wip + .ckpt pair from a previous interrupted run.
    // -----------------------------------------------------------------------
    let wip = wip_path(db_path);
    let resume_checkpoint: Option<Checkpoint> = if opts.no_resume || opts.dry_run {
        // User asked for a fresh run, or dry-run never uses checkpoints.
        delete_wip_pair(db_path);
        None
    } else if wip.exists() {
        match read_checkpoint(db_path) {
            Ok(Some(ckpt)) => {
                // Validate the WIP DB before trusting the checkpoint.
                if validate(&wip).is_ok() {
                    debug!(
                        wip = %wip.display(),
                        already_indexed = ckpt.indexed.len(),
                        "resuming from checkpoint"
                    );
                    Some(ckpt)
                } else {
                    // Corrupt WIP DB — start fresh.
                    debug!(wip = %wip.display(), "corrupt wip db, starting fresh");
                    delete_wip_pair(db_path);
                    None
                }
            }
            Ok(None) => {
                // WIP exists but no checkpoint: delete and start fresh.
                delete_wip_pair(db_path);
                None
            }
            Err(_) => {
                delete_wip_pair(db_path);
                None
            }
        }
    } else {
        None
    };

    // The tmp_path: either the existing WIP (resume) or a fresh temp file.
    let tmp_path = if resume_checkpoint.is_some() {
        wip.clone()
    } else {
        let temp_file = NamedTempFile::new_in(parent)?;
        let tmp_path = temp_file.path().to_path_buf();
        // Don't drop temp_file here; keep it alive to prevent path reuse
        // Explicitly persist to prevent automatic deletion on drop
        std::mem::forget(temp_file);
        tmp_path
    };

    let mut result = IndexResult {
        scripts_indexed: 0,
        dependencies_indexed: 0,
        db_path: tmp_path.clone(),
        dry_run: opts.dry_run,
        errors: Vec::new(),
    };

    // -----------------------------------------------------------------------
    // Use provided shutdown flag or create a local one with handler.
    // -----------------------------------------------------------------------
    let shutdown = if let Some(flag) = opts.shutdown.clone() {
        flag
    } else {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let flag_clone = Arc::clone(&flag);
            // We ignore the error if a handler is already installed (e.g. in tests).
            let _ = ctrlc::set_handler(move || {
                flag_clone.store(true, Ordering::SeqCst);
            });
        }
        flag
    };

    let build_result = (|| -> Result<()> {
        debug!(
            phase = "create_db",
            path = %tmp_path.display(),
            "starting create_db phase"
        );
        let mut conn = if resume_checkpoint.is_some() {
            // Open the existing WIP database for appending.
            Connection::open(&tmp_path)?
        } else {
            create_db(&tmp_path)?
        };
        // Relax durability and grow caches for this throwaway build
        // connection — see `apply_bulk_build_pragmas` doc comment. Applied
        // unconditionally since these are per-connection settings, not
        // persisted in the database file, so a resumed build's freshly
        // reopened connection needs them too.
        crate::core::db::apply_bulk_build_pragmas(&conn)?;
        let mut ts = TreeSitterExtractor::new()?;
        debug!(phase = "populate", "starting populate phase");
        populate(
            &mut conn,
            scan_roots,
            &opts.logical_prefix,
            head_lines,
            &opts.ignore_files,
            &mut ts,
            &mut result,
            &vc_config,
            db_path,
            resume_checkpoint,
            &shutdown,
            use_progress,
            opts.dry_run,
        )
    })();

    if let Err(e) = build_result {
        // If shutdown was requested we wrote a checkpoint before returning;
        // keep the WIP file so the user can resume.
        if !shutdown.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&tmp_path);
        }
        return Err(e);
    }

    // If interrupted, the checkpoint has already been written inside populate().
    if shutdown.load(Ordering::SeqCst) {
        return Err(Error::Interrupted);
    }

    // Clean up checkpoint files now that we have a complete DB.
    delete_wip_pair(db_path);

    validate(&tmp_path)?;
    debug!(
        phase = "validate",
        scripts_indexed = result.scripts_indexed,
        dependency_count = result.dependencies_indexed,
        error_count = result.errors.len(),
        "completed build validation"
    );

    if opts.dry_run {
        result.db_path = tmp_path;
        debug!(path = %result.db_path.display(), "completed dry-run index build");
        return Ok(result);
    }

    if db_path.exists() && opts.keep_copies > 0 {
        debug!(
            phase = "rotate_copies",
            keep_copies = opts.keep_copies,
            "starting rotate_copies phase"
        );
        rotate_copies(db_path, opts.keep_copies)?;
    }

    if use_progress {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.set_message("Swapping database…");
        pb.enable_steady_tick(std::time::Duration::from_millis(80));

        debug!(
            phase = "atomic_swap",
            from = %tmp_path.display(),
            to = %db_path.display(),
            "starting atomic_swap phase"
        );
        atomic_swap(&tmp_path, db_path)?;

        pb.finish_and_clear();
    } else {
        debug!(
            phase = "atomic_swap",
            from = %tmp_path.display(),
            to = %db_path.display(),
            "starting atomic_swap phase"
        );
        atomic_swap(&tmp_path, db_path)?;
    }

    result.db_path = db_path.to_path_buf();
    debug!(
        db_path = %result.db_path.display(),
        scripts_indexed = result.scripts_indexed,
        dependency_count = result.dependencies_indexed,
        error_count = result.errors.len(),
        "completed index build"
    );

    Ok(result)
}

// ---------------------------------------------------------------------------
// TTY detection
// ---------------------------------------------------------------------------

fn stderr_is_tty() -> bool {
    // Use the `std::io::IsTerminal` trait (stable since Rust 1.70).
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn validate(db_path: &Path) -> Result<()> {
    use rusqlite::OptionalExtension;

    let conn = Connection::open(db_path)?;
    let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if check != "ok" {
        return Err(Error::Validation(format!(
            "integrity_check failed: {check}"
        )));
    }
    let meta: Option<i64> = conn
        .query_row("SELECT id FROM index_metadata WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?;
    if meta.is_none() {
        return Err(Error::Validation("index_metadata row missing".into()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_index_creates_db() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("foo.py"), "import os\n# @author alice\n").unwrap();

        let db_path = dir.path().join("scripts.sqlite");
        let result = build_index(
            std::slice::from_ref(&root),
            &db_path,
            BuildOptions {
                logical_prefix: "/catalog/scripts".into(),
                head_lines: 10,
                keep_copies: 0,
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(result.scripts_indexed, 1);
        assert!(db_path.exists());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn dry_run_leaves_live_db_unchanged() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("foo.py"), "pass").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        build_index(
            std::slice::from_ref(&root),
            &db_path,
            BuildOptions {
                logical_prefix: "/catalog/scripts".into(),
                head_lines: 10,
                keep_copies: 0,
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();

        // dry run should NOT create the live db
        assert!(!db_path.exists());
    }

    #[test]
    fn rotation_creates_copy() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "pass").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        // First build
        build_index(
            std::slice::from_ref(&root),
            &db_path,
            BuildOptions {
                logical_prefix: "/catalog/scripts".into(),
                head_lines: 10,
                keep_copies: 2,
                ..Default::default()
            },
        )
        .unwrap();

        // Second build should rotate
        std::fs::write(root.join("b.py"), "pass").unwrap();
        build_index(
            std::slice::from_ref(&root),
            &db_path,
            BuildOptions {
                logical_prefix: "/catalog/scripts".into(),
                head_lines: 10,
                keep_copies: 2,
                ..Default::default()
            },
        )
        .unwrap();

        let copy1 = dir.path().join("scripts.sqlite.1");
        assert!(copy1.exists());
    }

    #[test]
    fn build_index_rejects_directory_db_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "pass").unwrap();
        let db_dir = dir.path().join("existing-db-dir");
        std::fs::create_dir(&db_dir).unwrap();

        let err = build_index(
            std::slice::from_ref(&root),
            &db_dir,
            BuildOptions {
                logical_prefix: "/catalog/scripts".into(),
                head_lines: 10,
                keep_copies: 2,
                ..Default::default()
            },
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("database path must be a file, found directory"),
            "unexpected error: {err}"
        );
    }
}
