use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::core::db::{JsonRow, row_to_map};
use crate::core::embeddings::EmbeddingsSidecar;
use crate::error::{Error, Result};

mod checkout;
mod deps;
mod diff;
mod query;
mod stats;

pub use diff::compare_catalogs;

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
/// Dependency graph for one logical script path.
pub struct DependencyGraph {
    /// Outbound dependencies from the requested script.
    pub uses: Vec<DependencyEntry>,
    /// Scripts that depend on the requested script.
    pub used_by: Vec<JsonRow>,
}

#[derive(Debug, Serialize)]
/// One outbound dependency edge with optional resolved metadata.
pub struct DependencyEntry {
    /// Dependency path as referenced by the source script.
    pub depends_on_path: String,
    /// Resolved logical path when indexed, otherwise mirrors `depends_on_path`.
    pub logical_path: String,
    /// Resolved dependency language, if indexed.
    pub language: Value,
    /// Resolved dependency owner, if indexed.
    pub owner: Value,
    /// Resolved dependency purpose, if indexed.
    pub purpose: Value,
    /// Whether the dependency resolved to an indexed script.
    pub indexed: bool,
    /// Edge kind: `import` (language-level) or `referenced` (path literal).
    pub kind: String,
}

/// Direction of a transitive dependency traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeDirection {
    /// Follow outbound `uses` edges.
    Uses,
    /// Follow inbound `used by` edges.
    UsedBy,
}

#[derive(Debug, Serialize)]
/// One node in a transitive dependency tree.
pub struct DepsTreeNode {
    /// Resolved logical path, or the raw dependency path when unresolved.
    pub logical_path: String,
    /// Whether the node resolved to an indexed script.
    pub indexed: bool,
    /// The path appears among its own ancestors; children are not expanded.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cycle: bool,
    /// The subtree was already expanded earlier in the same tree.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub repeated: bool,
    /// Children exist but were cut off by the depth limit.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Edge kind by which the parent reaches this node: `import` or
    /// `referenced`. `None` on the root, which has no incoming edge.
    #[serde(rename = "via", skip_serializing_if = "Option::is_none")]
    pub via_kind: Option<String>,
    /// Expanded child nodes.
    pub children: Vec<Self>,
}

#[derive(Debug, Serialize)]
/// Aggregated script counts for stats output.
pub struct StatsResult {
    /// Total count of indexed scripts.
    pub total_scripts: i64,
    /// Script counts grouped by language.
    pub by_language: Vec<LangCount>,
    /// Script counts grouped by owner.
    pub by_owner: Vec<OwnerCount>,
    /// Scripts with the most dependents, highest in-degree first
    /// (capped at [`MOST_DEPENDED_UPON_LIMIT`]).
    pub most_depended_upon: Vec<DependentCount>,
    /// Revision statistics when vc data has been indexed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revisions: Option<RevisionStats>,
}

/// Cap on [`StatsResult::most_depended_upon`] — a ranking, not a full dump.
pub const MOST_DEPENDED_UPON_LIMIT: usize = 10;

#[derive(Debug, Serialize)]
/// Count bucket for a language.
pub struct LangCount {
    /// Language key.
    pub language: String,
    /// Script count for `language`.
    pub count: i64,
}

#[derive(Debug, Serialize)]
/// Count bucket for an owner.
pub struct OwnerCount {
    /// Owner value.
    pub owner: String,
    /// Script count for `owner`.
    pub count: i64,
}

#[derive(Debug, Serialize)]
/// A script and how many other scripts depend on it (in-degree).
pub struct DependentCount {
    /// Logical path of the depended-upon script.
    pub logical_path: String,
    /// Number of dependency edges resolving to this script.
    pub count: i64,
}

/// Aggregated revision and checkout counts for stats output.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct RevisionStats {
    /// Number of scripts with at least one DEVELOP revision.
    pub scripts_with_active_checkouts: i64,
    /// Number of scripts with at least one ARCHIVE revision.
    pub scripts_with_archive_entries: i64,
    /// Total DEVELOP revision rows stored in the catalog.
    pub total_develop_revision_files: i64,
    /// Total ARCHIVE revision rows stored in the catalog.
    pub total_archive_revision_files: i64,
    /// Number of scripts with at least one WORKING revision.
    pub scripts_with_working_versions: i64,
    /// Total WORKING revision rows stored in the catalog.
    ///
    /// These are the checked-in version copies vc retains in the working
    /// directory beside the active symlink. They are counted here because
    /// they are otherwise invisible: they are deliberately kept out of
    /// `scripts`, so without a line of their own a catalog rebuild that
    /// reclassifies them looks like an unexplained drop in `total_scripts`.
    pub total_working_revision_files: i64,
    /// Number of scripts that have DEVELOP revisions owned by more than one user.
    pub scripts_checked_out_by_multiple_users: i64,
}

#[derive(Debug, Serialize)]
/// Stored metadata about the most recent index build.
pub struct IndexMetadata {
    /// Build timestamp from `index_metadata`.
    pub build_timestamp: Value,
    /// Schema version persisted in database.
    pub schema_version: Value,
    /// Schema version expected by the running binary.
    pub current_schema_version: i64,
}

#[derive(Debug, Serialize, Clone)]
/// One audit finding row.
pub struct AuditFinding {
    /// Audit check name.
    pub check: String,
    /// Finding severity (`error`, `warn`, `info`).
    pub severity: String,
    /// Logical path associated with this finding.
    pub logical_path: String,
    /// Human-readable finding detail.
    pub detail: String,
}

#[derive(Debug, Serialize, Default, Clone)]
/// Summary counters for audit findings by severity.
pub struct AuditSummary {
    /// Number of error-severity findings.
    pub error: usize,
    /// Number of warn-severity findings.
    pub warn: usize,
    /// Number of info-severity findings.
    pub info: usize,
}

#[derive(Debug, Serialize, Clone)]
/// Combined audit findings and summary.
pub struct AuditResult {
    /// Detailed findings.
    pub findings: Vec<AuditFinding>,
    /// Aggregated finding counts.
    pub summary: AuditSummary,
}

/// Extra inputs to [`SearchApi::audit`] beyond the catalog database itself.
///
/// Grouped into one struct, rather than added as more positional
/// parameters, because `sidecar` is optional (the embeddings sidecar isn't
/// always published) while the two thresholds have sane defaults most
/// callers won't want to think about.
pub struct AuditOptions<'a> {
    /// Embeddings sidecar backing the `near-duplicates` and `outliers`
    /// checks. `None` disables both checks unless explicitly requested, in
    /// which case `audit` returns a validation error.
    pub sidecar: Option<&'a EmbeddingsSidecar>,
    /// Cosine-similarity threshold at/above which a script pair counts as
    /// a near-duplicate.
    pub near_duplicate_threshold: f32,
    /// Cosine-similarity threshold below which a script's best match
    /// counts as an outlier.
    pub outlier_threshold: f32,
}

impl Default for AuditOptions<'_> {
    fn default() -> Self {
        Self {
            sidecar: None,
            near_duplicate_threshold: 0.97,
            outlier_threshold: 0.3,
        }
    }
}

#[derive(Debug, Serialize)]
/// Difference between two complete catalog snapshots.
pub struct CatalogDiff {
    /// Scripts present only in the new catalog.
    pub added: Vec<CatalogDiffScript>,
    /// Scripts present only in the old catalog.
    pub removed: Vec<CatalogDiffScript>,
    /// Scripts present in both catalogs but changed.
    pub changed: Vec<CatalogDiffChange>,
}

#[derive(Debug, Serialize)]
/// Compact script row used in catalog diff output.
pub struct CatalogDiffScript {
    /// Logical path of the script.
    pub logical_path: String,
    /// Script language, when known.
    pub language: Value,
    /// Script owner, when known.
    pub owner: Value,
}

#[derive(Debug, Serialize)]
/// Per-script changes between catalog snapshots.
pub struct CatalogDiffChange {
    /// Logical path of the changed script.
    pub logical_path: String,
    /// Field changes, keyed by field name, as `[old, new]`.
    pub fields: BTreeMap<String, [Value; 2]>,
    /// Dependency paths added in the new catalog.
    pub deps_added: Vec<String>,
    /// Dependency paths removed from the old catalog.
    pub deps_removed: Vec<String>,
}

// ---------------------------------------------------------------------------
// SearchApi
// ---------------------------------------------------------------------------

/// Query facade over the indexed SQLite catalog.
pub struct SearchApi {
    /// Open SQLite connection used for all queries.
    pub conn: Connection,
}

fn query_script_rows(conn: &Connection, sql: &str, params: Vec<SqlValue>) -> Result<Vec<JsonRow>> {
    let mut stmt = conn.prepare_cached(sql)?;
    let cols: Vec<String> = stmt
        .column_names()
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    stmt.query_map(params_from_iter(params), |row| Ok(row_to_map(row, &cols)))?
        .map(|r| r.map_err(Error::from))
        .collect()
}

/// Open a read-only database and construct a [`SearchApi`].
pub fn open_search_api(path: &std::path::Path) -> Result<SearchApi> {
    let conn = crate::core::db::open_db(path)?;
    Ok(SearchApi::new(conn))
}
