//! Per-file classification: script, WORKING revision, or skip.

use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::core::vc::{CheckoutRecord, REVISION_TYPE_WORKING, parse_checkout_filename};

use super::ScriptRecord;
use super::language::{SCRIPT_EXTENSIONS, detect_language, read_head, shebang_language};

/// Files larger than this are skipped during scanning rather than read into
/// memory and parsed — a multi-gigabyte data file sharing a script extension
/// (a huge `.csv`/`.json` export, for instance) would otherwise be read whole,
/// regex-scanned, and tree-sitter-parsed, which can stall a run for a long
/// time on what is almost certainly not really a script.
pub(super) const MAX_INDEXABLE_FILE_SIZE_BYTES: u64 = 8 * 1024 * 1024;

/// Number of candidates processed per parallel batch. Bounds how long a
/// Ctrl-C has to wait for the in-flight batch to finish before the shutdown
/// flag is rechecked, mirroring `EXTRACT_BATCH_SIZE` in
/// `builder/pipeline.rs`, which does the same for phase 2's extraction work.
pub(super) const SCAN_PROCESS_BATCH_SIZE: usize = 500;

/// A regular file the walk decided is worth a closer look — not yet
/// classified as a script, a WORKING revision, or something to skip. Kept
/// deliberately small since these accumulate per scan root before the
/// parallel pass in [`process_candidate`] runs.
pub(super) struct ScanCandidate {
    pub(super) path: PathBuf,
    pub(super) is_symlink: bool,
}

/// What [`process_candidate`] decided about one [`ScanCandidate`].
pub(super) enum CandidateOutcome {
    Record(ScriptRecord),
    Revision(CheckoutRecord),
    Skip,
}

/// Build a script's logical catalog path from where it was scanned.
///
/// The indexer runs on Linux, so the path components here are already
/// `/`-separated and a literal `\` in a name is just an (unusual but legal)
/// filename character — not a separator to rewrite.
fn make_logical_path(filepath: &Path, root: &Path, logical_prefix: &str) -> String {
    let relative = match filepath.strip_prefix(root) {
        Ok(r) => r.to_string_lossy(),
        Err(_) => return filepath.to_string_lossy().into_owned(),
    };
    if logical_prefix.is_empty() {
        filepath.to_string_lossy().into_owned()
    } else {
        format!("{}/{}", logical_prefix.trim_end_matches('/'), relative)
    }
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

/// Stat, sniff, and classify one candidate file. Pure filesystem I/O with no
/// shared state, so it's safe to call from any worker thread — this is what
/// lets a scan root's candidate list process in parallel across cores
/// instead of one file at a time on the directory-walking thread, which is
/// what actually dominates scan time on a large tree (this function does
/// every stat, file read, and symlink resolution; the walk itself just
/// enumerates names).
pub(super) fn process_candidate(
    filepath: &Path,
    is_symlink: bool,
    root: &Path,
    logical_prefix: &str,
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
            let logical_path =
                make_logical_path(&filepath.with_file_name(&script_name), root, logical_prefix);
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

    let logical_path = make_logical_path(filepath, root, logical_prefix);

    let symlink_target = if is_symlink {
        std::fs::canonicalize(filepath)
            .ok()
            .map(|resolved| make_logical_path(&resolved, root, logical_prefix))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_path_joins_scan_relative_segments() {
        assert_eq!(
            make_logical_path(
                Path::new("/net/scripts/tools/foo.py"),
                Path::new("/net/scripts"),
                "/catalog/scripts",
            ),
            "/catalog/scripts/tools/foo.py"
        );
    }

    #[test]
    fn logical_path_keeps_a_literal_backslash_in_a_filename() {
        // `\` is an ordinary character in a Linux filename, not a separator.
        // Rewriting it (as the Windows-era code did) invented a directory
        // level that doesn't exist, and the logical path then matched nothing
        // on disk.
        assert_eq!(
            make_logical_path(
                Path::new(r"/net/scripts/od\d.py"),
                Path::new("/net/scripts"),
                "/catalog/scripts",
            ),
            r"/catalog/scripts/od\d.py"
        );
    }
}
