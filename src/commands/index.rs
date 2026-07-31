use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use scat_core::indexer::builder::{BuildOptions, build_index};
use scat_core::indexer::scanner::max_mtime_in_roots_with_shutdown;
use tracing::warn;

use crate::output::print_json;

// Each bool is an independent CLI flag passed straight through from clap;
// grouping them into a struct would just move the same list one level over.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub fn cmd_index(
    scan_roots: &[PathBuf],
    db_path: &Path,
    logical_prefix: &str,
    head_lines: usize,
    ignore_files: &[PathBuf],
    keep_copies: usize,
    dry_run: bool,
    config: scat_core::core::vc::VcConfig,
    json: bool,
    quiet: bool,
    no_resume: bool,
    force: bool,
    no_incremental: bool,
    threads: Option<usize>,
) -> Result<()> {
    let effective_scan_roots: Vec<PathBuf> = if scan_roots.is_empty() {
        config.scan_roots.clone()
    } else {
        scan_roots.to_vec()
    };
    if effective_scan_roots.is_empty() {
        anyhow::bail!(
            "No scan roots specified. Use --scan-root or add scan_roots to the config file."
        );
    }

    // Start with CLI-supplied ignore files, then append a temp file for any
    // inline patterns from the config.  The temp file must stay alive until
    // after build_index returns, so we keep it in scope here.
    let mut effective_ignore: Vec<PathBuf> = ignore_files.to_vec();
    let _config_ignore_tmp: Option<tempfile::NamedTempFile> = if config.ignore_patterns.is_empty() {
        None
    } else {
        use std::io::Write;
        let mut tmp =
            tempfile::NamedTempFile::new().with_context(|| "Failed to create temp ignore file")?;
        for pattern in &config.ignore_patterns {
            writeln!(tmp, "{pattern}")?;
        }
        effective_ignore.push(tmp.path().to_path_buf());
        Some(tmp)
    };

    // Register Ctrl-C handler at the application entry point. The same signal
    // is checked by pre-build change detection and the index build itself.
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let shutdown_clone = std::sync::Arc::clone(&shutdown);
        let _ = ctrlc::set_handler(move || {
            shutdown_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }

    // ---------------------------------------------------------------------------
    // Change-detection: skip rebuild when nothing has changed.
    // ---------------------------------------------------------------------------
    if !force
        && !dry_run
        && db_path.exists()
        && let Some(indexed_at_secs) = read_indexed_at(db_path)
    {
        let checkout_dirs: Vec<&str> = config.all_checkout_dirs().collect();
        let max_mtime = max_mtime_in_roots_with_shutdown(
            &effective_scan_roots,
            &effective_ignore,
            &checkout_dirs,
            &shutdown,
        )
        .with_context(|| "Failed to check scan root modification times")?;
        if should_skip_catalog_rebuild(indexed_at_secs, max_mtime) {
            if json {
                print_json(&serde_json::json!({
                    "up_to_date": true,
                    "db_path": db_path.display().to_string(),
                }));
            } else {
                println!("catalog is up to date — skipping rebuild");
            }
            return Ok(());
        }
    }

    let opts = BuildOptions {
        logical_prefix: logical_prefix.to_string(),
        head_lines,
        ignore_files: effective_ignore,
        keep_copies,
        dry_run,
        vc_config: Some(config),
        quiet: quiet || json,
        no_resume,
        no_incremental,
        shutdown: Some(shutdown),
        threads,
    };

    let result = build_index(&effective_scan_roots, db_path, opts)
        .with_context(|| format!("Failed to build index at {}", db_path.display()))?;

    if json {
        print_json(&serde_json::json!({
            "scripts_indexed": result.scripts_indexed,
            "scripts_reused": result.scripts_reused,
            "dependencies_indexed": result.dependencies_indexed,
            "db_path": result.db_path.display().to_string(),
            "dry_run": result.dry_run,
            "errors": result.errors.iter().map(|(p, e)| serde_json::json!({"path": p, "error": e})).collect::<Vec<_>>(),
        }));
    } else {
        let reused_suffix = if result.scripts_reused > 0 {
            format!(" ({} reused unchanged)", result.scripts_reused)
        } else {
            String::new()
        };
        println!(
            "Indexed {} scripts in total{} ({} dependencies) → {}{}",
            result.scripts_indexed,
            reused_suffix,
            result.dependencies_indexed,
            result.db_path.display(),
            if dry_run { " (dry run)" } else { "" }
        );
        if !result.errors.is_empty() {
            warn!(
                error_count = result.errors.len(),
                "file(s) failed during indexing"
            );
            for (path, err) in &result.errors {
                warn!(path = %path, error = %err, "indexing file failed");
            }
        }
    }
    Ok(())
}

pub fn should_skip_catalog_rebuild(indexed_at_secs: f64, max_mtime: Option<f64>) -> bool {
    max_mtime.is_some_and(|mtime| mtime.floor() < indexed_at_secs)
}

/// Open the existing catalog database and return the `build_timestamp` as UNIX
/// epoch seconds, or `None` if the DB cannot be read, has no metadata row, or
/// has a schema-version mismatch (all of which should trigger a rebuild).
fn read_indexed_at(db_path: &Path) -> Option<f64> {
    use scat_core::core::db::{SCHEMA_VERSION, open_readonly};

    let conn = open_readonly(db_path).ok()?;
    let (ts, schema_ver): (Option<String>, i64) = conn
        .query_row(
            "SELECT build_timestamp, schema_version FROM index_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;

    // Schema mismatch → always rebuild.
    if schema_ver != SCHEMA_VERSION {
        return None;
    }

    let ts = ts?;
    chrono::DateTime::parse_from_rfc3339(&ts)
        .ok()
        .map(|dt| dt.timestamp() as f64)
}
