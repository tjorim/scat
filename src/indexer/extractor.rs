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
    /// Related-script paths declared via vc's `@parentfile`/`@childfile`/
    /// `@inputfile`/`@outputfile`/`@paramfile` header keywords (see
    /// [`parse_related_file_keywords`]). Displayed as-is by `scat show`, and
    /// also fed into dependency-edge resolution as high-confidence
    /// author-declared references — see `extract_for_insert` in
    /// `builder/pipeline.rs`.
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
            re: Regex::new(r"(?i)@scripttype\s+(.+)").unwrap(),
            field_name: "scripttype",
            metadata_key: "scripttype",
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

/// Return only the leading run of comment lines. Metadata belongs to the
/// file's header, so comment-looking text after the first executable line is
/// intentionally excluded.
fn leading_comment_block(content: &str) -> String {
    content
        .lines()
        .take_while(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("<#") || trimmed.starts_with("//") || trimmed.starts_with('#')
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_header_comments(content: &str, meta: &mut ExtractedMetadata) {
    let header = leading_comment_block(content);
    let mut found: HashSet<&'static str> = HashSet::new();
    let mut history_raw: Vec<String> = Vec::new();

    for line in header.lines().take(40) {
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

    parse_related_file_keywords(&header, meta);
    parse_block_keywords(&header, meta);
}

// ---------------------------------------------------------------------------
// vc related-file keyword parsing (@parentfile/@childfile/@inputfile/
// @outputfile/@paramfile)
// ---------------------------------------------------------------------------

struct RelatedFilePattern {
    re: Regex,
    /// Metadata key each matched value is grouped under in `meta.fields`
    /// (stored as a JSON array, since a script can declare more than one).
    metadata_key: &'static str,
}

static RELATED_FILE_PATTERNS: std::sync::LazyLock<Vec<RelatedFilePattern>> =
    std::sync::LazyLock::new(|| {
        vec![
            RelatedFilePattern {
                re: Regex::new(r"(?i)@parentfile\s+(.+)").unwrap(),
                metadata_key: "parentfile",
            },
            RelatedFilePattern {
                re: Regex::new(r"(?i)@childfile\s+(.+)").unwrap(),
                metadata_key: "childfile",
            },
            RelatedFilePattern {
                re: Regex::new(r"(?i)@inputfile\s+(.+)").unwrap(),
                metadata_key: "inputfile",
            },
            RelatedFilePattern {
                re: Regex::new(r"(?i)@outputfile\s+(.+)").unwrap(),
                metadata_key: "outputfile",
            },
            RelatedFilePattern {
                re: Regex::new(r"(?i)@paramfile\s+(.+)").unwrap(),
                metadata_key: "paramfile",
            },
        ]
    });

/// Parse vc's `@parentfile`/`@childfile`/`@inputfile`/`@outputfile`/
/// `@paramfile` header keywords — author-declared paths to other managed
/// scripts (see docs/VC_CONTRACT.md's "Metadata conventions" section).
///
/// Unlike the single-value keywords above, each of these may appear on
/// multiple lines (e.g. a script with several `@inputfile` entries), so every
/// match is kept. Per-keyword values are stored as JSON arrays under their
/// own `meta.fields` key (mirroring `@history`'s `history`/`history_entries`
/// arrays); all of them together also populate `meta.related`, which
/// `extract_for_insert` (`builder/pipeline.rs`) folds into the same
/// path-literal "referenced" dependency-edge candidates as the regex and
/// Ansible-YAML extractors use — an edge that doesn't resolve to an indexed
/// script is dropped later, so a stale or external path here is harmless.
fn parse_related_file_keywords(content: &str, meta: &mut ExtractedMetadata) {
    // Document order, not grouped by keyword yet — `meta.related` should read
    // in the order the author declared these, not alphabetically by keyword.
    let mut matches: Vec<(&'static str, String)> = Vec::new();

    for line in content.lines().take(40) {
        let stripped = strip_comment_prefix(line);
        for pat in RELATED_FILE_PATTERNS.iter() {
            if let Some(cap) = pat.re.captures(stripped) {
                let value = cap[1].trim().to_string();
                if value.is_empty() {
                    continue;
                }
                trace!(field = pat.metadata_key, value = %value, "parsed related-file header field");
                matches.push((pat.metadata_key, value));
            }
        }
    }

    let mut seen: HashSet<String> = meta.related.iter().cloned().collect();
    for (metadata_key, value) in matches {
        if seen.insert(value.clone()) {
            meta.related.push(value.clone());
        }
        match meta.fields.get_mut(metadata_key) {
            Some(serde_json::Value::Array(arr)) => arr.push(serde_json::Value::String(value)),
            _ => {
                meta.fields.insert(
                    metadata_key.to_string(),
                    serde_json::Value::Array(vec![serde_json::Value::String(value)]),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// vc block keyword parsing (@usage/@endusage, @description/@enddescription)
// ---------------------------------------------------------------------------

/// How far into the file to look for a block keyword's opening marker.
/// Generous compared to the single-line keywords' 40-line window (these
/// blocks can follow a longer run of single-value header fields), but still
/// bounded — a marker string appearing this deep in the file is far more
/// likely to be incidental than a real header block.
const BLOCK_KEYWORD_SCAN_LINES: usize = 200;

/// Longest a block is allowed to run before its closing marker must appear.
/// Bounds the cost of a missing/misspelled closing marker, which would
/// otherwise make every following line look like part of the block.
const BLOCK_KEYWORD_MAX_LINES: usize = 200;

/// Parse one `@<open> ... @<close>` block keyword (e.g. `@usage`/`@endusage`)
/// from `content`, returning its content lines (comment prefix stripped,
/// blank lines preserved) if both markers are found within the bounds above.
/// `open` and `close` are matched case-insensitively as the *entire*
/// stripped line content — `## @usage` opens, `## some usage text` does not.
fn parse_block_keyword(content: &str, open: &str, close: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines
        .iter()
        .take(BLOCK_KEYWORD_SCAN_LINES)
        .position(|line| strip_comment_prefix(line).eq_ignore_ascii_case(open))?;

    let mut collected = Vec::new();
    for line in lines.iter().skip(start + 1).take(BLOCK_KEYWORD_MAX_LINES) {
        let stripped = strip_comment_prefix(line);
        if stripped.eq_ignore_ascii_case(close) {
            return Some(collected);
        }
        collected.push(stripped.to_string());
    }
    // No closing marker within the bound — treat as absent rather than
    // guessing where an unterminated block was meant to end.
    None
}

/// Parse vc's `@usage`/`@endusage` and `@description`/`@enddescription`
/// block keywords (see docs/VC_CONTRACT.md's "Metadata conventions"
/// section) into `meta.fields`, joined back into a single multi-line string
/// per block. Unlike the single-line keywords, these carry free-form prose
/// rather than one value per line.
///
/// When no `@brief`/`@purpose` line set `meta.purpose`, the description
/// block's first line fills it instead — mirroring how a Python docstring's
/// first line already does the same, and for the same reason: some scripts
/// only document their purpose in the longer-form block.
fn parse_block_keywords(content: &str, meta: &mut ExtractedMetadata) {
    if let Some(lines) = parse_block_keyword(content, "@usage", "@endusage") {
        meta.fields.insert(
            "usage".to_string(),
            serde_json::Value::String(lines.join("\n")),
        );
    }

    if let Some(lines) = parse_block_keyword(content, "@description", "@enddescription") {
        if meta.purpose.is_empty()
            && let Some(first) = lines.iter().find(|l| !l.is_empty())
        {
            meta.purpose = first.clone();
        }
        meta.fields.insert(
            "description".to_string(),
            serde_json::Value::String(lines.join("\n")),
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
    fn parses_scripttype_into_fields() {
        let meta = parse_headers("# @scripttype shell");
        assert_eq!(
            meta.fields.get("scripttype").and_then(|v| v.as_str()),
            Some("shell")
        );
    }

    #[test]
    fn ignores_metadata_markers_in_the_body_after_executable_code() {
        let meta = parse_headers(
            "# @brief Header purpose\n\
             echo running\n\
             # @scripttype shell\n\
             # @childfile /catalog/scripts/worker.sh\n\
             # @usage\n\
             # ./body-only.sh\n\
             # @endusage\n\
             # @description\n\
             # Body-only description\n\
             # @enddescription\n",
        );

        assert_eq!(meta.purpose, "Header purpose");
        assert!(!meta.fields.contains_key("scripttype"));
        assert!(!meta.fields.contains_key("usage"));
        assert!(!meta.fields.contains_key("description"));
        assert!(meta.related.is_empty());
        assert!(!meta.fields.contains_key("childfile"));
    }

    // -----------------------------------------------------------------------
    // Block keywords: @usage/@endusage, @description/@enddescription
    // -----------------------------------------------------------------------

    #[test]
    fn parses_usage_block_into_a_joined_string() {
        let content = "\
# @usage
# ./deploy.sh <env> <version>
#   env: target environment
#   version: release tag
# @endusage
";
        let meta = parse_headers(content);
        let usage = meta.fields.get("usage").and_then(|v| v.as_str()).unwrap();
        assert_eq!(
            usage,
            "./deploy.sh <env> <version>\nenv: target environment\nversion: release tag"
        );
    }

    #[test]
    fn parses_description_block_and_fills_purpose_from_first_line() {
        let content = "\
# @description
# Deploys the release to production.
#
# Retries on transient failures.
# @enddescription
";
        let meta = parse_headers(content);
        let description = meta
            .fields
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(
            description,
            "Deploys the release to production.\n\nRetries on transient failures."
        );
        assert_eq!(meta.purpose, "Deploys the release to production.");
    }

    #[test]
    fn description_block_does_not_overwrite_an_explicit_brief() {
        let content = "\
# @brief The real one-line purpose
# @description
# A longer explanation that should not replace @brief.
# @enddescription
";
        let meta = parse_headers(content);
        assert_eq!(meta.purpose, "The real one-line purpose");
    }

    #[test]
    fn block_keyword_without_closing_marker_is_ignored() {
        let content = "\
# @usage
# ./deploy.sh <env>
# (no closing marker)
";
        let meta = parse_headers(content);
        assert!(
            !meta.fields.contains_key("usage"),
            "an unterminated block must not be guessed at"
        );
    }

    #[test]
    fn block_keyword_is_case_insensitive_and_ignores_partial_matches() {
        let content = "\
# @USAGE
# some text mentioning @usage inline should not close the block
# @EndUsage
";
        let meta = parse_headers(content);
        let usage = meta.fields.get("usage").and_then(|v| v.as_str()).unwrap();
        assert_eq!(
            usage,
            "some text mentioning @usage inline should not close the block"
        );
    }

    // -----------------------------------------------------------------------
    // Related-file keywords: @parentfile/@childfile/@inputfile/@outputfile/@paramfile
    // -----------------------------------------------------------------------

    #[test]
    fn parses_parentfile_into_related() {
        let meta = parse_headers("# @parentfile launcher.sh");
        assert_eq!(meta.related, vec!["launcher.sh".to_string()]);
        assert_eq!(
            meta.fields
                .get("parentfile")
                .and_then(|v| v.as_array())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn collects_all_five_related_file_keywords() {
        let content = "\
# @parentfile launcher.sh
# @childfile worker.sh
# @inputfile config.yaml
# @outputfile report.csv
# @paramfile defaults.json
";
        let meta = parse_headers(content);
        assert_eq!(
            meta.related,
            vec![
                "launcher.sh".to_string(),
                "worker.sh".to_string(),
                "config.yaml".to_string(),
                "report.csv".to_string(),
                "defaults.json".to_string(),
            ]
        );
    }

    #[test]
    fn keeps_multiple_lines_of_the_same_related_file_keyword() {
        // Unlike @author/@brief, these are not first-match-wins: a script can
        // declare several inputs.
        let meta = parse_headers("# @inputfile a.csv\n# @inputfile b.csv\n");
        assert_eq!(meta.related, vec!["a.csv".to_string(), "b.csv".to_string()]);
        let arr = meta.fields["inputfile"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn dedupes_identical_related_file_values() {
        let meta = parse_headers("# @parentfile shared.sh\n# @childfile shared.sh\n");
        assert_eq!(meta.related, vec!["shared.sh".to_string()]);
    }

    #[test]
    fn ignores_empty_related_file_value() {
        let meta = parse_headers("# @parentfile   \n");
        assert!(meta.related.is_empty());
        assert!(!meta.fields.contains_key("parentfile"));
    }

    #[test]
    fn no_related_file_keywords_leaves_related_empty() {
        let meta = parse_headers("# @brief Nothing declared here\n");
        assert!(meta.related.is_empty());
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
