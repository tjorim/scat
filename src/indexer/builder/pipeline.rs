use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rusqlite::Connection;
use serde_json::Value;
use tracing::{debug, warn};

use crate::core::db::SCHEMA_VERSION;
use crate::core::vc::{ProcessedScript, VcConfig, infer_warnings, scan_checkouts};
use crate::error::{Error, Result};
use crate::indexer::ast_deps::AstDependencies;
use crate::indexer::checkpoint::{Checkpoint, write_checkpoint};
use crate::indexer::extractor::{ExtractedMetadata, extract};
use crate::indexer::scanner::{ScriptRecord, scan_paths_with_revisions};
use crate::indexer::treesitter_deps::TreeSitterExtractor;

use super::IndexResult;

/// Number of scripts extracted per parallel batch in phase 2. Bounds how
/// long a Ctrl-C or progress-bar update has to wait — extraction within a
/// batch runs across all cores, but batches themselves run one at a time —
/// while still giving each batch enough work to spread well across cores.
const EXTRACT_BATCH_SIZE: usize = 200;

thread_local! {
    // One TreeSitterExtractor per worker thread, reused across every batch
    // for that thread's lifetime rather than rebuilt (two fresh parsers)
    // for every batch — `map_init` would otherwise re-run its init closure
    // once per batch per thread, since cached `map_init` state doesn't
    // persist across separate `.par_iter()` calls.
    static EXTRACTOR: std::cell::RefCell<TreeSitterExtractor> = std::cell::RefCell::new(
        TreeSitterExtractor::new().expect("failed to initialize tree-sitter extractor")
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn populate(
    conn: &mut Connection,
    scan_roots: &[PathBuf],
    logical_prefix: &str,
    head_lines: usize,
    ignore_files: &[PathBuf],
    result: &mut IndexResult,
    vc_config: &VcConfig,
    db_path: &Path,
    resume_checkpoint: Option<Checkpoint>,
    shutdown: &AtomicBool,
    use_progress: bool,
    dry_run: bool,
    incremental: bool,
) -> Result<()> {
    let build_ts = chrono::Utc::now().to_rfc3339();

    // -----------------------------------------------------------------------
    // Phase 1: Scan
    // -----------------------------------------------------------------------
    let scan_pb = if use_progress {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} Scanning…  [{elapsed_precise}]  {pos} files  ({per_sec})  {msg}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(80));
        Some(pb)
    } else {
        None
    };

    debug!(
        phase = "scan_paths",
        root_count = scan_roots.len(),
        "starting scan_paths phase"
    );
    let checkout_dirs: Vec<&str> = vc_config.all_checkout_dirs().collect();
    let scan_result = scan_paths_with_revisions(
        scan_roots,
        logical_prefix,
        head_lines,
        ignore_files,
        &checkout_dirs,
        scan_pb.as_ref(),
        shutdown,
    )?;
    let records = scan_result.scripts;
    let total = records.len();
    debug!(
        phase = "scan_paths",
        script_count = total,
        "completed build phase"
    );

    if let Some(pb) = &scan_pb {
        pb.finish_and_clear();
    }

    // RAII transaction: auto-rolls back on drop if not explicitly committed.
    // Opened here (rather than just before phase 2) so the incremental-seed
    // diff below can query the database it's about to build on top of.
    let tx = conn.transaction()?;

    // Firing the FTS-sync triggers once per row during a bulk insert is far
    // slower than a single bulk rebuild once all rows are in place; dropped
    // here, restored (along with the bulk FTS rebuild) at the end of phase 3.
    // Idempotent, so safe to re-run on a resumed build where they're already
    // dropped from the interrupted attempt.
    tx.execute_batch(crate::core::db::DROP_FTS_TRIGGERS_SQL)?;

    // -----------------------------------------------------------------------
    // Resume / incremental: determine which records to skip.
    // -----------------------------------------------------------------------
    // A resume checkpoint (mid-build interrupt) and incremental seeding
    // (starting from the previous completed build — see
    // `builder::build_index`) both mean "some of `records` don't need
    // re-extraction"; they share `already_indexed` since only one applies on
    // any given run (incremental seeding only happens on a fresh, non-resume
    // attempt — see the caller).
    let mut already_indexed = resume_checkpoint
        .as_ref()
        .map(|c| c.indexed.clone())
        .unwrap_or_default();

    if incremental {
        seed_from_existing_database(&tx, &records, &mut already_indexed)?;
    }

    // Count how many we are actually going to process.
    let to_process: Vec<&ScriptRecord> = records
        .iter()
        .filter(|r| !already_indexed.contains(&r.physical_path))
        .collect();

    // Seed result counters from already-indexed scripts so the final summary
    // is accurate.
    result.scripts_indexed = already_indexed.len();
    result.scripts_reused = already_indexed.len();

    // -----------------------------------------------------------------------
    // Phase 2: Extract / index
    // -----------------------------------------------------------------------
    let index_pb = if use_progress {
        let pb = ProgressBar::new(to_process.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.cyan} Indexing… {bar:30.cyan/blue}  {pos:>5}/{len:5} scripts  [{elapsed_precise} / ~{eta}]  {msg}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(80));
        Some(pb)
    } else {
        None
    };

    // Track newly indexed paths for checkpointing.
    let mut newly_indexed: HashSet<String> = already_indexed;
    let mut processed = Vec::new();

    // When resuming or building incrementally, `scripts` already has rows
    // from before this run started, so seed the display counters from it —
    // otherwise the "python N shell M" progress message would undercount.
    let (mut python_count, mut shell_count) = if resume_checkpoint.is_some() || incremental {
        let mut py_count = 0usize;
        let mut sh_count = 0usize;
        {
            let mut stmt =
                tx.prepare("SELECT language, COUNT(*) FROM scripts GROUP BY language")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let lang: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                match lang.as_str() {
                    "python" => py_count = count as usize,
                    "shell" => sh_count = count as usize,
                    _ => {}
                }
            }
        }
        (py_count, sh_count)
    } else {
        (0, 0)
    };

    for batch in to_process.chunks(EXTRACT_BATCH_SIZE) {
        // Check for Ctrl-C interrupt before starting the next batch's
        // (parallel, not itself interruptible mid-flight) extraction.
        if shutdown.load(Ordering::SeqCst) {
            checkpoint_for_resume(tx, db_path, result, newly_indexed, dry_run)?;
            return Err(Error::Interrupted);
        }

        // The CPU-bound work — file read, tree-sitter parse, regex
        // extraction — is independent per script, so it runs in parallel
        // across a worker pool (one TreeSitterExtractor per worker thread,
        // cached in EXTRACTOR). SQLite writes stay strictly sequential on
        // `tx` below, since SQLite only supports one writer at a time.
        let extracted: Vec<(&ScriptRecord, Result<ExtractedRow<'_>>)> = batch
            .par_iter()
            .map(|record| {
                let record: &ScriptRecord = record;
                let row = EXTRACTOR.with(|ts| extract_for_insert(record, &mut ts.borrow_mut()));
                (record, row)
            })
            .collect();

        for (record, extraction) in extracted {
            // Check for Ctrl-C interrupt.
            if shutdown.load(Ordering::SeqCst) {
                checkpoint_for_resume(tx, db_path, result, newly_indexed, dry_run)?;
                return Err(Error::Interrupted);
            }

            if let Some(pb) = &index_pb {
                pb.set_message(format!(
                    "python {} shell {}  errors {}",
                    python_count,
                    shell_count,
                    result.errors.len()
                ));
            }

            let outcome = extraction.and_then(|row| insert_extracted_row(&tx, &row, &build_ts));
            match outcome {
                Ok((ps, dep_count)) => {
                    result.scripts_indexed += 1;
                    result.dependencies_indexed += dep_count;
                    newly_indexed.insert(record.physical_path.clone());
                    match record.language.as_str() {
                        "python" => python_count += 1,
                        "shell" => shell_count += 1,
                        _ => {}
                    }
                    processed.push(ps);
                }
                Err(e) => {
                    result
                        .errors
                        .push((record.physical_path.clone(), e.to_string()));
                }
            }

            if let Some(pb) = &index_pb {
                pb.inc(1);
            }
        }
    }

    if let Some(pb) = &index_pb {
        pb.finish_and_clear();
    }

    debug!(
        phase = "process_scripts",
        script_count = result.scripts_indexed,
        dependency_count = result.dependencies_indexed,
        error_count = result.errors.len(),
        "completed build phase"
    );

    // Ctrl-C between phase 2 and phase 3 checkpoints the same way an in-loop
    // interrupt does, so a resumed run can skip straight back into phase 3
    // instead of waiting for phase 3 (which has no per-record progress of
    // its own) to run to completion first.
    if shutdown.load(Ordering::SeqCst) {
        checkpoint_for_resume(tx, db_path, result, newly_indexed, dry_run)?;
        return Err(Error::Interrupted);
    }

    // -----------------------------------------------------------------------
    // Phase 3: Checkouts / warnings / dependency resolution
    // -----------------------------------------------------------------------
    // Unlike phases 1-2, this phase has no per-record progress to report —
    // scan_checkouts, infer_warnings, and the resolve_* passes each run as a
    // single pass over the whole catalog. On a large catalog this can take
    // real time; without a spinner and staged messages a run in progress is
    // indistinguishable from a hung one, especially since the phase 2 bar
    // was just cleared.
    let finalize_pb = if use_progress {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}  [{elapsed_precise}]")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(80));
        Some(pb)
    } else {
        None
    };
    let set_finalize_msg = |msg: &str| {
        if let Some(pb) = &finalize_pb {
            pb.set_message(msg.to_string());
        }
    };

    set_finalize_msg("Recording checkouts…");
    debug!(phase = "scan_checkouts", "starting scan_checkouts phase");
    let checkouts = scan_checkouts(vc_config, logical_prefix);
    debug!(
        phase = "scan_checkouts",
        checkout_count = checkouts.len(),
        "completed build phase"
    );
    // `scan_checkouts` always returns the complete current set (it isn't
    // itself incremental), so clearing first and re-inserting fresh is
    // simpler than diffing — and correct either way, since a stale
    // checkout revision must not survive after its DEVELOP/ARCHIVE entry is
    // gone (a no-op on the always-fresh-db path, where this table starts
    // empty anyway).
    tx.execute("DELETE FROM revisions", [])?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO revisions
             (logical_path, physical_path, revision_type, os_flavor, user, timestamp, age_seconds)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
        )?;
        for c in &checkouts {
            stmt.execute(rusqlite::params![
                c.logical_path,
                c.physical_path,
                c.revision_type,
                c.os_flavor,
                c.user,
                c.timestamp,
                c.age_seconds
            ])?;
        }
    }

    // Reset derived checkout/warning state before re-deriving it below —
    // otherwise a script whose checkout was cleaned up, or whose warning no
    // longer applies, would keep showing stale state forever on an
    // incrementally-seeded database (a no-op on the always-fresh-db path,
    // where every row was just inserted with these already at their default).
    tx.execute(
        "UPDATE scripts SET checkout_user = NULL, checkout_timestamp = NULL,
         checkout_os = NULL, checkout_age_seconds = NULL, vc_warnings = '[]'",
        [],
    )?;

    apply_checkout_summaries(&tx)?;

    set_finalize_msg("Inferring warnings…");
    debug!(phase = "infer_warnings", "starting infer_warnings phase");
    let warnings = infer_warnings(&tx)?;
    debug!(
        phase = "infer_warnings",
        warning_count = warnings.len(),
        "completed build phase"
    );
    let mut warnings_by_path: HashMap<String, Vec<Value>> = HashMap::new();
    for warning in warnings {
        let mut entry = serde_json::Map::new();
        entry.insert("kind".into(), Value::String(warning.kind));
        entry.insert("message".into(), Value::String(warning.message));
        entry.insert("details".into(), Value::Object(warning.details));
        warnings_by_path
            .entry(warning.logical_path)
            .or_default()
            .push(Value::Object(entry));
    }
    {
        let mut stmt = tx.prepare("UPDATE scripts SET vc_warnings = ? WHERE logical_path = ?")?;
        for (lp, payload) in warnings_by_path {
            stmt.execute(rusqlite::params![serde_json::to_string(&payload)?, lp])?;
        }
    }

    set_finalize_msg("Resolving dependencies…");
    debug!(
        phase = "resolve_dependencies",
        "starting dependency resolution phase"
    );
    let module_map = super::resolve::build_module_map(&tx)?;
    super::resolve::resolve_dependency_targets(&tx, &module_map)?;
    super::resolve::resolve_reference_targets(&tx)?;
    super::resolve::resolve_function_targets(&tx, &module_map)?;
    debug!(
        phase = "resolve_dependencies",
        "completed dependency resolution phase"
    );

    set_finalize_msg("Rebuilding search index…");
    debug!(phase = "rebuild_fts", "starting FTS rebuild phase");
    tx.execute_batch(crate::core::db::REBUILD_FTS_AND_RESTORE_TRIGGERS_SQL)?;
    debug!(phase = "rebuild_fts", "completed FTS rebuild phase");

    tx.execute(
        "INSERT OR REPLACE INTO index_metadata (id, build_timestamp, schema_version)
         VALUES (1, ?1, ?2)",
        rusqlite::params![build_ts, SCHEMA_VERSION],
    )?;

    if let Some(pb) = &finalize_pb {
        pb.finish_and_clear();
    }

    tx.commit()?;

    Ok(())
}

/// Commit the in-progress transaction and write a resume checkpoint so a
/// subsequent run can skip already-indexed scripts. Used both when Ctrl-C
/// fires mid-loop in phase 2 and when it fires between phase 2 and phase 3.
fn checkpoint_for_resume(
    tx: rusqlite::Transaction<'_>,
    db_path: &Path,
    result: &IndexResult,
    newly_indexed: HashSet<String>,
    dry_run: bool,
) -> Result<()> {
    tx.commit()?;
    if !dry_run {
        let ckpt = Checkpoint {
            indexed: newly_indexed,
        };
        if let Err(e) = write_checkpoint(db_path, &ckpt) {
            warn!(error = %e, "failed to write resume checkpoint — progress cannot be resumed");
        }
        // Rename tmp_path → wip_path so the resume logic can find it.
        let wip = crate::indexer::checkpoint::wip_path(db_path);
        if result.db_path != wip
            && let Err(e) = std::fs::rename(&result.db_path, &wip)
        {
            warn!(error = %e, "failed to rename WIP database — resume may not work");
        }
    }
    Ok(())
}

/// For an incrementally-seeded database (started from a copy of the
/// previous completed build — see `builder::build_index`), find scripts
/// already present with matching size and mtime and add their physical
/// paths to `already_indexed`, same as a resume checkpoint would, so
/// phase 2 skips re-extracting them.
///
/// Scripts present in the database but no longer found by this scan are
/// deleted outright. This is what makes the rest of the pipeline safe to
/// leave untouched: the schema's `ON DELETE SET NULL` foreign keys
/// (`dependencies.resolved_script_id`, `function_calls.resolved_target_*`)
/// null out anything that referenced a deleted script, and phase 3's
/// resolve passes already only touch rows where the resolved id `IS NULL`
/// — so a dependency that pointed at a script which just got replaced (new
/// content, new row, new id) or removed correctly gets re-resolved against
/// current state without any incremental-specific resolve logic.
fn seed_from_existing_database(
    tx: &Connection,
    records: &[ScriptRecord],
    already_indexed: &mut HashSet<String>,
) -> Result<()> {
    let mut existing: HashMap<String, (i64, f64)> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT logical_path, size, mtime FROM scripts")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let logical_path: String = row.get(0)?;
            let size: i64 = row.get(1)?;
            let mtime: f64 = row.get(2)?;
            existing.insert(logical_path, (size, mtime));
        }
    }

    let current_paths: HashSet<&str> = records.iter().map(|r| r.logical_path.as_str()).collect();
    let removed: Vec<&String> = existing
        .keys()
        .filter(|lp| !current_paths.contains(lp.as_str()))
        .collect();
    if !removed.is_empty() {
        let mut stmt = tx.prepare("DELETE FROM scripts WHERE logical_path = ?1")?;
        for logical_path in removed {
            stmt.execute(rusqlite::params![logical_path])?;
        }
    }

    for record in records {
        if existing.get(&record.logical_path) == Some(&(record.size as i64, record.mtime)) {
            already_indexed.insert(record.physical_path.clone());
        }
    }

    Ok(())
}

/// Everything `extract_for_insert` computes for one script — pure, so it can
/// run on any worker thread. `insert_extracted_row` writes this into the
/// database sequentially afterward.
struct ExtractedRow<'a> {
    record: &'a ScriptRecord,
    meta: ExtractedMetadata,
    ast_result: Option<AstDependencies>,
    deps: Vec<String>,
    references: Vec<String>,
    tags_json: String,
    ep_json: String,
    related_json: String,
    fields_json: String,
}

/// File read, parsing, and dependency/reference extraction for one script.
/// No database access — safe to call from any worker thread.
fn extract_for_insert<'a>(
    record: &'a ScriptRecord,
    ts: &mut TreeSitterExtractor,
) -> Result<ExtractedRow<'a>> {
    let meta = extract(record);
    let ast_result = if record.language == "python" {
        let module_name = super::resolve::module_name_from_logical_path(&record.logical_path);
        Some(ts.extract_python_ast(&meta.content, Some(&module_name)))
    } else {
        None
    };

    let tags_json = serde_json::to_string(&meta.tags)?;
    let ep_json = serde_json::to_string(&meta.entry_points)?;
    let related_json = serde_json::to_string(&meta.related)?;
    let fields_json = serde_json::to_string(&meta.fields)?;

    let deps = extract_deps(record, &meta.content, ts, ast_result.as_ref());

    // Path-literal "referenced" edges (a script copied/executed by path, or
    // listed in a manifest). Candidates that don't resolve to an indexed
    // script are dropped later in `resolve_reference_targets`; a script
    // mentioning its own path is not a dependency, so skip it here.
    let references = crate::indexer::treesitter_deps::extract_reference_paths(&meta.content)
        .into_iter()
        .filter(|reference| reference != &record.logical_path)
        .collect();

    Ok(ExtractedRow {
        record,
        meta,
        ast_result,
        deps,
        references,
        tags_json,
        ep_json,
        related_json,
        fields_json,
    })
}

/// Writes one already-extracted script into the database. Must run
/// sequentially against `conn` — SQLite allows only one writer at a time.
fn insert_extracted_row(
    conn: &Connection,
    row: &ExtractedRow<'_>,
    indexed_at: &str,
) -> Result<(ProcessedScript, usize)> {
    let record = row.record;

    conn.prepare_cached(
        "INSERT OR REPLACE INTO scripts
         (logical_path, language, size, mtime, content,
          owner, purpose, tags, entry_points, related, symlink_target,
          metadata_json, vc_warnings, indexed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
    )?
    .execute(rusqlite::params![
        record.logical_path,
        record.language,
        record.size as i64,
        record.mtime,
        row.meta.content,
        row.meta.owner,
        row.meta.purpose,
        row.tags_json,
        row.ep_json,
        row.related_json,
        record.symlink_target,
        row.fields_json,
        "[]",
        indexed_at,
    ])?;

    let script_id = conn.last_insert_rowid();

    let dep_count = row.deps.len();
    for dep in &row.deps {
        conn.prepare_cached(
            "INSERT OR IGNORE INTO dependencies (script_id, depends_on_path) VALUES (?1,?2)",
        )?
        .execute(rusqlite::params![script_id, dep])?;
    }

    for reference in &row.references {
        conn.prepare_cached(
            "INSERT OR IGNORE INTO dependencies (script_id, depends_on_path, kind)
             VALUES (?1, ?2, 'referenced')",
        )?
        .execute(rusqlite::params![script_id, reference])?;
    }

    if let Some(ast) = &row.ast_result {
        for definition in &ast.definitions {
            conn.prepare_cached(
                "INSERT OR IGNORE INTO function_definitions
                 (script_id, name, kind, line, docstring, decorators)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(rusqlite::params![
                script_id,
                definition.name,
                definition.kind,
                definition.line as i64,
                definition.docstring,
                serde_json::to_string(&definition.decorators)?,
            ])?;
        }

        for call in &ast.calls {
            conn.prepare_cached(
                "INSERT INTO function_calls
                 (script_id, caller, callee, line, resolved_target_name)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(rusqlite::params![
                script_id,
                call.caller,
                call.callee,
                call.line as i64,
                call.resolved_target,
            ])?;
        }
    }

    let ps = ProcessedScript {
        logical_path: record.logical_path.clone(),
        physical_path: record.physical_path.clone(),
        symlink_target: record.symlink_target.clone(),
        mtime: record.mtime,
    };

    Ok((ps, dep_count))
}

fn extract_deps(
    record: &ScriptRecord,
    content: &str,
    ts: &mut TreeSitterExtractor,
    ast_result: Option<&crate::indexer::ast_deps::AstDependencies>,
) -> Vec<String> {
    if record.language == "python"
        && let Some(ast) = ast_result
    {
        return ast.imports.clone();
    }
    ts.extract_deps(content, &record.language)
}

fn apply_checkout_summaries(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE scripts
         SET checkout_user      = s.users,
             checkout_timestamp = s.newest_timestamp,
             checkout_os        = s.os_flavors,
             checkout_age_seconds = s.max_age
         FROM (
             SELECT logical_path,
                    GROUP_CONCAT(DISTINCT user)      AS users,
                    MAX(timestamp)                   AS newest_timestamp,
                    GROUP_CONCAT(DISTINCT os_flavor) AS os_flavors,
                    MAX(age_seconds)                 AS max_age
             FROM revisions
             WHERE revision_type = 'DEVELOP'
             GROUP BY logical_path
         ) AS s
         WHERE scripts.logical_path = s.logical_path",
        [],
    )?;
    Ok(())
}
