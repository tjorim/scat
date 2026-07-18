use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::Connection;
use serde_json::Value;
use tracing::{debug, warn};

use crate::core::db::SCHEMA_VERSION;
use crate::core::vc::{ProcessedScript, VcConfig, infer_warnings, scan_checkouts};
use crate::error::{Error, Result};
use crate::indexer::checkpoint::{Checkpoint, write_checkpoint};
use crate::indexer::extractor::extract;
use crate::indexer::scanner::{ScriptRecord, scan_paths_with_revisions};
use crate::indexer::treesitter_deps::TreeSitterExtractor;

use super::IndexResult;

#[allow(clippy::too_many_arguments)]
pub(super) fn populate(
    conn: &mut Connection,
    scan_roots: &[PathBuf],
    logical_prefix: &str,
    head_lines: usize,
    ignore_files: &[PathBuf],
    ts: &mut TreeSitterExtractor,
    result: &mut IndexResult,
    vc_config: &VcConfig,
    db_path: &Path,
    resume_checkpoint: Option<Checkpoint>,
    shutdown: &AtomicBool,
    use_progress: bool,
    dry_run: bool,
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

    // -----------------------------------------------------------------------
    // Resume: determine which records to skip.
    // -----------------------------------------------------------------------
    let already_indexed = resume_checkpoint
        .as_ref()
        .map(|c| c.indexed.clone())
        .unwrap_or_default();

    // Count how many we are actually going to process.
    let to_process: Vec<&ScriptRecord> = records
        .iter()
        .filter(|r| !already_indexed.contains(&r.physical_path))
        .collect();

    // Seed result counters from already-indexed scripts so the final summary
    // is accurate.
    result.scripts_indexed = already_indexed.len();

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

    // RAII transaction: auto-rolls back on drop if not explicitly committed.
    let tx = conn.transaction()?;

    // Track newly indexed paths for checkpointing.
    let mut newly_indexed: HashSet<String> = already_indexed;
    let mut processed = Vec::new();

    // When resuming, count already-indexed scripts by language for accurate progress display.
    let (mut python_count, mut shell_count) = if resume_checkpoint.is_some() {
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

    for record in &to_process {
        // Check for Ctrl-C interrupt.
        if shutdown.load(Ordering::SeqCst) {
            // Flush what we have so far.
            tx.commit()?;
            // Write checkpoint.
            if !dry_run {
                let ckpt = Checkpoint {
                    indexed: newly_indexed.clone(),
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

        match process_script(&tx, record, ts, &build_ts) {
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

    // -----------------------------------------------------------------------
    // Phase 3: Checkouts / warnings
    // -----------------------------------------------------------------------
    let checkouts = scan_checkouts(vc_config, logical_prefix);
    debug!(
        phase = "scan_checkouts",
        checkout_count = checkouts.len(),
        "completed build phase"
    );
    for c in &checkouts {
        tx.execute(
            "INSERT OR REPLACE INTO revisions
             (logical_path, physical_path, revision_type, os_flavor, user, timestamp, age_seconds)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                c.logical_path,
                c.physical_path,
                c.revision_type,
                c.os_flavor,
                c.user,
                c.timestamp,
                c.age_seconds
            ],
        )?;
    }

    apply_checkout_summaries(&tx)?;

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
    for (lp, payload) in warnings_by_path {
        tx.execute(
            "UPDATE scripts SET vc_warnings = ? WHERE logical_path = ?",
            rusqlite::params![serde_json::to_string(&payload)?, lp],
        )?;
    }

    let module_map = super::resolve::build_module_map(&tx)?;
    super::resolve::resolve_dependency_targets(&tx, &module_map)?;
    super::resolve::resolve_reference_targets(&tx)?;
    super::resolve::resolve_function_targets(&tx, &module_map)?;

    tx.execute(
        "INSERT OR REPLACE INTO index_metadata (id, build_timestamp, schema_version)
         VALUES (1, ?1, ?2)",
        rusqlite::params![build_ts, SCHEMA_VERSION],
    )?;

    tx.commit()?;

    Ok(())
}

fn process_script(
    conn: &Connection,
    record: &ScriptRecord,
    ts: &mut TreeSitterExtractor,
    indexed_at: &str,
) -> Result<(ProcessedScript, usize)> {
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

    conn.execute(
        "INSERT OR REPLACE INTO scripts
         (logical_path, language, size, mtime, content,
          owner, purpose, tags, entry_points, related, symlink_target,
          metadata_json, vc_warnings, indexed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        rusqlite::params![
            record.logical_path,
            record.language,
            record.size as i64,
            record.mtime,
            meta.content,
            meta.owner,
            meta.purpose,
            tags_json,
            ep_json,
            related_json,
            record.symlink_target,
            fields_json,
            "[]",
            indexed_at,
        ],
    )?;

    let script_id = conn.last_insert_rowid();

    let deps = extract_deps(record, &meta.content, ts, ast_result.as_ref());
    let dep_count = deps.len();
    for dep in &deps {
        conn.execute(
            "INSERT OR IGNORE INTO dependencies (script_id, depends_on_path) VALUES (?1,?2)",
            rusqlite::params![script_id, dep],
        )?;
    }

    // Path-literal "referenced" edges (a script copied/executed by path, or
    // listed in a manifest). Candidates that don't resolve to an indexed
    // script are dropped later in `resolve_reference_targets`; a script
    // mentioning its own path is not a dependency, so skip it here.
    for reference in crate::indexer::treesitter_deps::extract_reference_paths(&meta.content) {
        if reference == record.logical_path {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO dependencies (script_id, depends_on_path, kind)
             VALUES (?1, ?2, 'referenced')",
            rusqlite::params![script_id, reference],
        )?;
    }

    if let Some(ast) = ast_result {
        for definition in &ast.definitions {
            conn.execute(
                "INSERT OR IGNORE INTO function_definitions
                 (script_id, name, kind, line, docstring, decorators)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    script_id,
                    definition.name,
                    definition.kind,
                    definition.line as i64,
                    definition.docstring,
                    serde_json::to_string(&definition.decorators)?,
                ],
            )?;
        }

        for call in &ast.calls {
            conn.execute(
                "INSERT INTO function_calls
                 (script_id, caller, callee, line, resolved_target_name)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    script_id,
                    call.caller,
                    call.callee,
                    call.line as i64,
                    call.resolved_target,
                ],
            )?;
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
