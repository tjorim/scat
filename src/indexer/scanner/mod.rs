//! File-system scanner and language detection.
//!
//! Scanning is split into: language/shebang detection ([`language`]),
//! ignore-file loading and root-boundary checks ([`filter`]), per-file
//! classification ([`candidate`]), and change detection ([`mtime`]). This
//! module ties them together into the directory walk.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use ignore::WalkBuilder;
use indicatif::ProgressBar;
use rayon::prelude::*;
use tracing::{debug, warn};

use crate::core::vc::CheckoutRecord;
use crate::error::{Error, Result};
use crate::indexer::scan_tree::ScanTreeView;

mod candidate;
mod filter;
mod language;
mod mtime;

pub use language::{detect_language, read_head, shebang_language};
pub use mtime::{max_mtime_in_roots, max_mtime_in_roots_with_shutdown};

use candidate::{CandidateOutcome, SCAN_PROCESS_BATCH_SIZE, ScanCandidate, process_candidate};
use filter::{canonicalize_roots, get_external_ignore_paths, is_within_roots};

#[derive(Debug, Clone)]
/// Script candidate discovered by filesystem scanning.
pub struct ScriptRecord {
    /// Logical catalog path for the script.
    pub logical_path: String,
    /// Absolute or scanned file path on disk.
    pub physical_path: String,
    /// Detected language key (`python`, `shell`, etc.).
    pub language: String,
    /// File size in bytes.
    pub size: u64,
    /// File mtime as UNIX epoch seconds.
    pub mtime: f64,
    /// Optional logical path of resolved symlink target.
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, Default)]
/// Scan output: active scripts discovered during filesystem traversal.
pub struct ScanResult {
    /// Active scripts to index into `scripts`.
    pub scripts: Vec<ScriptRecord>,
    /// Checked-in version copies observed in working directories
    /// (`<script>_<timestamp>` files next to the active symlink), recorded
    /// as `WORKING` revisions. DEVELOP/ARCHIVE revisions are discovered
    /// separately by `scan_checkouts`.
    pub revisions: Vec<CheckoutRecord>,
}

/// Scan roots recursively and return indexable scripts plus WORKING revisions.
///
/// Files inside any directory component named in `checkout_dirs` are skipped;
/// those are handled by the vc checkout scanner, not indexed as scripts.
/// Pass an empty slice to fall back to the default names (`DEVELOP`, `ARCHIVE`).
///
/// Version-named files in the working directories themselves
/// (`<script>_<timestamp>`, the checked-in copies vc keeps next to the active
/// symlink) are returned as `WORKING` revisions rather than indexed as
/// scripts.
pub fn scan_paths_with_revisions(
    roots: &[PathBuf],
    logical_prefix: &str,
    head_lines: usize,
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
    progress: Option<&ProgressBar>,
    shutdown: &AtomicBool,
) -> Result<ScanResult> {
    scan_paths_with_revisions_impl(
        roots,
        logical_prefix,
        head_lines,
        ignore_files,
        checkout_dirs,
        progress,
        None,
        shutdown,
    )
}

/// Like [`scan_paths_with_revisions`], but also feeds a live multi-line
/// directory tree view as the walk progresses — see [`ScanTreeView`]. Used
/// only for interactive manual runs; unattended cron builds pass `None` to
/// [`scan_paths_with_revisions`] instead and never construct one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_paths_with_revisions_and_tree(
    roots: &[PathBuf],
    logical_prefix: &str,
    head_lines: usize,
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
    progress: Option<&ProgressBar>,
    tree: Option<&mut ScanTreeView>,
    shutdown: &AtomicBool,
) -> Result<ScanResult> {
    scan_paths_with_revisions_impl(
        roots,
        logical_prefix,
        head_lines,
        ignore_files,
        checkout_dirs,
        progress,
        tree,
        shutdown,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_paths_with_revisions_impl(
    roots: &[PathBuf],
    logical_prefix: &str,
    head_lines: usize,
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
    progress: Option<&ProgressBar>,
    mut tree: Option<&mut ScanTreeView>,
    shutdown: &AtomicBool,
) -> Result<ScanResult> {
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
    let mut records = Vec::new();
    let mut revisions = Vec::new();
    let mut total_skipped = 0usize;
    let external_ignore_paths = get_external_ignore_paths(ignore_files)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    // Build an Arc'd set of checkout dir names for use in per-root filter_entry
    // closures, which require 'static + Send + Sync.
    let checkout_dirs_set: std::sync::Arc<std::collections::HashSet<String>> = std::sync::Arc::new(
        effective_checkout_dirs
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    );
    // Symlinked directories are only followed if they resolve inside one of
    // these — see `is_within_roots`.
    let canonical_roots: std::sync::Arc<Vec<PathBuf>> =
        std::sync::Arc::new(canonicalize_roots(roots));

    for root in roots {
        let mut found_in_root = 0usize;
        let mut skipped_in_root = 0usize;
        let mut candidates: Vec<ScanCandidate> = Vec::new();
        // Same os_flavor derivation as `scan_checkouts`: the scan_root's
        // parent directory name (e.g. `linux` from /catalog/linux/scripts).
        let os_flavor = root
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let cds = checkout_dirs_set.clone();
        let croots = canonical_roots.clone();
        let mut walk = WalkBuilder::new(root);
        walk.follow_links(true)
            .sort_by_file_path(std::cmp::Ord::cmp)
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .add_custom_ignore_filename(".catignore")
            .filter_entry(move |e| {
                // Prune checkout container directories before descending — avoids
                // traversing large DEVELOP/ARCHIVE trees file by file.
                if e.file_type().is_some_and(|t| t.is_dir()) {
                    let name = e.file_name().to_str().unwrap_or("");
                    if cds.contains(name) {
                        return false;
                    }
                    if e.path_is_symlink() && !is_within_roots(e.path(), &croots) {
                        warn!(
                            path = %e.path().display(),
                            "symlinked directory resolves outside configured scan roots, skipping to avoid unbounded traversal"
                        );
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
        let it = walk.build();

        for entry in it {
            if let Some(pb) = progress {
                pb.inc(1);
            }
            if shutdown.load(Ordering::Relaxed) {
                if let Some(tree) = tree.as_deref_mut() {
                    tree.clear();
                }
                if let Some(pb) = progress {
                    pb.finish_and_clear();
                }
                return Err(Error::Interrupted);
            }

            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    warn!(error = %err, "error traversing directory, skipping");
                    skipped_in_root += 1;
                    total_skipped += 1;
                    continue;
                }
            };

            // Every walked entry lands here (directories included), so this
            // reflects where the walk currently is rather than only the rare
            // entry that both survives every filter below and happens to land
            // on a throttle boundary — showing the directory a file lives in
            // (or the directory itself) is what makes a stalled scan visibly
            // "stuck in" a particular folder instead of just frozen.
            if let Some(pb) = progress
                && pb.position() % 20 == 0
            {
                let display_path = if entry.file_type().is_some_and(|t| t.is_dir()) {
                    entry.path()
                } else {
                    entry.path().parent().unwrap_or_else(|| entry.path())
                };
                pb.set_message(display_path.display().to_string());
                if let Some(tree) = tree.as_deref_mut() {
                    tree.update(root, entry.path());
                }
            }

            let file_type = if let Some(file_type) = entry.file_type() {
                file_type
            } else {
                skipped_in_root += 1;
                total_skipped += 1;
                continue;
            };

            if file_type.is_dir() {
                continue;
            }

            if !file_type.is_file() {
                skipped_in_root += 1;
                total_skipped += 1;
                continue;
            }

            let filepath = entry.path();

            // Editor backups (`prepare_release~`) are not scripts. Without
            // this they would slip through for extensionless names: `foo.sh~`
            // is already dropped below as an unknown extension, but `foo~` has
            // no extension at all and would reach the shebang sniff and index
            // as a duplicate of `foo`.
            if filepath
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.ends_with('~'))
            {
                skipped_in_root += 1;
                total_skipped += 1;
                continue;
            }

            candidates.push(ScanCandidate {
                path: filepath.to_path_buf(),
                is_symlink: entry.path_is_symlink(),
            });
        }

        // The per-candidate work — stat, shebang sniff for extensionless
        // files, symlink resolution — is pure I/O with no shared state
        // (see `process_candidate`), so it runs across a rayon worker pool
        // instead of one file at a time on the thread that just walked the
        // directory tree. Batched, rather than one `par_iter()` over the
        // whole root, so a Ctrl-C during a huge root only has to wait for
        // the in-flight batch rather than the entire root to finish.
        for batch in candidates.chunks(SCAN_PROCESS_BATCH_SIZE) {
            if shutdown.load(Ordering::Relaxed) {
                if let Some(tree) = tree.as_deref_mut() {
                    tree.clear();
                }
                if let Some(pb) = progress {
                    pb.finish_and_clear();
                }
                return Err(Error::Interrupted);
            }

            let outcomes: Vec<CandidateOutcome> = batch
                .par_iter()
                .map(|c| {
                    process_candidate(
                        &c.path,
                        c.is_symlink,
                        root,
                        logical_prefix,
                        head_lines,
                        &os_flavor,
                        now,
                    )
                })
                .collect();

            for outcome in outcomes {
                match outcome {
                    CandidateOutcome::Record(record) => {
                        records.push(record);
                        found_in_root += 1;
                    }
                    CandidateOutcome::Revision(revision) => revisions.push(revision),
                    CandidateOutcome::Skip => {
                        skipped_in_root += 1;
                        total_skipped += 1;
                    }
                }
            }
        }

        debug!(
            root = %root.display(),
            found_count = found_in_root,
            skipped_count = skipped_in_root,
            "completed scan root"
        );
    }

    debug!(
        found_count = records.len(),
        skipped_count = total_skipped,
        "completed file scan"
    );

    Ok(ScanResult {
        scripts: records,
        revisions,
    })
}

/// Scan roots recursively and return indexable active script records.
pub fn scan_paths(
    roots: &[PathBuf],
    logical_prefix: &str,
    head_lines: usize,
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
    progress: Option<&ProgressBar>,
    shutdown: &AtomicBool,
) -> Result<Vec<ScriptRecord>> {
    Ok(scan_paths_with_revisions(
        roots,
        logical_prefix,
        head_lines,
        ignore_files,
        checkout_dirs,
        progress,
        shutdown,
    )?
    .scripts)
}

#[cfg(test)]
mod tests;
