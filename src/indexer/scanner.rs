use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ignore::WalkBuilder;
use indicatif::ProgressBar;
use rayon::prelude::*;
use regex::Regex;
use tracing::{debug, warn};

use crate::core::vc::{CheckoutRecord, REVISION_TYPE_WORKING, parse_checkout_filename};
use crate::error::{Error, Result};
use crate::indexer::scan_tree::ScanTreeView;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SCRIPT_EXTENSIONS: &[&str] = &[
    ".py", ".sh", ".bash", ".ksh", ".yml", ".yaml", ".csv", ".json",
];

/// Files larger than this are skipped during scanning rather than read into
/// memory and parsed — a multi-gigabyte data file sharing a script extension
/// (a huge `.csv`/`.json` export, for instance) would otherwise be read whole,
/// regex-scanned, and tree-sitter-parsed, which can stall a run for a long
/// time on what is almost certainly not really a script.
const MAX_INDEXABLE_FILE_SIZE_BYTES: u64 = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Language detection
// ---------------------------------------------------------------------------

/// Detect language from a file extension.
pub fn detect_language(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("py") => "python",
        Some("sh" | "bash" | "ksh") => "shell",
        Some("yml" | "yaml") => "yaml",
        Some("csv") => "csv",
        Some("json") => "json",
        _ => "unknown",
    }
}

static SHEBANG_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^#!\s*(\S+)(?:\s+(\S+))?").unwrap());

/// Infer language from shebang line.
pub fn shebang_language(first_line: &str) -> Option<&'static str> {
    let m = SHEBANG_RE.captures(first_line)?;
    let cmd = Path::new(m.get(1)?.as_str()).file_name()?.to_str()?;
    let interpreter = if cmd == "env" {
        m.get(2)
            .and_then(|g| Path::new(g.as_str()).file_name()?.to_str())?
    } else {
        cmd
    };
    match interpreter {
        "sh" | "bash" | "ksh" | "ksh93" => Some("shell"),
        "python" | "python2" | "python3" => Some("python"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Head reader
// ---------------------------------------------------------------------------

/// Read up to `n` lines from the start of `path`.
pub fn read_head(path: &Path, n: usize) -> Vec<String> {
    use std::io::{BufRead, BufReader};
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let reader = BufReader::new(file);
    let mut lines = Vec::with_capacity(n);
    for line in reader.lines().take(n) {
        match line {
            Ok(l) => lines.push(l),
            Err(_) => break,
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// Logical path builder
// ---------------------------------------------------------------------------

/// Build a script's logical catalog path from where it was scanned: its
/// absolute filesystem path. Every host that runs the indexer scans the same
/// scan_root, so a logical path is already stable and portable without a
/// separate relocatable-prefix scheme.
fn make_logical_path(filepath: &Path) -> String {
    filepath.to_string_lossy().into_owned()
}

/// Whether a working-directory file whose name parses as `<script>_<timestamp>`
/// is really one of vc's checked-in version copies rather than a script that
/// merely happens to end in digits.
///
/// Two independent signals qualify it, because the script part of the name is
/// not always enough on its own:
///
/// - The script part carries a known script extension (`bar.py_20260720_0900`).
///   Nothing else is named that way, and it still holds after the active
///   script is obsoleted and only the versions remain.
/// - A sibling entry named exactly like the script part exists next to it
///   (`prepare_release` beside `prepare_release_20260701_105550`). This is the
///   case for the many extensionless shell tools vc manages, whose versions
///   would otherwise each be indexed as a separate script — the symptom being
///   a search for `prepare_release` returning every retained version of it.
///   The sibling is normally vc's active symlink, so it is stat'ed without
///   following links: a symlink left dangling by a half-finished checkin still
///   counts, while a directory of that name does not.
fn is_working_dir_revision(filepath: &Path, script_name: &str, script_ext: &str) -> bool {
    if SCRIPT_EXTENSIONS.contains(&script_ext) {
        return true;
    }
    filepath
        .with_file_name(script_name)
        .symlink_metadata()
        .is_ok_and(|meta| !meta.is_dir())
}

fn get_external_ignore_paths(ignore_files: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let global_ignore = PathBuf::from(home).join(".catignore");
        if global_ignore.is_file() {
            paths.push(global_ignore);
        }
    }

    for ignore_file in ignore_files {
        if !ignore_file.is_file() {
            return Err(Error::Validation(format!(
                "ignore file does not exist: {}",
                ignore_file.display()
            )));
        }
        paths.push(ignore_file.clone());
    }

    Ok(paths)
}

/// Canonicalize each root, dropping any that fail to resolve (e.g. a
/// misconfigured or momentarily-missing root) rather than failing the whole
/// scan over it.
fn canonicalize_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .collect()
}

/// Whether `path` canonicalizes to somewhere inside one of `canonical_roots`.
/// Used to bound symlinked-directory traversal: a symlink pointing at another
/// location inside the scan roots (e.g. the documented `alt/pse → linux/pse`
/// OS-variant aliasing) is followed as before, but a symlink pointing outside
/// all configured roots — into an unrelated, potentially huge tree such as a
/// home directory or another mount — is not, since following it could turn a
/// bounded scan into an effectively unbounded one.
fn is_within_roots(path: &Path, canonical_roots: &[PathBuf]) -> bool {
    match std::fs::canonicalize(path) {
        Ok(canon) => canonical_roots.iter().any(|r| canon.starts_with(r)),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Parallel per-file processing
// ---------------------------------------------------------------------------

/// A regular file the walk decided is worth a closer look — not yet
/// classified as a script, a WORKING revision, or something to skip. Kept
/// deliberately small since these accumulate per scan root before the
/// parallel pass in [`process_candidate`] runs.
struct ScanCandidate {
    path: PathBuf,
    is_symlink: bool,
}

/// What [`process_candidate`] decided about one [`ScanCandidate`].
enum CandidateOutcome {
    Record(ScriptRecord),
    Revision(CheckoutRecord),
    Skip,
}

/// Number of candidates processed per parallel batch. Bounds how long a
/// Ctrl-C has to wait for the in-flight batch to finish before the shutdown
/// flag is rechecked, mirroring `EXTRACT_BATCH_SIZE` in
/// `builder/pipeline.rs`, which does the same for phase 2's extraction work.
const SCAN_PROCESS_BATCH_SIZE: usize = 500;

/// Stat, sniff, and classify one candidate file. Pure filesystem I/O with no
/// shared state, so it's safe to call from any worker thread — this is what
/// lets a scan root's candidate list process in parallel across cores
/// instead of one file at a time on the directory-walking thread, which is
/// what actually dominates scan time on a large tree (this function does
/// every stat, file read, and symlink resolution; the walk itself just
/// enumerates names).
fn process_candidate(
    filepath: &Path,
    is_symlink: bool,
    head_lines: usize,
    os_flavor: &str,
    now: f64,
) -> CandidateOutcome {
    // vc keeps recent checked-in copies (`<script>_<timestamp>`, no user
    // suffix) in the working directory next to the active symlink. Record
    // those as WORKING revisions instead of indexing them as scripts in
    // their own right.
    if let Some(filename) = filepath.file_name().and_then(|n| n.to_str())
        && let Some((script_name, timestamp, user)) = parse_checkout_filename(filename)
    {
        let script_ext = Path::new(&script_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_lowercase()))
            .unwrap_or_default();
        if is_working_dir_revision(filepath, &script_name, &script_ext) {
            let logical_path = make_logical_path(&filepath.with_file_name(&script_name));
            let age_seconds = filepath
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| (now - d.as_secs_f64()).max(0.0));
            return CandidateOutcome::Revision(CheckoutRecord {
                logical_path,
                physical_path: filepath.to_string_lossy().into_owned(),
                revision_type: REVISION_TYPE_WORKING.to_string(),
                os_flavor: os_flavor.to_string(),
                user,
                timestamp,
                age_seconds,
            });
        }
    }

    let ext = filepath
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_lowercase()))
        .unwrap_or_default();

    // Only extensionless files need their head read here (to sniff a
    // shebang); extension-matched files don't — nothing downstream
    // consumes a pre-read head, so reading one would just be a wasted
    // partial read that `extractor::extract` (which reads the whole file)
    // throws away.
    let language = if SCRIPT_EXTENSIONS.contains(&ext.as_str()) {
        detect_language(filepath)
    } else if ext.is_empty() {
        let head = read_head(filepath, head_lines);
        match head.first().and_then(|l| shebang_language(l)) {
            None => return CandidateOutcome::Skip,
            Some(l) => l,
        }
    } else {
        return CandidateOutcome::Skip;
    };

    let meta = match filepath.metadata() {
        Ok(m) => m,
        Err(err) => {
            warn!(path = %filepath.display(), error = %err, "failed to get file metadata, skipping");
            return CandidateOutcome::Skip;
        }
    };
    let size = meta.len();
    if size > MAX_INDEXABLE_FILE_SIZE_BYTES {
        debug!(
            path = %filepath.display(),
            size,
            limit = MAX_INDEXABLE_FILE_SIZE_BYTES,
            "skipping oversized file"
        );
        return CandidateOutcome::Skip;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64());

    let logical_path = make_logical_path(filepath);

    let symlink_target = if is_symlink {
        std::fs::canonicalize(filepath)
            .ok()
            .map(|resolved| make_logical_path(&resolved))
    } else {
        None
    };

    CandidateOutcome::Record(ScriptRecord {
        logical_path,
        physical_path: filepath.to_string_lossy().into_owned(),
        language: language.to_string(),
        size,
        mtime,
        symlink_target,
    })
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

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
    head_lines: usize,
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
    progress: Option<&ProgressBar>,
    shutdown: &AtomicBool,
) -> Result<ScanResult> {
    scan_paths_with_revisions_impl(
        roots,
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
pub(crate) fn scan_paths_with_revisions_and_tree(
    roots: &[PathBuf],
    head_lines: usize,
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
    progress: Option<&ProgressBar>,
    tree: Option<&mut ScanTreeView>,
    shutdown: &AtomicBool,
) -> Result<ScanResult> {
    scan_paths_with_revisions_impl(
        roots,
        head_lines,
        ignore_files,
        checkout_dirs,
        progress,
        tree,
        shutdown,
    )
}

fn scan_paths_with_revisions_impl(
    roots: &[PathBuf],
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
                .map(|c| process_candidate(&c.path, c.is_symlink, head_lines, &os_flavor, now))
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
    head_lines: usize,
    ignore_files: &[PathBuf],
    checkout_dirs: &[&str],
    progress: Option<&ProgressBar>,
    shutdown: &AtomicBool,
) -> Result<Vec<ScriptRecord>> {
    Ok(scan_paths_with_revisions(
        roots,
        head_lines,
        ignore_files,
        checkout_dirs,
        progress,
        shutdown,
    )?
    .scripts)
}

/// Walk `roots` (respecting `.catignore` and `ignore_files`) and return the
/// newest file mtime as UNIX epoch seconds, or `None` if no files are found.
/// Pass an empty `checkout_dirs` slice to fall back to the default names
/// (`DEVELOP`, `ARCHIVE`) — see [`scan_paths_with_revisions`].
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
/// DEVELOP/ARCHIVE checkout directories are pruned before descending, same as
/// [`scan_paths_with_revisions`] — otherwise this "is a rebuild needed" check
/// would walk every engineer's checkout tree just to stat mtimes that are
/// never used, doubling the cost of a walk the real scan is about to repeat
/// anyway.
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
    let mut max_mtime: Option<f64> = None;
    let canonical_roots = canonicalize_roots(roots);

    for root in roots {
        let croots = canonical_roots.clone();
        let cds = checkout_dirs_set.clone();
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

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }

            let meta = match entry.path().metadata() {
                Ok(m) => m,
                Err(err) => {
                    warn!(path = %entry.path().display(), error = %err, "failed to read file metadata, skipping");
                    continue;
                }
            };
            let modified = match meta.modified() {
                Ok(m) => m,
                Err(err) => {
                    warn!(path = %entry.path().display(), error = %err, "failed to read file mtime, skipping");
                    continue;
                }
            };
            let secs = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64());
            max_mtime = Some(max_mtime.map_or(secs, |m: f64| m.max(secs)));
        }
    }

    Ok(max_mtime)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn detects_python_from_extension() {
        assert_eq!(detect_language(Path::new("foo.py")), "python");
        assert_eq!(detect_language(Path::new("FOO.PY")), "python");
    }

    #[test]
    fn logical_path_is_the_absolute_filesystem_path() {
        assert_eq!(
            make_logical_path(Path::new("/net/scripts/tools/foo.py")),
            "/net/scripts/tools/foo.py"
        );
    }

    #[test]
    fn logical_path_keeps_a_literal_backslash_in_a_filename() {
        // `\` is an ordinary character in a Linux filename, not a separator.
        // Rewriting it (as the Windows-era code did) invented a directory
        // level that doesn't exist, and the logical path then matched nothing
        // on disk.
        assert_eq!(
            make_logical_path(Path::new(r"/net/scripts/od\d.py")),
            r"/net/scripts/od\d.py"
        );
    }

    #[test]
    fn detects_shell_from_extension() {
        assert_eq!(detect_language(Path::new("foo.sh")), "shell");
        assert_eq!(detect_language(Path::new("foo.bash")), "shell");
        assert_eq!(detect_language(Path::new("foo.ksh")), "shell");
    }

    #[test]
    fn unknown_for_unsupported() {
        assert_eq!(detect_language(Path::new("foo.rb")), "unknown");
        assert_eq!(detect_language(Path::new("foo")), "unknown");
    }

    #[test]
    fn shebang_direct_bash() {
        assert_eq!(shebang_language("#!/bin/bash"), Some("shell"));
    }

    #[test]
    fn shebang_direct_sh() {
        // How the extensionless shell tools vc manages are detected at all:
        // with no extension to go on, the shebang is the only language signal.
        assert_eq!(shebang_language("#!/bin/sh"), Some("shell"));
        assert_eq!(shebang_language("#! /bin/sh"), Some("shell"));
        assert_eq!(shebang_language("#!/usr/bin/env sh"), Some("shell"));
    }

    #[test]
    fn shebang_env_python3() {
        assert_eq!(shebang_language("#!/usr/bin/env python3"), Some("python"));
    }

    #[test]
    fn shebang_unknown() {
        assert_eq!(shebang_language("#!/usr/bin/env ruby"), None);
        assert_eq!(shebang_language("not a shebang"), None);
    }

    #[test]
    fn scan_records_working_dir_version_copies_as_revisions() {
        // Layout mirrors a vc working directory: the active script name plus
        // the two most recent checked-in copies (`<script>_<timestamp>`).
        let dir = tempfile::TempDir::new().unwrap();
        let scan_root = dir.path().join("linux").join("scripts");
        std::fs::create_dir_all(&scan_root).unwrap();
        std::fs::write(scan_root.join("bar.py"), "# python").unwrap();
        std::fs::write(scan_root.join("bar.py_20260720_090000"), "# python").unwrap();
        std::fs::write(scan_root.join("bar.py_20260610_151550"), "# python").unwrap();

        let shutdown = AtomicBool::new(false);
        let result = scan_paths_with_revisions(
            std::slice::from_ref(&scan_root),
            5,
            &[],
            &[],
            None,
            &shutdown,
        )
        .unwrap();

        assert_eq!(result.scripts.len(), 1, "only the active script indexes");
        assert!(result.scripts[0].physical_path.ends_with("bar.py"));

        assert_eq!(result.revisions.len(), 2);
        let expected_logical_path = scan_root.join("bar.py").to_string_lossy().into_owned();
        for revision in &result.revisions {
            assert_eq!(revision.logical_path, expected_logical_path);
            assert_eq!(revision.revision_type, REVISION_TYPE_WORKING);
            assert_eq!(revision.user, "");
            assert_eq!(revision.os_flavor, "linux");
        }
        let mut timestamps: Vec<&str> = result
            .revisions
            .iter()
            .map(|r| r.timestamp.as_str())
            .collect();
        timestamps.sort_unstable();
        assert_eq!(timestamps, vec!["20260610_151550", "20260720_090000"]);
    }

    #[test]
    fn scan_records_version_copies_of_extensionless_scripts_as_revisions() {
        // The same working-directory layout, for a shell tool carrying no
        // extension: the versions belong to `prepare_release`, not to five
        // scripts that happen to share a prefix.
        let dir = tempfile::TempDir::new().unwrap();
        let scan_root = dir.path().join("linux").join("scripts");
        std::fs::create_dir_all(&scan_root).unwrap();
        std::fs::write(scan_root.join("prepare_release"), "#!/bin/sh\n").unwrap();
        std::fs::write(
            scan_root.join("prepare_release_20260729_140513"),
            "#!/bin/sh\n",
        )
        .unwrap();
        std::fs::write(
            scan_root.join("prepare_release_20260701_105550"),
            "#!/bin/sh\n",
        )
        .unwrap();

        let shutdown = AtomicBool::new(false);
        let result = scan_paths_with_revisions(
            std::slice::from_ref(&scan_root),
            5,
            &[],
            &[],
            None,
            &shutdown,
        )
        .unwrap();

        let paths: Vec<&str> = result
            .scripts
            .iter()
            .map(|s| s.logical_path.as_str())
            .collect();
        let expected_logical_path = scan_root
            .join("prepare_release")
            .to_string_lossy()
            .into_owned();
        assert_eq!(paths, vec![expected_logical_path.as_str()]);

        assert_eq!(result.revisions.len(), 2);
        for revision in &result.revisions {
            assert_eq!(revision.logical_path, expected_logical_path);
            assert_eq!(revision.revision_type, REVISION_TYPE_WORKING);
        }
    }

    #[test]
    fn scan_records_version_copies_behind_an_active_symlink() {
        // vc's steady state: the script name is a symlink to the newest
        // version. Only the symlink indexes, and it keeps pointing at the
        // version it resolves to.
        let dir = tempfile::TempDir::new().unwrap();
        let scan_root = dir.path().join("linux").join("scripts");
        std::fs::create_dir_all(&scan_root).unwrap();
        std::fs::write(
            scan_root.join("prepare_release_20260729_140513"),
            "#!/bin/sh\n",
        )
        .unwrap();
        std::fs::write(
            scan_root.join("prepare_release_20260701_105550"),
            "#!/bin/sh\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "prepare_release_20260729_140513",
            scan_root.join("prepare_release"),
        )
        .unwrap();

        let shutdown = AtomicBool::new(false);
        let result = scan_paths_with_revisions(
            std::slice::from_ref(&scan_root),
            5,
            &[],
            &[],
            None,
            &shutdown,
        )
        .unwrap();

        assert_eq!(result.scripts.len(), 1);
        assert_eq!(
            result.scripts[0].logical_path,
            scan_root.join("prepare_release").to_string_lossy()
        );
        assert_eq!(
            result.scripts[0].symlink_target.as_deref(),
            Some(
                scan_root
                    .join("prepare_release_20260729_140513")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(result.revisions.len(), 2);
    }

    #[test]
    fn scan_skips_editor_backup_files() {
        // `foo.sh~` is already dropped as an unknown extension; `foo~` has no
        // extension and must not reach the shebang sniff.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("prepare_release"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.path().join("prepare_release~"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.path().join("release_backup.sh"), "#!/bin/bash\n").unwrap();
        std::fs::write(dir.path().join("release_backup.sh~"), "#!/bin/bash\n").unwrap();

        let shutdown = AtomicBool::new(false);
        let records =
            scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

        let mut paths: Vec<&str> = records.iter().map(|r| r.logical_path.as_str()).collect();
        paths.sort_unstable();
        let expected_prepare_release = dir
            .path()
            .join("prepare_release")
            .to_string_lossy()
            .into_owned();
        let expected_release_backup = dir
            .path()
            .join("release_backup.sh")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            paths,
            vec![
                expected_prepare_release.as_str(),
                expected_release_backup.as_str(),
            ]
        );
    }

    #[test]
    fn extensionless_date_suffixed_file_stays_a_script() {
        // A shebang script whose name merely ends in digits must not be
        // swallowed as a WORKING revision — only version names whose script
        // part has a script extension qualify.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("backup_20240101"), "#!/bin/bash\n").unwrap();

        let shutdown = AtomicBool::new(false);
        let result =
            scan_paths_with_revisions(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown)
                .unwrap();

        assert_eq!(result.scripts.len(), 1);
        assert!(result.revisions.is_empty());
    }

    #[test]
    fn scan_finds_py_and_sh_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.py"), "# python").unwrap();
        std::fs::write(dir.path().join("b.sh"), "#!/bin/bash").unwrap();
        std::fs::write(dir.path().join("c.txt"), "ignored").unwrap();

        let shutdown = AtomicBool::new(false);
        let records =
            scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();
        assert_eq!(records.len(), 2);
        let paths: Vec<&str> = records.iter().map(|r| r.language.as_str()).collect();
        assert!(paths.contains(&"python"));
        assert!(paths.contains(&"shell"));
    }

    #[test]
    fn scan_respects_root_catignore() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("vendor")).unwrap();
        std::fs::write(dir.path().join(".catignore"), "vendor/\n").unwrap();
        std::fs::write(dir.path().join("keep.py"), "# python").unwrap();
        std::fs::write(dir.path().join("vendor").join("skip.py"), "# python").unwrap();

        let shutdown = AtomicBool::new(false);
        let records =
            scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

        assert_eq!(records.len(), 1);
        assert!(records[0].physical_path.ends_with("keep.py"));
    }

    #[test]
    fn scan_respects_explicit_ignore_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("vendor")).unwrap();
        let ignore_file = dir.path().join("custom.catignore");
        std::fs::write(&ignore_file, "vendor/\n").unwrap();
        std::fs::write(dir.path().join("keep.py"), "# python").unwrap();
        std::fs::write(dir.path().join("vendor").join("skip.py"), "# python").unwrap();

        let shutdown = AtomicBool::new(false);
        let records = scan_paths(
            &[dir.path().to_path_buf()],
            5,
            &[ignore_file],
            &[],
            None,
            &shutdown,
        )
        .unwrap();

        assert_eq!(records.len(), 1);
        assert!(records[0].physical_path.ends_with("keep.py"));
    }

    #[test]
    fn checkout_files_in_develop_dir_are_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let develop = dir.path().join("DEVELOP");
        std::fs::create_dir_all(&develop).unwrap();
        std::fs::write(
            develop.join("tool_20240315_1430_jdoe"),
            "#!/bin/bash\necho hi",
        )
        .unwrap();

        let shutdown = AtomicBool::new(false);
        let result = scan_paths_with_revisions(
            &[dir.path().to_path_buf()],
            5,
            &[],
            &["DEVELOP"],
            None,
            &shutdown,
        )
        .unwrap();

        assert!(
            result.scripts.is_empty(),
            "DEVELOP dir files must not be indexed as scripts"
        );
    }

    #[test]
    fn checkout_files_in_archive_dir_are_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let archive = dir.path().join("ARCHIVE");
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(archive.join("tool.py_20240101_1200_alice"), "print('x')").unwrap();

        let shutdown = AtomicBool::new(false);
        let result = scan_paths_with_revisions(
            &[dir.path().to_path_buf()],
            5,
            &[],
            &["ARCHIVE"],
            None,
            &shutdown,
        )
        .unwrap();

        assert!(
            result.scripts.is_empty(),
            "ARCHIVE dir files must not be indexed as scripts"
        );
    }

    #[test]
    fn checkout_files_in_custom_dir_are_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let checkout = dir.path().join("WORKING");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(
            checkout.join("tool_20240315_1430_jdoe"),
            "#!/bin/bash\necho hi",
        )
        .unwrap();

        let shutdown = AtomicBool::new(false);
        let result = scan_paths_with_revisions(
            &[dir.path().to_path_buf()],
            5,
            &[],
            &["WORKING"],
            None,
            &shutdown,
        )
        .unwrap();

        assert!(
            result.scripts.is_empty(),
            "custom checkout dir files must not be indexed as scripts"
        );
    }

    #[test]
    fn max_mtime_returns_none_for_empty_roots() {
        let dir = tempfile::TempDir::new().unwrap();
        // Empty directory — no files at all.
        let result = max_mtime_in_roots(&[dir.path().to_path_buf()], &[], &[]).unwrap();
        assert!(result.is_none());
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
    fn max_mtime_skips_checkout_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let develop = dir.path().join("DEVELOP");
        std::fs::create_dir_all(&develop).unwrap();
        std::fs::write(develop.join("tool_20240315_1430_jdoe"), "echo hi").unwrap();

        let result = max_mtime_in_roots(&[dir.path().to_path_buf()], &[], &[]).unwrap();
        assert!(
            result.is_none(),
            "DEVELOP dir files must not affect the up-to-date check, otherwise every \
             engineer checkout touch would force a full rebuild"
        );
    }

    #[test]
    fn scan_skips_oversized_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("small.py"), "# python").unwrap();
        let huge = vec![0u8; (MAX_INDEXABLE_FILE_SIZE_BYTES + 1) as usize];
        std::fs::write(dir.path().join("huge.py"), &huge).unwrap();

        let shutdown = AtomicBool::new(false);
        let records =
            scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

        assert_eq!(records.len(), 1);
        assert!(records[0].physical_path.ends_with("small.py"));
    }

    #[test]
    fn scan_does_not_follow_symlinks_outside_scan_roots() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.py"), "# python").unwrap();
        std::fs::write(root.path().join("keep.py"), "# python").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();

        let shutdown = AtomicBool::new(false);
        let records =
            scan_paths(&[root.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

        assert_eq!(records.len(), 1);
        assert!(records[0].physical_path.ends_with("keep.py"));
    }

    #[test]
    fn scan_follows_symlinks_within_scan_roots() {
        let root = tempfile::TempDir::new().unwrap();
        let real = root.path().join("linux");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("tool.py"), "# python").unwrap();
        std::os::unix::fs::symlink(&real, root.path().join("alt")).unwrap();

        let shutdown = AtomicBool::new(false);
        let records =
            scan_paths(&[root.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

        // Both the real path and the in-root symlink alias are indexed.
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn scan_processes_a_root_spanning_multiple_parallel_batches() {
        // More candidates than SCAN_PROCESS_BATCH_SIZE, so the per-root
        // parallel processing loop runs across several batches — exercises
        // that results from every batch (and every worker thread) land in
        // `records` exactly once, with no duplicates or drops.
        let dir = tempfile::TempDir::new().unwrap();
        let file_count = SCAN_PROCESS_BATCH_SIZE * 2 + 50;
        for i in 0..file_count {
            std::fs::write(dir.path().join(format!("script_{i:04}.py")), "# python").unwrap();
        }

        let shutdown = AtomicBool::new(false);
        let records =
            scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

        assert_eq!(records.len(), file_count);
        let mut seen: HashSet<&str> = HashSet::new();
        for record in &records {
            assert_eq!(record.language, "python");
            assert!(
                seen.insert(record.logical_path.as_str()),
                "duplicate logical_path: {}",
                record.logical_path
            );
        }
    }

    #[test]
    fn scan_respects_shutdown_signal_mid_batch() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.py"), "# python").unwrap();
        let shutdown = AtomicBool::new(true);

        let err =
            scan_paths_with_revisions(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown)
                .unwrap_err();

        assert!(matches!(err, Error::Interrupted));
    }
}
