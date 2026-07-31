use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::Connection;
use tempfile::NamedTempFile;
use tracing::{debug, warn};

use crate::core::db::create_build_db;
use crate::core::vc::{VcConfig, load_vc_config};
use crate::error::{Error, Result};
use crate::indexer::atomic::{atomic_swap, rotate_copies};
use crate::indexer::checkpoint::{Checkpoint, delete_wip_pair, read_checkpoint, wip_path};

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
    /// Of `scripts_indexed`, how many were skipped (not re-extracted) because
    /// they were already present and unchanged — from an incrementally
    /// seeded previous build, or from an interrupted attempt being resumed.
    pub scripts_reused: usize,
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
    /// Always do a full rebuild instead of seeding from the previous
    /// completed build. See `build_index`'s incremental-seeding doc comment.
    pub no_incremental: bool,
    /// Shared shutdown flag for Ctrl-C handling. If None, a local handler is registered.
    pub shutdown: Option<Arc<AtomicBool>>,
    /// Worker threads for the parallel scan and extraction phases. `None`
    /// uses rayon's default (the number of logical CPUs, or `RAYON_NUM_THREADS`
    /// if set).
    pub threads: Option<usize>,
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
    if opts.threads == Some(0) {
        return Err(Error::Validation(
            "threads must be greater than zero".into(),
        ));
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
        wip
    } else {
        let temp_file = NamedTempFile::new_in(parent)?;
        let tmp_path = temp_file.path().to_path_buf();
        // Don't drop temp_file here; keep it alive to prevent path reuse
        // Explicitly persist to prevent automatic deletion on drop
        std::mem::forget(temp_file);
        tmp_path
    };

    // -----------------------------------------------------------------------
    // Incremental seeding: on a fresh (non-resume) attempt, start from a copy
    // of the previous completed build instead of an empty database, so
    // `populate` can skip re-extracting scripts that haven't changed. Only
    // eligible when the previous build's schema matches exactly — a version
    // bump means the stored rows may not match the current column set, so
    // this always falls back to a full rebuild rather than risk seeding from
    // incompatible data.
    //
    // Deliberately does *not* run a `PRAGMA integrity_check` on db_path here:
    // it was already validated at the end of whatever build produced it, a
    // full check is slow on a large catalog (defeating the point of doing
    // this incrementally), and `seed_from_previous_build` below falls back to
    // a full rebuild on its own if db_path turns out to be unreadable. A
    // build's own output is still always integrity-checked (see `validate`
    // below) before it's ever swapped in as the live database, regardless of
    // whether this path was taken.
    let incremental_seed = resume_checkpoint.is_none()
        && !opts.dry_run
        && !opts.no_incremental
        && db_path.exists()
        && crate::core::db::schema_version_of(db_path) == Some(crate::core::db::SCHEMA_VERSION);

    let mut result = IndexResult {
        scripts_indexed: 0,
        scripts_reused: 0,
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

    let run_build = || -> Result<()> {
        debug!(
            phase = "create_db",
            path = %tmp_path.display(),
            "starting create_db phase"
        );
        let (mut conn, incremental) = if resume_checkpoint.is_some() {
            // Open the existing WIP database for appending.
            (Connection::open(&tmp_path)?, false)
        } else if incremental_seed {
            match seed_from_previous_build(db_path, &tmp_path) {
                Ok(conn) => (conn, true),
                Err(e) => {
                    // db_path turned out to be unreadable/corrupt, or the
                    // backup step otherwise failed — fall back to a full
                    // rebuild rather than fail the whole run over it. This
                    // is also why `incremental_seed` above doesn't itself
                    // integrity-check db_path: this is the fallback for
                    // exactly that case.
                    warn!(
                        error = %e,
                        "failed to seed incremental build from previous database, falling back to a full rebuild"
                    );
                    (create_build_db(&tmp_path)?, false)
                }
            }
        } else {
            (create_build_db(&tmp_path)?, false)
        };
        // Relax durability and grow caches for this throwaway build
        // connection — see `apply_bulk_build_pragmas` doc comment. Applied
        // unconditionally since these are per-connection settings, not
        // persisted in the database file, so a resumed build's freshly
        // reopened connection needs them too.
        crate::core::db::apply_bulk_build_pragmas(&conn)?;
        debug!(phase = "populate", incremental, "starting populate phase");
        populate(
            &mut conn,
            scan_roots,
            &opts.logical_prefix,
            head_lines,
            &opts.ignore_files,
            &mut result,
            &vc_config,
            db_path,
            resume_checkpoint,
            &shutdown,
            use_progress,
            opts.dry_run,
            incremental,
        )
    };

    // Scanning (scanner.rs) and extraction (builder/pipeline.rs) both farm
    // per-file work out to rayon. Installing a custom pool here — instead of
    // letting each `par_iter()` fall back to rayon's global default pool —
    // is what makes `opts.threads` actually bound the parallelism of both
    // phases, since a pool installed with `.install()` becomes the "current"
    // pool for every `par_iter()` called from within the closure, however
    // deep the call stack.
    let build_result = match opts.threads {
        Some(n) => {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .map_err(|e| Error::Validation(format!("failed to build thread pool: {e}")))?;
            pool.install(run_build)
        }
        None => run_build(),
    };

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

    finalize_journal_mode(&tmp_path)?;
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
// Incremental seeding
// ---------------------------------------------------------------------------

/// Seed `tmp_path` with a consistent snapshot of `db_path` via SQLite's
/// Online Backup API, then reapply the schema DDL (idempotent; a cheap
/// safety net against schema drift even though the caller already checked
/// `schema_version`).
///
/// The backup API — not a raw `std::fs::copy` — is what makes this safe
/// under concurrent access: `db_path` is the live catalog, and a reader (an
/// open TUI session, a search query) could be holding a connection to it at
/// the moment a build starts. SQLite only checkpoints a WAL-mode database's
/// write-ahead log into the main file when the *last* connection to it
/// closes; with a concurrent reader still open, a plain file copy of just
/// the main file could silently miss whatever's still sitting in the `-wal`
/// sidecar. The backup API instead reads through SQLite's own consistent
/// snapshot mechanism and copies page-by-page, so it's correct regardless of
/// what's checkpointed versus what's still in the WAL.
fn seed_from_previous_build(db_path: &Path, tmp_path: &Path) -> Result<Connection> {
    let src = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut dst = Connection::open(tmp_path)?;
    // Before the WAL switch below, for the same reason `create_db` does it
    // first — see `apply_exclusive_wal_locking`.
    crate::core::db::apply_exclusive_wal_locking(&dst)?;
    {
        let backup = rusqlite::backup::Backup::new(&src, &mut dst)?;
        backup.step(-1)?;
    }
    // Whatever journal mode the seed carried, the WIP file wants WAL for the
    // insert-heavy phase ahead of it — the same mode `create_db` sets on a
    // from-scratch build, so both paths perform alike. The published
    // database is switched back to a rollback journal by
    // `finalize_journal_mode` just before the swap.
    let _: String = dst.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    dst.execute_batch(crate::core::db::DDL)?;
    Ok(dst)
}

// ---------------------------------------------------------------------------
// Publication
// ---------------------------------------------------------------------------

/// Switch a finished WIP database out of WAL mode before it is published.
///
/// WAL is the right mode *during* a build — one writer, many small commits,
/// no fsync per commit — but the wrong mode for the file that gets swapped
/// onto the shared drive. WAL requires an `mmap`-able `-shm` file shared
/// between connections, which network filesystems generally don't provide,
/// and it needs one *even for read-only connections*: a reader that can't
/// create `-shm` next to the catalog can't open it at all. A rollback-journal
/// database is a single self-contained file that any reader can open with no
/// sidecars and no write access to the directory holding it.
///
/// Switching modes here also checkpoints and removes the `-wal` file, so the
/// atomic swap moves one complete file into place rather than a main file
/// whose newest pages are still sitting in a sidecar left behind by the
/// rename.
fn finalize_journal_mode(db_path: &Path) -> Result<()> {
    let conn = Connection::open(db_path)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))?;
    debug!(
        phase = "finalize_journal_mode",
        path = %db_path.display(),
        journal_mode = %mode,
        "prepared database for publication"
    );
    Ok(())
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
    fn build_index_with_explicit_thread_count_still_indexes_everything() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "import os\n").unwrap();
        std::fs::write(root.join("b.sh"), "#!/bin/bash\necho hi\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        let result = build_index(
            std::slice::from_ref(&root),
            &db_path,
            BuildOptions {
                logical_prefix: "/catalog/scripts".into(),
                head_lines: 10,
                keep_copies: 0,
                threads: Some(2),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(result.scripts_indexed, 2);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn build_index_rejects_zero_threads() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "pass\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        let err = build_index(
            std::slice::from_ref(&root),
            &db_path,
            BuildOptions {
                logical_prefix: "/catalog/scripts".into(),
                head_lines: 10,
                keep_copies: 0,
                threads: Some(0),
                ..Default::default()
            },
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("threads must be greater than zero"),
            "unexpected error: {err}"
        );
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

    fn incremental_opts(keep_copies: usize) -> BuildOptions {
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies,
            ..Default::default()
        }
    }

    #[test]
    fn incremental_reuses_unchanged_scripts() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "import os\n").unwrap();
        std::fs::write(root.join("b.py"), "import sys\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        let first =
            build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();
        assert_eq!(first.scripts_reused, 0, "first build has nothing to reuse");
        assert_eq!(first.scripts_indexed, 2);

        // Nothing changed on disk — the second build should reuse both.
        let second =
            build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();
        assert_eq!(second.scripts_indexed, 2);
        assert_eq!(
            second.scripts_reused, 2,
            "unchanged scripts should be reused, not re-extracted"
        );
    }

    #[test]
    fn incremental_reextracts_changed_script() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "# @brief original\n").unwrap();
        std::fs::write(root.join("b.py"), "import sys\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();

        // Change only a.py's content (and thus its size).
        std::fs::write(root.join("a.py"), "# @brief updated and longer\n").unwrap();

        let second =
            build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();
        assert_eq!(second.scripts_indexed, 2);
        assert_eq!(
            second.scripts_reused, 1,
            "only the unchanged script should be reused"
        );

        let conn =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let purpose: String = conn
            .query_row(
                "SELECT purpose FROM scripts WHERE logical_path LIKE '%a.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(purpose, "updated and longer");
    }

    #[test]
    fn incremental_reuses_touched_but_unchanged_script() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "import os\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();

        // Bump mtime without changing content — e.g. a `touch`, or a VC
        // checkout that rewrites the file with identical bytes.
        let file = std::fs::File::open(root.join("a.py")).unwrap();
        let new_mtime = std::time::SystemTime::now() + std::time::Duration::from_mins(2);
        file.set_modified(new_mtime).unwrap();

        let second =
            build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();
        assert_eq!(second.scripts_indexed, 1);
        assert_eq!(
            second.scripts_reused, 1,
            "content-identical script should be reused via hash comparison even though mtime changed"
        );

        // The stored mtime should have been refreshed to the new value so
        // the next build stays on the cheap size+mtime fast path.
        let conn =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let stored_mtime: f64 = conn
            .query_row(
                "SELECT mtime FROM scripts WHERE logical_path LIKE '%a.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let expected_mtime = new_mtime
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!(
            (stored_mtime - expected_mtime).abs() < 1.0,
            "stored mtime should be refreshed to the new on-disk mtime, got {stored_mtime}, expected ~{expected_mtime}"
        );
    }

    #[test]
    fn incremental_removes_deleted_script_and_unresolves_dependents() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("main.py"), "import helper\n").unwrap();
        std::fs::write(root.join("helper.py"), "pass\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();

        {
            let conn =
                Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .unwrap();
            let resolved: Option<i64> = conn
                .query_row(
                    "SELECT d.resolved_script_id FROM dependencies d
                     JOIN scripts s ON s.id = d.script_id
                     WHERE s.logical_path LIKE '%main.py'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(
                resolved.is_some(),
                "helper.py should resolve before deletion"
            );
        }

        // Remove helper.py from disk; main.py itself is untouched.
        std::fs::remove_file(root.join("helper.py")).unwrap();

        let second =
            build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();
        assert_eq!(second.scripts_indexed, 1, "only main.py remains");
        assert_eq!(
            second.scripts_reused, 1,
            "main.py's own content didn't change, so it should be reused"
        );

        let conn =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let helper_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM scripts WHERE logical_path LIKE '%helper.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(helper_count, 0, "deleted script's row must be removed");

        let resolved_after: Option<i64> = conn
            .query_row(
                "SELECT d.resolved_script_id FROM dependencies d
                 JOIN scripts s ON s.id = d.script_id
                 WHERE s.logical_path LIKE '%main.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            resolved_after.is_none(),
            "a dependency on a now-deleted script must become unresolved, not dangle"
        );
    }

    #[test]
    fn no_incremental_flag_forces_full_rebuild() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "pass\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();

        let second = build_index(
            std::slice::from_ref(&root),
            &db_path,
            BuildOptions {
                no_incremental: true,
                ..incremental_opts(0)
            },
        )
        .unwrap();

        assert_eq!(
            second.scripts_reused, 0,
            "--no-incremental must force a full rebuild"
        );
    }

    #[test]
    fn incremental_falls_back_on_schema_mismatch() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "pass\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();

        // Simulate a schema upgrade since the last build.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute("UPDATE index_metadata SET schema_version = -1", [])
                .unwrap();
        }

        let second =
            build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();

        assert_eq!(
            second.scripts_reused, 0,
            "a schema_version mismatch must fall back to a full rebuild"
        );
    }

    #[test]
    fn incremental_reresolves_dependency_pointing_at_stale_target() {
        // Simulates what an incremental seed can carry over: a dependency
        // row whose `resolved_script_id` no longer matches what fresh
        // resolution would produce (e.g. a shadowing script changed which
        // target is correct). Since `caller.py` itself doesn't change
        // between builds, phase 2 reuses it without touching its
        // dependency row — so if the resolve phase only re-resolved rows
        // that were already NULL, this stale value would never be
        // corrected. It must be reset and re-resolved on every build.
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("caller.py"), "import helper\n").unwrap();
        std::fs::write(root.join("helper.py"), "pass\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();

        let (caller_id, helper_id) = {
            let conn = Connection::open(&db_path).unwrap();
            let caller_id: i64 = conn
                .query_row(
                    "SELECT id FROM scripts WHERE logical_path LIKE '%caller.py'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let helper_id: i64 = conn
                .query_row(
                    "SELECT id FROM scripts WHERE logical_path LIKE '%helper.py'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            // Corrupt the resolution to point at the wrong (but
            // FK-valid) target, standing in for a stale value carried
            // over by an incremental seed.
            conn.execute(
                "UPDATE dependencies SET resolved_script_id = ?1 WHERE script_id = ?2",
                rusqlite::params![caller_id, caller_id],
            )
            .unwrap();
            (caller_id, helper_id)
        };

        // Nothing on disk changed, so caller.py is reused rather than
        // re-extracted — its dependency row is only touched by the
        // resolve phase, not by extraction.
        let second =
            build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();
        assert_eq!(
            second.scripts_reused, 2,
            "both scripts are unchanged on disk"
        );

        let conn =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let resolved: Option<i64> = conn
            .query_row(
                "SELECT resolved_script_id FROM dependencies WHERE script_id = ?1",
                [caller_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            resolved,
            Some(helper_id),
            "the resolve phase must reset and re-resolve every dependency, \
             not just ones already NULL, so a stale carried-over target gets corrected"
        );
    }

    #[test]
    fn seed_from_previous_build_copies_data_via_backup_api() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("scripts");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.py"), "pass\n").unwrap();
        let db_path = dir.path().join("scripts.sqlite");

        build_index(std::slice::from_ref(&root), &db_path, incremental_opts(0)).unwrap();

        let tmp_path = dir.path().join("seeded.sqlite");
        let conn = seed_from_previous_build(&db_path, &tmp_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM scripts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "backup-seeded database should contain the previous build's rows"
        );
    }

    #[test]
    fn seed_from_previous_build_errors_on_invalid_source_instead_of_panicking() {
        let dir = tempfile::TempDir::new().unwrap();
        let bogus_src = dir.path().join("not-a-database.sqlite");
        std::fs::write(&bogus_src, b"not a sqlite file at all").unwrap();
        let tmp_path = dir.path().join("seeded.sqlite");

        let err = seed_from_previous_build(&bogus_src, &tmp_path);
        assert!(
            err.is_err(),
            "an unreadable/corrupt source must return an error, not panic, \
             so build_index's caller can fall back to a full rebuild"
        );
    }
}
