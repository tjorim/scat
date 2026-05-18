//! Script-level diff: compare active catalog content against vc checkouts,
//! archive files, or arbitrary file pairs.
//!
//! # Modes
//!
//! | CLI invocation | Mode |
//! |---|---|
//! | `scat diff /path` | active catalog content vs most-recent vc checkout |
//! | `scat diff /path --against <file>` | active catalog content vs explicit file |
//! | `scat diff --old <file> --new <file>` | two explicit files (no catalog) |

use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;
use similar::{ChangeTag, TextDiff};

use crate::core::db::query_rows;
use crate::core::vc::REVISION_TYPE_DEVELOP;
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// How a diff source was obtained.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Content read from the catalog `scripts.content` column.
    Active,
    /// Physical path from an indexed DEVELOP revision.
    Checkout,
    /// Explicitly supplied path (via `--against`, `--old`, or `--new`).
    Explicit,
}

/// One line in a diff hunk.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct DiffLine {
    /// Whether this line is unchanged context, removed from old, or added in new.
    pub kind: DiffLineKind,
    /// Line text without a trailing newline.
    pub content: String,
    /// True when this line has no trailing newline (last line of a file without a final newline).
    pub missing_newline: bool,
}

/// Classification of a single diff line.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    /// Unchanged line present in both sides.
    Context,
    /// Line present only in the old (left) side.
    Delete,
    /// Line present only in the new (right) side.
    Insert,
}

/// One contiguous changed region, padded with context lines.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct DiffHunk {
    /// 1-based line number of the first line of this hunk in the old file.
    pub old_start: usize,
    /// Number of lines from the old file covered by this hunk.
    pub old_count: usize,
    /// 1-based line number of the first line of this hunk in the new file.
    pub new_start: usize,
    /// Number of lines from the new file covered by this hunk.
    pub new_count: usize,
    /// All lines (context + changes) for this hunk.
    pub lines: Vec<DiffLine>,
}

/// Result of a script-level diff operation.
#[derive(Debug, Serialize)]
pub struct ScriptDiffResult {
    /// Logical catalog path, `null` when using `--old`/`--new` mode.
    pub logical_path: Option<String>,
    /// Label for the "old" (left) side.
    pub old_path: String,
    /// Label for the "new" (right) side.
    pub new_path: String,
    /// How the old content was obtained.
    pub old_kind: SourceKind,
    /// How the new content was obtained.
    pub new_kind: SourceKind,
    /// Changed regions found between old and new.
    pub hunks: Vec<DiffHunk>,
}

// ---------------------------------------------------------------------------
// Context lines around each hunk
// ---------------------------------------------------------------------------

const CONTEXT: usize = 3;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compare the cataloged active content of `logical_path` against the
/// most-recent indexed DEVELOP vc revision.
///
/// Returns an error with actionable hints when:
/// - the script is not indexed, or has no stored content,
/// - no checkout rows exist for the path,
/// - the checkout file is not present on disk.
pub fn diff_catalog_vs_checkout(conn: &Connection, logical_path: &str) -> Result<ScriptDiffResult> {
    let old_content = catalog_content(conn, logical_path)?;

    // Fetch DEVELOP revisions, ordered most-recent first.
    let checkouts = query_rows(
        conn,
        "SELECT physical_path, timestamp, os_flavor
         FROM revisions
         WHERE logical_path = ?
           AND revision_type = ?
         ORDER BY timestamp DESC, physical_path DESC",
        &[&logical_path, &REVISION_TYPE_DEVELOP],
    )?;

    if checkouts.is_empty() {
        return Err(Error::Validation(format!(
            "No vc checkouts found for '{logical_path}' in catalog.\n\
             Hints:\n\
             • Run 'scat status {logical_path}' to see checkout state.\n\
             • Use 'scat diff {logical_path} --against <file>' to compare against a specific file.\n\
             • Re-index with vc config to capture checkout data."
        )));
    }

    // Pick the most-recent checkout (first row after ORDER BY timestamp DESC).
    let checkout = &checkouts[0];
    let physical_path = checkout
        .get("physical_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if physical_path.is_empty() {
        return Err(Error::Validation(
            "Checkout row has no physical_path.".to_string(),
        ));
    }

    let new_content = read_file_content(Path::new(&physical_path))?;
    let hunks = compute_diff(&old_content, &new_content);

    Ok(ScriptDiffResult {
        logical_path: Some(logical_path.to_string()),
        old_path: format!("catalog:{logical_path}"),
        new_path: physical_path,
        old_kind: SourceKind::Active,
        new_kind: SourceKind::Checkout,
        hunks,
    })
}

/// Compare the cataloged active content of `logical_path` against an
/// explicitly supplied file path (from `--against`).
pub fn diff_catalog_vs_file(
    conn: &Connection,
    logical_path: &str,
    against: &Path,
) -> Result<ScriptDiffResult> {
    let old_content = catalog_content(conn, logical_path)?;
    let new_content = read_file_content(against)?;
    let hunks = compute_diff(&old_content, &new_content);

    Ok(ScriptDiffResult {
        logical_path: Some(logical_path.to_string()),
        old_path: format!("catalog:{logical_path}"),
        new_path: against.display().to_string(),
        old_kind: SourceKind::Active,
        new_kind: SourceKind::Explicit,
        hunks,
    })
}

/// Compare two explicit files without any catalog involvement.
/// `logical_path` is `None` in the output.
pub fn diff_files(old: &Path, new: &Path) -> Result<ScriptDiffResult> {
    let old_content = read_file_content(old)?;
    let new_content = read_file_content(new)?;
    let hunks = compute_diff(&old_content, &new_content);

    Ok(ScriptDiffResult {
        logical_path: None,
        old_path: old.display().to_string(),
        new_path: new.display().to_string(),
        old_kind: SourceKind::Explicit,
        new_kind: SourceKind::Explicit,
        hunks,
    })
}

// ---------------------------------------------------------------------------
// Human-readable output
// ---------------------------------------------------------------------------

/// Render `result` as a unified-diff-style text block.
pub fn render_diff_text(result: &ScriptDiffResult) -> String {
    let mut out = String::new();
    out.push_str(&format!("--- {}\n", result.old_path));
    out.push_str(&format!("+++ {}\n", result.new_path));

    for hunk in &result.hunks {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));
        for line in &hunk.lines {
            let prefix = match line.kind {
                DiffLineKind::Context => ' ',
                DiffLineKind::Delete => '-',
                DiffLineKind::Insert => '+',
            };
            out.push(prefix);
            out.push_str(&line.content);
            out.push('\n');
            if line.missing_newline {
                out.push_str("\\ No newline at end of file\n");
            }
        }
    }

    if result.hunks.is_empty() {
        out.push_str("(no differences)\n");
    }

    out
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Fetch catalog content for `logical_path`, erroring with actionable hints
/// when the script is absent or has no stored content.
fn catalog_content(conn: &Connection, logical_path: &str) -> Result<String> {
    let rows = query_rows(
        conn,
        "SELECT content FROM scripts WHERE logical_path = ?",
        &[&logical_path],
    )?;

    match rows.into_iter().next() {
        None => Err(Error::Validation(format!(
            "Script '{logical_path}' not found in catalog.\n\
             Hints:\n\
             • Check the path with 'scat search {logical_path}'.\n\
             • Use 'scat diff --old <file> --new <file>' to compare without catalog metadata."
        ))),
        Some(mut row) => {
            let content = match row.remove("content") {
                Some(serde_json::Value::String(s)) => s,
                _ => String::new(),
            };
            if content.is_empty() {
                Err(Error::Validation(format!(
                    "Script '{logical_path}' has no content stored in catalog.\n\
                     Hints:\n\
                     • Re-index to capture content.\n\
                     • Use 'scat diff --old <file> --new <file>' to compare files directly."
                )))
            } else {
                Ok(content)
            }
        }
    }
}

/// Read file at `path`, returning an informative error if it does not exist.
fn read_file_content(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| {
        Error::Validation(format!(
            "Cannot read '{}': {e}\n\
             Hint: Check that the file path is correct and accessible.",
            path.display()
        ))
    })
}

// ---------------------------------------------------------------------------
// Diff algorithm (via `similar`)
// ---------------------------------------------------------------------------

/// Compute a line-level diff between `old` and `new` content strings.
/// Returns a list of [`DiffHunk`] values with `CONTEXT` context lines each.
pub fn compute_diff(old: &str, new: &str) -> Vec<DiffHunk> {
    let diff = TextDiff::from_lines(old, new);

    diff.grouped_ops(CONTEXT)
        .into_iter()
        .map(|group| {
            let first = &group[0];
            let last = &group[group.len() - 1];

            let old_start = first.old_range().start;
            let new_start = first.new_range().start;
            let old_count = last.old_range().end - first.old_range().start;
            let new_count = last.new_range().end - first.new_range().start;

            let lines = group
                .iter()
                .flat_map(|op| diff.iter_changes(op))
                .map(|change| DiffLine {
                    kind: match change.tag() {
                        ChangeTag::Equal => DiffLineKind::Context,
                        ChangeTag::Delete => DiffLineKind::Delete,
                        ChangeTag::Insert => DiffLineKind::Insert,
                    },
                    content: change.value().trim_end_matches('\n').to_string(),
                    missing_newline: change.missing_newline(),
                })
                .collect();

            DiffHunk {
                old_start: if old_count == 0 { 0 } else { old_start + 1 },
                old_count,
                new_start: if new_count == 0 { 0 } else { new_start + 1 },
                new_count,
                lines,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn diff_text(old: &str, new: &str) -> String {
        let result = ScriptDiffResult {
            logical_path: None,
            old_path: "old".to_string(),
            new_path: "new".to_string(),
            old_kind: SourceKind::Explicit,
            new_kind: SourceKind::Explicit,
            hunks: compute_diff(old, new),
        };
        render_diff_text(&result)
    }

    #[test]
    fn identical_files_produce_no_hunks() {
        let content = "line1\nline2\nline3\n";
        let hunks = compute_diff(content, content);
        assert!(hunks.is_empty());
    }

    #[test]
    fn single_line_change() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let hunks = compute_diff(old, new);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        assert!(
            hunk.lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Delete && l.content == "b")
        );
        assert!(
            hunk.lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Insert && l.content == "B")
        );
    }

    #[test]
    fn deleted_lines_only() {
        let old = "a\nb\nc\n";
        let new = "a\nc\n";
        let hunks = compute_diff(old, new);
        assert_eq!(hunks.len(), 1);
        assert!(
            hunks[0]
                .lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Delete && l.content == "b")
        );
    }

    #[test]
    fn inserted_lines_only() {
        let old = "a\nc\n";
        let new = "a\nb\nc\n";
        let hunks = compute_diff(old, new);
        assert_eq!(hunks.len(), 1);
        assert!(
            hunks[0]
                .lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Insert && l.content == "b")
        );
    }

    #[test]
    fn distant_changes_produce_separate_hunks() {
        // Two changes far apart should produce two separate hunks.
        let mut old_lines: Vec<String> = (0..30).map(|i| format!("line{i}")).collect();
        let mut new_lines = old_lines.clone();
        old_lines[0] = "OLD_TOP".to_string();
        old_lines[29] = "OLD_BOTTOM".to_string();
        new_lines[0] = "NEW_TOP".to_string();
        new_lines[29] = "NEW_BOTTOM".to_string();

        let old = old_lines.join("\n");
        let new = new_lines.join("\n");
        let hunks = compute_diff(&old, &new);
        assert_eq!(hunks.len(), 2, "expected two separate hunks");
    }

    #[test]
    fn render_shows_unified_header() {
        let text = diff_text("a\nb\n", "a\nB\n");
        assert!(text.contains("--- old"), "missing --- header");
        assert!(text.contains("+++ new"), "missing +++ header");
        assert!(text.contains("@@"), "missing @@ hunk header");
    }

    #[test]
    fn empty_files_produce_no_hunks() {
        let hunks = compute_diff("", "");
        assert!(hunks.is_empty());
    }

    #[test]
    fn new_file_all_inserted() {
        let hunks = compute_diff("", "line1\nline2\n");
        assert_eq!(hunks.len(), 1);
        assert!(
            hunks[0]
                .lines
                .iter()
                .all(|l| l.kind == DiffLineKind::Insert)
        );
    }

    #[test]
    fn old_file_all_deleted() {
        let hunks = compute_diff("line1\nline2\n", "");
        assert_eq!(hunks.len(), 1);
        assert!(
            hunks[0]
                .lines
                .iter()
                .all(|l| l.kind == DiffLineKind::Delete)
        );
    }

    #[test]
    fn new_file_hunk_header_uses_zero_old_start() {
        let hunks = compute_diff("", "line1\nline2\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 0);
        assert_eq!(hunks[0].old_count, 0);
        assert_eq!(hunks[0].new_start, 1);
        assert_eq!(hunks[0].new_count, 2);
    }

    #[test]
    fn deleted_file_hunk_header_uses_zero_new_start() {
        let hunks = compute_diff("line1\nline2\n", "");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[0].old_count, 2);
        assert_eq!(hunks[0].new_start, 0);
        assert_eq!(hunks[0].new_count, 0);
    }

    #[test]
    fn missing_newline_marker_in_render() {
        // File without trailing newline on the last line.
        let text = diff_text("a\nb", "a\nB");
        assert!(
            text.contains("\\ No newline at end of file"),
            "expected missing-newline marker"
        );
    }

    #[test]
    fn no_missing_newline_marker_when_newline_present() {
        let text = diff_text("a\nb\n", "a\nB\n");
        assert!(
            !text.contains("\\ No newline at end of file"),
            "unexpected missing-newline marker"
        );
    }
}
