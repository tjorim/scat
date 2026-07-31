use std::collections::HashSet;
use std::path::Path;

use regex::Regex;
use tracing::{trace, warn};

use crate::indexer::scanner::ScriptRecord;

// ---------------------------------------------------------------------------
// History entry parsing
// ---------------------------------------------------------------------------

/// A structured changelog entry parsed from a single `@history` header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The raw value after the `@history` tag (trimmed).
    pub raw: String,
    /// ISO-8601 date extracted from the line, e.g. `"2024-05-10"`.
    pub date: Option<String>,
    /// Author token extracted from the line.
    pub author: Option<String>,
    /// Version token extracted from the line, e.g. `"1.2.3"`.
    pub version: Option<String>,
    /// Human-readable change summary (everything not classified as date/version/author).
    pub summary: Option<String>,
}

impl HistoryEntry {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "raw":     self.raw,
            "date":    self.date,
            "author":  self.author,
            "version": self.version,
            "summary": self.summary,
        })
    }
}

static HISTORY_DATE_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\b(\d{4}-\d{2}-\d{2})\b").unwrap());

static HISTORY_VERSION_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\b(\d+\.\d+(?:\.\d+)*)\b").unwrap());

/// Parse a single raw `@history` value into a [`HistoryEntry`].
///
/// Supports common field orderings without being brittle:
/// - `2024-05-10 alice Fixed timeout handling` (date author summary)
/// - `alice 2024-05-10 Fixed timeout handling` (author date summary)
/// - `1.2.3 2024-05-10 alice Fixed timeout handling` (version date author summary)
/// - `Fixed timeout handling` (summary only)
///
/// When a field cannot be confidently identified the raw value is preserved in
/// `summary` and `date`/`author`/`version` are left as `None`.
pub fn parse_history_entry(raw: &str) -> HistoryEntry {
    let raw_trimmed = raw.trim();
    let mut work = raw_trimmed.to_string();
    let mut date: Option<String> = None;
    let mut version: Option<String> = None;
    let mut author: Option<String> = None;

    // Extract ISO date (YYYY-MM-DD) — remove it from the working string so
    // subsequent token extraction is not confused.
    if let Some(m) = HISTORY_DATE_RE.find(&work) {
        date = Some(m.as_str().to_string());
        let (before, after) = (&work[..m.start()], &work[m.end()..]);
        work = format!("{before}{after}");
        work = work.split_whitespace().collect::<Vec<_>>().join(" ");
    }

    // Extract a version token (X.Y or X.Y.Z …).  Only attempt this when a
    // date was also found; without a date anchor a bare "1.0" in a free-text
    // summary would be misclassified.
    if date.is_some()
        && let Some(m) = HISTORY_VERSION_RE.find(&work)
    {
        version = Some(m.as_str().to_string());
        let (before, after) = (&work[..m.start()], &work[m.end()..]);
        work = format!("{before}{after}");
        work = work.split_whitespace().collect::<Vec<_>>().join(" ");
    }

    // If we found at least a date (or version), try to extract an author from
    // the first whitespace-delimited token — as long as it does not itself
    // look like a date or version.
    if date.is_some() || version.is_some() {
        let first_end = work.find(char::is_whitespace).unwrap_or(work.len());
        let first = work[..first_end].trim();
        if !first.is_empty()
            && !HISTORY_DATE_RE.is_match(first)
            && !HISTORY_VERSION_RE.is_match(first)
        {
            author = Some(first.to_string());
            work = work[first_end..].trim().to_string();
        }
    }

    let summary = if work.is_empty() { None } else { Some(work) };

    HistoryEntry {
        raw: raw_trimmed.to_string(),
        date,
        author,
        version,
        summary,
    }
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
/// Structured metadata extracted from a script file.
pub struct ExtractedMetadata {
    /// Full file content as UTF-8 (lossy) text.
    pub content: String,
    /// SHA-256 digest (lowercase hex) of the file's raw bytes, used for
    /// content-based change detection during incremental builds.
    pub content_hash: String,
    /// Normalized owner metadata extracted from comments.
    pub owner: String,
    /// Normalized purpose/brief metadata.
    pub purpose: String,
    /// Parsed tags list.
    pub tags: Vec<String>,
    /// Parsed entry point list.
    pub entry_points: Vec<String>,
    /// Parsed related-script list.
    pub related: Vec<String>,
    /// Sorted metadata map for deterministic JSON serialization.
    pub fields: std::collections::BTreeMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract structured metadata from a scanned script record.
pub fn extract(record: &ScriptRecord) -> ExtractedMetadata {
    let mut meta = ExtractedMetadata::default();
    let (content, content_hash) = read_file(Path::new(&record.physical_path));

    parse_header_comments(&content, &mut meta);

    if record.language == "python" {
        parse_python_docstring(&content, &mut meta);
    }

    meta.content = content;
    meta.content_hash = content_hash;
    meta
}

// ---------------------------------------------------------------------------
// File reading
// ---------------------------------------------------------------------------

/// Read a file's raw bytes, returning its lossy-UTF-8 text alongside a
/// SHA-256 hex digest of the *raw* bytes (computed before the lossy
/// conversion, so it reflects the file's actual on-disk content).
fn read_file(path: &Path) -> (String, String) {
    match std::fs::read(path) {
        Ok(bytes) => {
            let hash = hash_bytes(&bytes);
            (String::from_utf8_lossy(&bytes).into_owned(), hash)
        }
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to read file for metadata extraction"
            );
            (String::new(), hash_bytes(&[]))
        }
    }
}

/// SHA-256 hex digest of `bytes`.
fn hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// SHA-256 hex digest of a file's current on-disk content, or `None` if it
/// can't be read. Used by incremental seeding to check whether a file whose
/// mtime/size changed actually has different content — a `touch`, a
/// checkout/restore, or a VC operation that rewrites the file with identical
/// bytes all move the mtime without changing what's indexed.
pub fn hash_file(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| hash_bytes(&bytes))
}

// ---------------------------------------------------------------------------
// Header comment parsing
// ---------------------------------------------------------------------------

struct HeaderPattern {
    re: Regex,
    field_name: &'static str,
    metadata_key: &'static str,
}

static HEADER_PATTERNS: std::sync::LazyLock<Vec<HeaderPattern>> = std::sync::LazyLock::new(|| {
    vec![
        HeaderPattern {
            re: Regex::new(r"(?i)(?:@brief|brief\s*:)\s*(.+)").unwrap(),
            field_name: "purpose",
            metadata_key: "brief",
        },
        HeaderPattern {
            re: Regex::new(r"(?i)@purpose\s+(.+)").unwrap(),
            field_name: "purpose",
            metadata_key: "purpose",
        },
        HeaderPattern {
            re: Regex::new(r"(?i)@author\s+(.+)").unwrap(),
            field_name: "owner",
            metadata_key: "author",
        },
        HeaderPattern {
            re: Regex::new(r"(?i)@techowner\s+(.+)").unwrap(),
            field_name: "owner",
            metadata_key: "techowner",
        },
        HeaderPattern {
            re: Regex::new(r"(?i)@funcowner\s+(.+)").unwrap(),
            field_name: "owner",
            metadata_key: "funcowner",
        },
        HeaderPattern {
            re: Regex::new(r"(?i)@history\s+(.+)").unwrap(),
            field_name: "history",
            metadata_key: "history",
        },
        HeaderPattern {
            re: Regex::new(r"(?i)owner\s*:\s*(.+)").unwrap(),
            field_name: "owner",
            metadata_key: "owner",
        },
    ]
});

fn strip_comment_prefix(line: &str) -> &str {
    let s = line.trim();
    // Strip leading comment characters
    let s = s.strip_prefix("<#").unwrap_or(s);
    let s = s.strip_prefix("//").unwrap_or(s);
    let s = s.strip_prefix('#').unwrap_or(s);
    s.trim()
}

fn parse_header_comments(content: &str, meta: &mut ExtractedMetadata) {
    let mut found: HashSet<&'static str> = HashSet::new();
    let mut history_raw: Vec<String> = Vec::new();

    for line in content.lines().take(40) {
        let stripped = strip_comment_prefix(line);
        for pat in HEADER_PATTERNS.iter() {
            if found.contains(pat.field_name) && pat.field_name != "history" {
                continue;
            }
            if let Some(cap) = pat.re.captures(stripped) {
                let value = cap[1].trim().to_string();
                if value.is_empty() {
                    continue;
                }
                trace!(field = pat.metadata_key, value = %value, "parsed header field");

                if pat.field_name == "history" {
                    history_raw.push(value);
                    continue;
                }

                meta.fields.insert(
                    pat.metadata_key.to_string(),
                    serde_json::Value::String(value.clone()),
                );
                match pat.field_name {
                    "purpose" => meta.purpose = value,
                    "owner" => meta.owner = value,
                    _ => {}
                }
                found.insert(pat.field_name);
            }
        }
    }

    // Store all collected @history values as a JSON array (preserves every line).
    if !history_raw.is_empty() {
        meta.fields.insert(
            "history".to_string(),
            serde_json::Value::Array(
                history_raw
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
        // Also store structured parsed entries under `history_entries`.
        meta.fields.insert(
            "history_entries".to_string(),
            serde_json::Value::Array(
                history_raw
                    .iter()
                    .map(|s| parse_history_entry(s).to_json())
                    .collect(),
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// Python docstring extraction
// ---------------------------------------------------------------------------

static DOCSTRING_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r#"(?s)^\s*(?:"""(.*?)"""|'''(.*?)''')"#).unwrap());

fn parse_python_docstring(content: &str, meta: &mut ExtractedMetadata) {
    if !meta.purpose.is_empty() {
        return;
    }
    if let Some(cap) = DOCSTRING_RE.captures(content) {
        let doc = cap
            .get(1)
            .or_else(|| cap.get(2))
            .map_or("", |m| m.as_str())
            .trim();
        if let Some(first_line) = doc.lines().next() {
            let first = first_line.trim();
            if !first.is_empty() {
                meta.purpose = first.to_string();
                meta.fields.insert(
                    "docstring".to_string(),
                    serde_json::Value::String(first.to_string()),
                );
                trace!(value = %first, "parsed python docstring purpose");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_headers(content: &str) -> ExtractedMetadata {
        let mut meta = ExtractedMetadata::default();
        parse_header_comments(content, &mut meta);
        meta
    }

    #[test]
    fn parses_at_brief() {
        let meta = parse_headers("# @brief Does something useful");
        assert_eq!(meta.purpose, "Does something useful");
    }

    #[test]
    fn parses_author_field() {
        let meta = parse_headers("# @author alice@example.com");
        assert_eq!(meta.owner, "alice@example.com");
    }

    #[test]
    fn first_match_wins_for_owner() {
        let meta = parse_headers("# @author first\n# @author second");
        assert_eq!(meta.owner, "first");
    }

    #[test]
    fn docstring_sets_purpose() {
        let mut meta = ExtractedMetadata::default();
        parse_python_docstring(r#""""Verify patch freeze consistency.""""#, &mut meta);
        assert_eq!(meta.purpose, "Verify patch freeze consistency.");
    }

    #[test]
    fn docstring_does_not_overwrite_purpose() {
        let mut meta = ExtractedMetadata {
            purpose: "from header".to_string(),
            ..Default::default()
        };
        parse_python_docstring(r#""""From docstring.""""#, &mut meta);
        assert_eq!(meta.purpose, "from header");
    }

    // -----------------------------------------------------------------------
    // parse_history_entry tests
    // -----------------------------------------------------------------------

    #[test]
    fn history_entry_date_author_summary() {
        let e = parse_history_entry("2024-05-10 alice Fixed timeout handling");
        assert_eq!(e.raw, "2024-05-10 alice Fixed timeout handling");
        assert_eq!(e.date.as_deref(), Some("2024-05-10"));
        assert_eq!(e.author.as_deref(), Some("alice"));
        assert_eq!(e.version, None);
        assert_eq!(e.summary.as_deref(), Some("Fixed timeout handling"));
    }

    #[test]
    fn history_entry_author_date_summary() {
        let e = parse_history_entry("alice 2024-05-10 Fixed timeout handling");
        assert_eq!(e.date.as_deref(), Some("2024-05-10"));
        assert_eq!(e.author.as_deref(), Some("alice"));
        assert_eq!(e.version, None);
        assert_eq!(e.summary.as_deref(), Some("Fixed timeout handling"));
    }

    #[test]
    fn history_entry_version_date_author_summary() {
        let e = parse_history_entry("1.2.3 2024-05-10 alice Fixed timeout handling");
        assert_eq!(e.date.as_deref(), Some("2024-05-10"));
        assert_eq!(e.author.as_deref(), Some("alice"));
        assert_eq!(e.version.as_deref(), Some("1.2.3"));
        assert_eq!(e.summary.as_deref(), Some("Fixed timeout handling"));
    }

    #[test]
    fn history_entry_summary_only() {
        let e = parse_history_entry("Fixed timeout handling");
        assert_eq!(e.raw, "Fixed timeout handling");
        assert_eq!(e.date, None);
        assert_eq!(e.author, None);
        assert_eq!(e.version, None);
        assert_eq!(e.summary.as_deref(), Some("Fixed timeout handling"));
    }

    #[test]
    fn history_entry_date_only() {
        let e = parse_history_entry("2024-05-10");
        assert_eq!(e.date.as_deref(), Some("2024-05-10"));
        assert_eq!(e.author, None);
        assert_eq!(e.version, None);
        assert_eq!(e.summary, None);
    }

    #[test]
    fn history_entry_empty_string() {
        let e = parse_history_entry("  ");
        assert_eq!(e.raw, "");
        assert_eq!(e.date, None);
        assert_eq!(e.author, None);
        assert_eq!(e.version, None);
        assert_eq!(e.summary, None);
    }

    #[test]
    fn history_entry_raw_preserved() {
        let raw = "gibberish :: not a real entry ??";
        let e = parse_history_entry(raw);
        assert_eq!(e.raw, raw);
        assert_eq!(e.summary.as_deref(), Some(raw));
    }

    #[test]
    fn history_entry_version_without_date_not_extracted() {
        // When there is no date, version should NOT be extracted to avoid
        // misclassifying numbers in a summary.
        let e = parse_history_entry("1.2.3 Some summary text");
        assert_eq!(e.date, None);
        assert_eq!(e.version, None);
        assert_eq!(e.summary.as_deref(), Some("1.2.3 Some summary text"));
    }

    // -----------------------------------------------------------------------
    // Multiple @history lines → array storage
    // -----------------------------------------------------------------------

    #[test]
    fn multiple_history_lines_stored_as_array() {
        let content = "\
# @history 2024-05-10 alice Fixed timeout handling
# @history 2024-04-01 bob Initial implementation
";
        let meta = parse_headers(content);

        let history = meta.fields.get("history").expect("history key must exist");
        let arr = history.as_array().expect("history must be a JSON array");
        assert_eq!(arr.len(), 2);
        assert_eq!(
            arr[0].as_str().unwrap(),
            "2024-05-10 alice Fixed timeout handling"
        );
        assert_eq!(
            arr[1].as_str().unwrap(),
            "2024-04-01 bob Initial implementation"
        );
    }

    #[test]
    fn multiple_history_lines_produce_history_entries() {
        let content = "\
# @history 2024-05-10 alice Fixed timeout handling
# @history 2024-04-01 bob Initial implementation
";
        let meta = parse_headers(content);

        let entries = meta
            .fields
            .get("history_entries")
            .expect("history_entries key must exist");
        let arr = entries
            .as_array()
            .expect("history_entries must be a JSON array");
        assert_eq!(arr.len(), 2);

        let first = arr[0].as_object().unwrap();
        assert_eq!(
            first["raw"].as_str().unwrap(),
            "2024-05-10 alice Fixed timeout handling"
        );
        assert_eq!(first["date"].as_str().unwrap(), "2024-05-10");
        assert_eq!(first["author"].as_str().unwrap(), "alice");
        assert!(first["version"].is_null());
        assert_eq!(first["summary"].as_str().unwrap(), "Fixed timeout handling");

        let second = arr[1].as_object().unwrap();
        assert_eq!(second["date"].as_str().unwrap(), "2024-04-01");
        assert_eq!(second["author"].as_str().unwrap(), "bob");
        assert_eq!(
            second["summary"].as_str().unwrap(),
            "Initial implementation"
        );
    }

    #[test]
    fn no_history_lines_produces_no_history_keys() {
        let meta = parse_headers("# @brief No history here\n");
        assert!(!meta.fields.contains_key("history"));
        assert!(!meta.fields.contains_key("history_entries"));
    }

    #[test]
    fn single_history_line_is_wrapped_in_array() {
        let meta = parse_headers("# @history 2024-01-15 carol Deploy fix\n");
        let arr = meta
            .fields
            .get("history")
            .and_then(|v| v.as_array())
            .expect("history must be array even with one entry");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str().unwrap(), "2024-01-15 carol Deploy fix");
    }

    #[test]
    fn malformed_history_line_does_not_panic() {
        // Ensures the parser survives unexpected input without crashing.
        // The empty-value check in parse_header_comments skips blank-after-trim
        // lines, so "# @history " (just whitespace) is dropped.
        let meta = parse_headers("# @history !!!\n# @history 2024-13-99 bad date\n");
        let entries = meta
            .fields
            .get("history_entries")
            .and_then(|v| v.as_array())
            .expect("history_entries array must be present");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["raw"].as_str().unwrap(), "!!!");
        assert_eq!(entries[1]["raw"].as_str().unwrap(), "2024-13-99 bad date");
    }
}
