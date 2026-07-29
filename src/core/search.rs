use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params_from_iter, types::Value as SqlValue};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::core::db::{
    JsonRow, SCHEMA_VERSION, append_script_filters, fts_query_filtered, query_rows, row_string,
    row_to_map,
};
use crate::core::script_view::{ListField, ScriptView, logical_parent_dir};
use crate::core::vc::{REVISION_TYPE_ARCHIVE, REVISION_TYPE_DEVELOP, REVISION_TYPE_WORKING};
use crate::error::{Error, Result};

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
    pub children: Vec<DepsTreeNode>,
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
    /// Revision statistics when vc data has been indexed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revisions: Option<RevisionStats>,
}

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
    let mut stmt = conn.prepare(sql)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    stmt.query_map(params_from_iter(params), |row| Ok(row_to_map(row, &cols)))?
        .map(|r| r.map_err(Error::from))
        .collect()
}

impl SearchApi {
    /// Create a query API from an open connection.
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }

    // ------------------------------------------------------------------
    // Full-text search
    // ------------------------------------------------------------------

    /// Run full-text search with optional language, owner, and tag filters.
    pub fn search_with_filters(
        &self,
        query: &str,
        limit: usize,
        language: Option<&str>,
        owner: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<JsonRow>> {
        fts_query_filtered(&self.conn, query, limit, language, owner, tag)
    }

    /// Search by partial logical path with optional language, owner, and tag filters.
    pub fn search_by_path_with_filters(
        &self,
        query: &str,
        limit: usize,
        language: Option<&str>,
        owner: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<JsonRow>> {
        let lim = limit as i64;
        let mut sql =
            String::from("SELECT * FROM scripts WHERE INSTR(LOWER(logical_path), LOWER(?)) > 0");
        let mut params = vec![SqlValue::Text(query.to_string())];
        append_script_filters(&mut sql, &mut params, None, language, owner, tag);
        sql.push_str(" ORDER BY logical_path LIMIT ?");
        params.push(SqlValue::Integer(lim));
        query_script_rows(&self.conn, &sql, params)
    }

    // ------------------------------------------------------------------
    // Regex search
    // ------------------------------------------------------------------

    /// Search scripts by regex pattern matched against `logical_path` and `purpose`.
    ///
    /// Rows are fetched from SQLite (pre-filtered by any supported metadata
    /// filters), then filtered in Rust using the compiled [`Regex`]. Up to
    /// `limit` matches are returned, ordered by `logical_path`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] when the regex pattern is invalid or a database error
    /// occurs.
    pub fn search_by_regex_with_filters(
        &self,
        pattern: &str,
        limit: usize,
        language: Option<&str>,
        owner: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<JsonRow>> {
        let re = Regex::new(pattern).map_err(|e| {
            Error::Validation(format!("invalid regex pattern {:?}: {}", pattern, e))
        })?;

        let mut sql = String::from("SELECT * FROM scripts WHERE 1=1");
        let mut params = Vec::new();
        append_script_filters(&mut sql, &mut params, None, language, owner, tag);
        sql.push_str(" ORDER BY logical_path");

        let mut stmt = self.conn.prepare(&sql)?;
        let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = stmt.query(params_from_iter(params))?;

        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            let json_row = row_to_map(row, &cols);
            let path = json_row
                .get("logical_path")
                .and_then(Value::as_str)
                .unwrap_or("");
            let purpose = json_row
                .get("purpose")
                .and_then(Value::as_str)
                .unwrap_or("");
            if re.is_match(path) || re.is_match(purpose) {
                results.push(json_row);
                if results.len() >= limit {
                    break;
                }
            }
        }

        Ok(results)
    }

    // ------------------------------------------------------------------
    // Listing
    // ------------------------------------------------------------------

    /// List scripts with optional language/owner/tag filters and pagination.
    pub fn list_scripts(
        &self,
        language: Option<&str>,
        owner: Option<&str>,
        tag: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<JsonRow>> {
        let lim = limit as i64;
        let off = offset as i64;
        let mut sql = String::from("SELECT * FROM scripts WHERE 1=1");
        let mut params = Vec::new();
        append_script_filters(&mut sql, &mut params, None, language, owner, tag);
        sql.push_str(" ORDER BY logical_path LIMIT ? OFFSET ?");
        params.push(SqlValue::Integer(lim));
        params.push(SqlValue::Integer(off));
        query_script_rows(&self.conn, &sql, params)
    }

    // ------------------------------------------------------------------
    // Detail
    // ------------------------------------------------------------------

    /// Return one script row by logical path.
    pub fn get_script(&self, logical_path: &str) -> Result<Option<JsonRow>> {
        let rows = query_rows(
            &self.conn,
            "SELECT * FROM scripts WHERE logical_path = ?",
            &[&logical_path],
        )?;
        Ok(rows.into_iter().next())
    }

    // ------------------------------------------------------------------
    // Folder siblings
    // ------------------------------------------------------------------

    /// Return the indexed scripts that live directly in `dir` (one level
    /// deep). `dir` should be a directory path as returned by
    /// [`logical_parent_dir`] (for example `/catalog/scripts`, or `/` for
    /// the root). Returns an empty list for an empty `dir` (a bare,
    /// non-absolute logical path has no folder to list).
    pub fn scripts_in_dir(&self, dir: &str) -> Result<Vec<JsonRow>> {
        if dir.is_empty() {
            return Ok(vec![]);
        }
        // Escape LIKE metacharacters in the folder path so a literal `%`/`_`
        // in a directory name can't widen the match.
        let escaped_dir = dir
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let prefix = if dir == "/" {
            escaped_dir
        } else {
            format!("{escaped_dir}/")
        };
        let one_level = format!("{prefix}%");
        let two_level = format!("{prefix}%/%");

        query_script_rows(
            &self.conn,
            "SELECT * FROM scripts
             WHERE logical_path LIKE ?1 ESCAPE '\\'
               AND logical_path NOT LIKE ?2 ESCAPE '\\'
             ORDER BY logical_path",
            vec![SqlValue::Text(one_level), SqlValue::Text(two_level)],
        )
    }

    /// Return the other indexed scripts that live directly in the same
    /// parent folder as `logical_path` (one level deep, excluding
    /// `logical_path` itself).
    pub fn siblings(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        let mut rows = self.scripts_in_dir(logical_parent_dir(logical_path))?;
        rows.retain(|row| row_string(row, "logical_path") != logical_path);
        Ok(rows)
    }

    /// Return the names (single path components, no separators) of the
    /// immediate subdirectories of `dir` that contain at least one indexed
    /// script, sorted and deduplicated. Directories only exist in the
    /// catalog as path prefixes, so this derives them from `logical_path`
    /// rows at least one level deeper than `dir`.
    pub fn subdirs_of(&self, dir: &str) -> Result<Vec<String>> {
        if dir.is_empty() {
            return Ok(vec![]);
        }
        let escaped_dir = dir
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let (escaped_prefix, prefix) = if dir == "/" {
            (escaped_dir, dir.to_string())
        } else {
            (format!("{escaped_dir}/"), format!("{dir}/"))
        };
        let pattern = format!("{escaped_prefix}%/%");
        // SQLite substr is 1-indexed and counts characters for text, so the
        // unescaped prefix's char count locates the first char after it.
        let start = (prefix.chars().count() + 1) as i64;

        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT substr(substr(logical_path, ?2), 1,
                                    instr(substr(logical_path, ?2), '/') - 1) AS name
             FROM scripts
             WHERE logical_path LIKE ?1 ESCAPE '\\'
             ORDER BY name",
        )?;
        let rows: Result<Vec<String>> = stmt
            .query_map(
                rusqlite::params![SqlValue::Text(pattern), SqlValue::Integer(start)],
                |row| row.get(0),
            )?
            .map(|r| r.map_err(Error::from))
            .collect();
        rows
    }

    // ------------------------------------------------------------------
    // Related scripts
    // ------------------------------------------------------------------

    /// Return related scripts from explicit relations and dependency edges.
    pub fn related_scripts(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        let script = match self.get_script(logical_path)? {
            Some(s) => s,
            None => return Ok(vec![]),
        };

        let mut related: std::collections::BTreeSet<String> = Default::default();

        if let Some(Value::String(raw)) = script.get("related")
            && let Ok(Value::Array(paths)) = serde_json::from_str::<Value>(raw)
        {
            for p in paths {
                if let Value::String(s) = p {
                    related.insert(s);
                }
            }
        }

        let script_id = script.get("id").and_then(Value::as_i64).unwrap_or(0);

        let deps = query_rows(
            &self.conn,
            "SELECT s.logical_path
             FROM dependencies d
             JOIN scripts s ON s.id = d.resolved_script_id
             WHERE d.script_id = ?",
            &[&script_id],
        )?;
        for row in deps {
            if let Some(Value::String(p)) = row.get("logical_path") {
                related.insert(p.clone());
            }
        }

        let rev = query_rows(
            &self.conn,
            "SELECT s.logical_path
             FROM scripts s JOIN dependencies d ON d.script_id = s.id
             WHERE d.resolved_script_id = ?",
            &[&script_id],
        )?;
        for row in rev {
            if let Some(Value::String(p)) = row.get("logical_path") {
                related.insert(p.clone());
            }
        }

        related.remove(logical_path);
        if related.is_empty() {
            return Ok(vec![]);
        }

        const SQLITE_PARAM_LIMIT: usize = 999;
        let mut all_rows = Vec::new();
        let paths_vec: Vec<&str> = related.iter().map(String::as_str).collect();

        for chunk in paths_vec.chunks(SQLITE_PARAM_LIMIT) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let sql = format!(
                "SELECT * FROM scripts WHERE logical_path IN ({placeholders}) ORDER BY logical_path"
            );
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|p| p as &dyn rusqlite::ToSql).collect();

            let mut stmt = self.conn.prepare(&sql)?;
            let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            let rows: std::result::Result<Vec<JsonRow>, _> = stmt
                .query_map(params.as_slice(), |row| Ok(row_to_map(row, &cols)))?
                .map(|r| r.map_err(Error::from))
                .collect();
            all_rows.extend(rows?);
        }

        all_rows.sort_by(|a, b| {
            let a_path = a.get("logical_path").and_then(|v| v.as_str()).unwrap_or("");
            let b_path = b.get("logical_path").and_then(|v| v.as_str()).unwrap_or("");
            a_path.cmp(b_path)
        });

        Ok(all_rows)
    }

    // ------------------------------------------------------------------
    // Dependency graph
    // ------------------------------------------------------------------

    /// Return dependency graph for a script.
    pub fn dependency_graph(&self, logical_path: &str) -> Result<DependencyGraph> {
        let script = match self.get_script(logical_path)? {
            Some(s) => s,
            None => {
                return Ok(DependencyGraph {
                    uses: vec![],
                    used_by: vec![],
                });
            }
        };
        let script_id = script.get("id").and_then(Value::as_i64).unwrap_or(0);

        let uses_rows = query_rows(
            &self.conn,
            "SELECT d.depends_on_path, d.kind, s.logical_path, s.language, s.owner, s.purpose
             FROM dependencies d
             LEFT JOIN scripts s ON s.id = d.resolved_script_id
             WHERE d.script_id = ?
             ORDER BY d.kind, d.depends_on_path",
            &[&script_id],
        )?;

        let uses = uses_rows
            .into_iter()
            .map(|row| {
                let dep = row_string(&row, "depends_on_path");
                let lp = row_string(&row, "logical_path");
                let indexed = !lp.is_empty();
                DependencyEntry {
                    logical_path: if indexed { lp } else { dep.clone() },
                    depends_on_path: dep,
                    language: row.get("language").cloned().unwrap_or(Value::Null),
                    owner: row.get("owner").cloned().unwrap_or(Value::Null),
                    purpose: row.get("purpose").cloned().unwrap_or(Value::Null),
                    indexed,
                    kind: row_string(&row, "kind"),
                }
            })
            .collect();

        let used_by = query_rows(
            &self.conn,
            "SELECT s.logical_path, s.language, s.owner, s.purpose, d.kind
             FROM scripts s JOIN dependencies d ON d.script_id = s.id
             WHERE d.resolved_script_id = ?
             ORDER BY s.logical_path",
            &[&script_id],
        )?;

        Ok(DependencyGraph { uses, used_by })
    }

    /// Return the transitive dependency tree for a script in one direction,
    /// or `None` when the script is not indexed.
    ///
    /// Traversal is cycle-safe: a path that appears among its own ancestors is
    /// emitted as a `cycle` leaf, a subtree that was already expanded earlier
    /// in the same tree is emitted as a `repeated` leaf, and nodes at
    /// `max_depth` are emitted as `truncated` leaves.
    pub fn dependency_tree(
        &self,
        logical_path: &str,
        direction: TreeDirection,
        max_depth: usize,
    ) -> Result<Option<DepsTreeNode>> {
        let script = match self.get_script(logical_path)? {
            Some(s) => s,
            None => return Ok(None),
        };
        let script_id = script.get("id").and_then(Value::as_i64);
        let root_path = row_string(&script, "logical_path");

        let mut ancestors = BTreeSet::new();
        let mut expanded = BTreeSet::new();
        self.tree_node(
            root_path,
            script_id,
            None,
            direction,
            max_depth,
            &mut ancestors,
            &mut expanded,
        )
        .map(Some)
    }

    #[allow(clippy::too_many_arguments)]
    fn tree_node(
        &self,
        logical_path: String,
        script_id: Option<i64>,
        via_kind: Option<String>,
        direction: TreeDirection,
        depth_left: usize,
        ancestors: &mut BTreeSet<String>,
        expanded: &mut BTreeSet<String>,
    ) -> Result<DepsTreeNode> {
        let mut node = DepsTreeNode {
            indexed: script_id.is_some(),
            cycle: false,
            repeated: false,
            truncated: false,
            via_kind,
            children: vec![],
            logical_path,
        };
        let Some(script_id) = script_id else {
            return Ok(node);
        };
        if ancestors.contains(&node.logical_path) {
            node.cycle = true;
            return Ok(node);
        }
        if expanded.contains(&node.logical_path) {
            node.repeated = true;
            return Ok(node);
        }

        if depth_left == 0 {
            // At the depth limit we only need to know whether *any* edge
            // exists, so skip the join/order used to materialize full rows.
            let edge_sql = match direction {
                TreeDirection::Uses => "SELECT 1 FROM dependencies WHERE script_id = ? LIMIT 1",
                TreeDirection::UsedBy => {
                    "SELECT 1 FROM dependencies WHERE resolved_script_id = ? LIMIT 1"
                }
            };
            node.truncated = self
                .conn
                .query_row(edge_sql, [script_id], |_| Ok(()))
                .optional()?
                .is_some();
            return Ok(node);
        }

        let edges = match direction {
            TreeDirection::Uses => query_rows(
                &self.conn,
                "SELECT d.depends_on_path, d.kind, s.logical_path, s.id
                 FROM dependencies d
                 LEFT JOIN scripts s ON s.id = d.resolved_script_id
                 WHERE d.script_id = ?
                 ORDER BY d.kind, COALESCE(s.logical_path, d.depends_on_path)",
                &[&script_id],
            )?,
            TreeDirection::UsedBy => query_rows(
                &self.conn,
                "SELECT s.logical_path, s.id, d.kind
                 FROM scripts s JOIN dependencies d ON d.script_id = s.id
                 WHERE d.resolved_script_id = ?
                 ORDER BY d.kind, s.logical_path",
                &[&script_id],
            )?,
        };
        if edges.is_empty() {
            return Ok(node);
        }

        ancestors.insert(node.logical_path.clone());
        for edge in edges {
            let child_id = edge.get("id").and_then(Value::as_i64);
            let child_path = match child_id {
                Some(_) => row_string(&edge, "logical_path"),
                None => row_string(&edge, "depends_on_path"),
            };
            node.children.push(self.tree_node(
                child_path,
                child_id,
                Some(row_string(&edge, "kind")),
                direction,
                depth_left - 1,
                ancestors,
                expanded,
            )?);
        }
        ancestors.remove(&node.logical_path);

        // Only remember subtrees that actually hide something when repeated.
        if !node.children.is_empty() {
            expanded.insert(node.logical_path.clone());
        }
        Ok(node)
    }

    /// Return function definitions recorded for a script.
    pub fn get_functions_defined_in(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        let script = match self.get_script(logical_path)? {
            Some(s) => s,
            None => return Ok(vec![]),
        };
        let script_id = script.get("id").and_then(Value::as_i64).unwrap_or(0);
        query_rows(
            &self.conn,
            "SELECT name, kind, line, docstring, decorators
             FROM function_definitions
             WHERE script_id = ?
             ORDER BY line, name",
            &[&script_id],
        )
    }

    /// Return all call sites targeting functions defined in `logical_path`.
    pub fn get_callers_of_functions_in(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        let script = match self.get_script(logical_path)? {
            Some(s) => s,
            None => return Ok(vec![]),
        };
        let script_id = script.get("id").and_then(Value::as_i64).unwrap_or(0);
        query_rows(
            &self.conn,
            "SELECT DISTINCT fd.name AS target_function,
                            s.logical_path, s.language, s.owner, s.purpose,
                            fc.caller, fc.callee, fc.line, fc.resolved_target_name
             FROM function_definitions fd
             JOIN function_calls fc
               ON fc.resolved_target_script_id = fd.script_id
              AND (fc.callee = fd.name OR fc.callee LIKE ('%.' || fd.name))
             JOIN scripts s ON s.id = fc.script_id
             WHERE fd.script_id = ?
             ORDER BY fd.line, fd.name, s.logical_path, fc.line",
            &[&script_id],
        )
    }

    /// Return function-level outbound call dependencies for a script.
    pub fn get_function_dependencies(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        let script = match self.get_script(logical_path)? {
            Some(s) => s,
            None => return Ok(vec![]),
        };
        let script_id = script.get("id").and_then(Value::as_i64).unwrap_or(0);
        query_rows(
            &self.conn,
            "SELECT fc.caller, fc.callee, fc.line, fc.resolved_target_name,
                    ts.logical_path AS target_logical_path
             FROM function_calls fc
             LEFT JOIN scripts ts ON ts.id = fc.resolved_target_script_id
             WHERE fc.script_id = ?
             ORDER BY fc.line, fc.callee",
            &[&script_id],
        )
    }

    /// Return scripts that define a matching function and satisfy optional filters.
    pub fn search_scripts_by_function_with_filters(
        &self,
        name: &str,
        limit: usize,
        language: Option<&str>,
        owner: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<JsonRow>> {
        let lim = limit as i64;
        let pattern = format!("%{name}%");
        let mut sql = String::from(
            "SELECT DISTINCT s.*
             FROM function_definitions fd
             JOIN scripts s ON s.id = fd.script_id
             WHERE LOWER(fd.name) LIKE LOWER(?)",
        );
        let mut params = vec![SqlValue::Text(pattern)];
        append_script_filters(&mut sql, &mut params, Some("s."), language, owner, tag);
        sql.push_str(" ORDER BY s.logical_path LIMIT ?");
        params.push(SqlValue::Integer(lim));
        query_script_rows(&self.conn, &sql, params)
    }

    // ------------------------------------------------------------------
    // Symlinks
    // ------------------------------------------------------------------

    /// Return scripts whose symlink target is `logical_path`.
    pub fn symlinks_to(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        query_rows(
            &self.conn,
            "SELECT * FROM scripts WHERE symlink_target=? ORDER BY logical_path",
            &[&logical_path],
        )
    }

    // ------------------------------------------------------------------
    // Checkout / vc state
    // ------------------------------------------------------------------

    /// Return checkout/status rows, optionally scoped to one logical path.
    pub fn checkout_status(&self, logical_path: Option<&str>) -> Result<Vec<JsonRow>> {
        if let Some(path) = logical_path {
            if let Some(row) = self.script_checkout_status(path)? {
                return Ok(vec![row]);
            }
            return self.orphan_checkout_status(Some(path));
        }

        let mut rows = query_rows(
            &self.conn,
            "SELECT s.*,
                    r.checkout_user,
                    r.checkout_timestamp,
                    r.checkout_os,
                    r.checkout_age_seconds
             FROM scripts s
             LEFT JOIN (
                 SELECT logical_path,
                        GROUP_CONCAT(DISTINCT user)      AS checkout_user,
                        MAX(timestamp)                   AS checkout_timestamp,
                        GROUP_CONCAT(DISTINCT os_flavor) AS checkout_os,
                        MAX(age_seconds)                 AS checkout_age_seconds
                 FROM revisions
                 WHERE revision_type = ?1
                 GROUP BY logical_path
             ) r ON r.logical_path = s.logical_path
             WHERE r.checkout_user IS NOT NULL
                OR (vc_warnings IS NOT NULL AND vc_warnings != '[]')
             ORDER BY s.logical_path",
            &[&REVISION_TYPE_DEVELOP],
        )?;

        let indexed: std::collections::HashSet<String> = rows
            .iter()
            .filter_map(|r| {
                r.get("logical_path")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();

        for orphan in self.orphan_checkout_status(None)? {
            let p = orphan
                .get("logical_path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !indexed.contains(&p) {
                rows.push(orphan);
            }
        }

        rows.sort_by(|a, b| {
            let pa = a.get("logical_path").and_then(Value::as_str).unwrap_or("");
            let pb = b.get("logical_path").and_then(Value::as_str).unwrap_or("");
            pa.cmp(pb)
        });
        Ok(rows)
    }

    /// Return raw checkout rows for one logical path.
    pub fn checkouts_for(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        self.revisions_for(logical_path)
    }

    /// Return indexed revision rows for one logical path.
    pub fn revisions_for(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        query_rows(
            &self.conn,
            "SELECT logical_path, physical_path, revision_type, os_flavor, user, timestamp, age_seconds
             FROM revisions WHERE logical_path=?
             ORDER BY revision_type, timestamp DESC, physical_path",
            &[&logical_path],
        )
    }

    fn script_checkout_status(&self, logical_path: &str) -> Result<Option<JsonRow>> {
        let mut rows = query_rows(
            &self.conn,
            "SELECT s.*,
                    r.checkout_user,
                    r.checkout_timestamp,
                    r.checkout_os,
                    r.checkout_age_seconds
             FROM scripts s
             LEFT JOIN (
                 SELECT logical_path,
                        GROUP_CONCAT(DISTINCT user)      AS checkout_user,
                        MAX(timestamp)                   AS checkout_timestamp,
                        GROUP_CONCAT(DISTINCT os_flavor) AS checkout_os,
                        MAX(age_seconds)                 AS checkout_age_seconds
                 FROM revisions
                 WHERE revision_type = ?1
                 GROUP BY logical_path
             ) r ON r.logical_path = s.logical_path
             WHERE s.logical_path = ?2",
            &[&REVISION_TYPE_DEVELOP, &logical_path],
        )?;
        Ok(rows.pop())
    }

    fn orphan_checkout_status(&self, logical_path: Option<&str>) -> Result<Vec<JsonRow>> {
        let orphan_warning = serde_json::json!([{
            "kind": "checkout_without_catalog_entry",
            "message": "Revision exists in DEVELOP/ARCHIVE but no active catalog entry was indexed.",
            "details": {}
        }])
        .to_string();

        let base_rows = if let Some(path) = logical_path {
            query_rows(
                &self.conn,
                "SELECT c.logical_path,
                        GROUP_CONCAT(DISTINCT c.user)      AS checkout_user,
                        MAX(c.timestamp)                   AS checkout_timestamp,
                        GROUP_CONCAT(DISTINCT c.os_flavor) AS checkout_os,
                        MAX(c.age_seconds)                 AS checkout_age_seconds
                 FROM revisions c
                 LEFT JOIN scripts s ON s.logical_path = c.logical_path
                 WHERE s.id IS NULL
                   AND c.revision_type = ?
                   AND c.logical_path = ?
                 GROUP BY c.logical_path ORDER BY c.logical_path",
                &[&REVISION_TYPE_DEVELOP, &path],
            )?
        } else {
            query_rows(
                &self.conn,
                "SELECT c.logical_path,
                        GROUP_CONCAT(DISTINCT c.user)      AS checkout_user,
                        MAX(c.timestamp)                   AS checkout_timestamp,
                        GROUP_CONCAT(DISTINCT c.os_flavor) AS checkout_os,
                        MAX(c.age_seconds)                 AS checkout_age_seconds
                 FROM revisions c
                 LEFT JOIN scripts s ON s.logical_path = c.logical_path
                 WHERE s.id IS NULL
                   AND c.revision_type = ?
                 GROUP BY c.logical_path ORDER BY c.logical_path",
                &[&REVISION_TYPE_DEVELOP],
            )?
        };

        Ok(base_rows
            .into_iter()
            .map(|mut row| {
                row.insert("language".into(), Value::Null);
                row.insert("owner".into(), Value::Null);
                row.insert("purpose".into(), Value::Null);
                row.insert("vc_warnings".into(), Value::String(orphan_warning.clone()));
                row
            })
            .collect())
    }

    // ------------------------------------------------------------------
    // Stats
    // ------------------------------------------------------------------

    /// Return catalog statistics grouped by language and owner.
    pub fn stats(&self) -> Result<StatsResult> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM scripts", [], |r| r.get(0))?;

        let by_language: Vec<LangCount> = {
            let mut stmt = self.conn.prepare(
                "SELECT COALESCE(language,'unknown') AS language, COUNT(*) AS count
                 FROM scripts GROUP BY language ORDER BY count DESC",
            )?;
            let rows: Result<Vec<LangCount>> = stmt
                .query_map([], |row| {
                    Ok(LangCount {
                        language: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        let by_owner: Vec<OwnerCount> = {
            let mut stmt = self.conn.prepare(
                "SELECT COALESCE(owner,'unknown') AS owner, COUNT(*) AS count
                 FROM scripts GROUP BY owner ORDER BY count DESC",
            )?;
            let rows: Result<Vec<OwnerCount>> = stmt
                .query_map([], |row| {
                    Ok(OwnerCount {
                        owner: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        Ok(StatsResult {
            total_scripts: total,
            by_language,
            by_owner,
            revisions: self.revision_stats()?,
        })
    }

    // ------------------------------------------------------------------
    // Index metadata
    // ------------------------------------------------------------------

    /// Return build/index metadata row and current schema version.
    pub fn index_metadata(&self) -> Result<IndexMetadata> {
        let row = self
            .conn
            .query_row(
                "SELECT build_timestamp, schema_version FROM index_metadata WHERE id=1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                    ))
                },
            )
            .optional()?;

        let (ts, sv) = row.unwrap_or((None, None));
        Ok(IndexMetadata {
            build_timestamp: ts.map(Value::String).unwrap_or(Value::Null),
            schema_version: sv.map(|n| Value::Number(n.into())).unwrap_or(Value::Null),
            current_schema_version: SCHEMA_VERSION,
        })
    }

    /// Run selected audit checks and return findings with summary counts.
    pub fn audit(&self, checks: Option<&[String]>, stale_days: i64) -> Result<AuditResult> {
        const ALL_CHECKS: &[&str] = &[
            "unowned",
            "no-purpose",
            "broken-deps",
            "orphan-checkouts",
            "stale-checkouts",
            "dead-scripts",
            "no-description",
        ];

        let selected = checks.map(|values| {
            values
                .iter()
                .map(|v| v.trim().to_string())
                .collect::<std::collections::HashSet<_>>()
        });

        if let Some(set) = &selected {
            for check in set {
                if !ALL_CHECKS.contains(&check.as_str()) {
                    return Err(Error::Validation(format!("unknown audit check: {check}")));
                }
            }
        }

        let stale_seconds = stale_days.max(0) as f64 * 86_400.0;

        let mut findings = Vec::new();

        if should_run(&selected, "unowned") {
            let rows = query_rows(
                &self.conn,
                "SELECT logical_path
                 FROM scripts
                 WHERE COALESCE(TRIM(json_extract(metadata_json, '$.techowner')), '') = ''
                   AND COALESCE(TRIM(json_extract(metadata_json, '$.funcowner')), '') = ''
                 ORDER BY logical_path",
                &[],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "unowned".to_string(),
                severity: "warn".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "no techowner or funcowner".to_string(),
            }));
        }

        if should_run(&selected, "no-purpose") {
            let rows = query_rows(
                &self.conn,
                "SELECT logical_path
                 FROM scripts
                 WHERE COALESCE(TRIM(purpose), '') = ''
                 ORDER BY logical_path",
                &[],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "no-purpose".to_string(),
                severity: "warn".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "purpose/brief is missing".to_string(),
            }));
        }

        if should_run(&selected, "broken-deps") {
            let rows = query_rows(
                &self.conn,
                "SELECT src.logical_path AS logical_path, d.depends_on_path AS dependency
                 FROM dependencies d
                 JOIN scripts src ON src.id = d.script_id
                 WHERE d.resolved_script_id IS NULL
                 ORDER BY src.logical_path, d.depends_on_path",
                &[],
            )?;
            findings.extend(rows.into_iter().map(|row| {
                let dep = row_string(&row, "dependency");
                AuditFinding {
                    check: "broken-deps".to_string(),
                    severity: "error".to_string(),
                    logical_path: row_string(&row, "logical_path"),
                    detail: format!("depends on {dep} (not indexed)"),
                }
            }));
        }

        if should_run(&selected, "orphan-checkouts") {
            let rows = query_rows(
                &self.conn,
                "SELECT r.logical_path
                 FROM revisions r
                 LEFT JOIN scripts s ON s.logical_path = r.logical_path
                 WHERE s.logical_path IS NULL
                   AND r.revision_type = ?1
                 GROUP BY r.logical_path
                 ORDER BY r.logical_path",
                &[&REVISION_TYPE_DEVELOP],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "orphan-checkouts".to_string(),
                severity: "warn".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "vc checkout exists without catalog entry".to_string(),
            }));
        }

        if should_run(&selected, "stale-checkouts") {
            let rows = query_rows(
                &self.conn,
                "SELECT logical_path, MAX(age_seconds) AS checkout_age_seconds
                 FROM revisions
                 WHERE revision_type = ?1
                   AND age_seconds IS NOT NULL
                 GROUP BY logical_path
                 HAVING MAX(age_seconds) >= ?2
                 ORDER BY logical_path",
                &[&REVISION_TYPE_DEVELOP, &stale_seconds],
            )?;
            findings.extend(rows.into_iter().map(|row| {
                let age = row
                    .get("checkout_age_seconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    / 86_400.0;
                AuditFinding {
                    check: "stale-checkouts".to_string(),
                    severity: "info".to_string(),
                    logical_path: row_string(&row, "logical_path"),
                    detail: format!("checkout is stale ({age:.0} days old)"),
                }
            }));
        }

        if should_run(&selected, "dead-scripts") {
            let rows = query_rows(
                &self.conn,
                "SELECT s.logical_path
                 FROM scripts s
                 LEFT JOIN dependencies d ON d.resolved_script_id = s.id
                 LEFT JOIN revisions r ON r.logical_path = s.logical_path
                  AND r.revision_type = ?1
                 LEFT JOIN scripts sym ON sym.symlink_target = s.logical_path
                 GROUP BY s.id
                 HAVING COUNT(DISTINCT d.id) = 0
                    AND COUNT(DISTINCT r.id) = 0
                    AND COUNT(DISTINCT sym.id) = 0
                 ORDER BY s.logical_path",
                &[&REVISION_TYPE_DEVELOP],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "dead-scripts".to_string(),
                severity: "info".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "no dependents, never checked out".to_string(),
            }));
        }

        if should_run(&selected, "no-description") {
            let rows = query_rows(
                &self.conn,
                "SELECT logical_path
                 FROM scripts
                 WHERE COALESCE(TRIM(json_extract(metadata_json, '$.brief')), '') = ''
                   AND COALESCE(TRIM(json_extract(metadata_json, '$.docstring')), '') = ''
                 ORDER BY logical_path",
                &[],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "no-description".to_string(),
                severity: "info".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "missing both docstring and @brief metadata".to_string(),
            }));
        }

        findings.sort_by(|a, b| {
            a.check
                .cmp(&b.check)
                .then_with(|| a.logical_path.cmp(&b.logical_path))
                .then_with(|| a.detail.cmp(&b.detail))
        });

        let summary = findings.iter().fold(AuditSummary::default(), |mut acc, f| {
            match f.severity.as_str() {
                "error" => acc.error += 1,
                "warn" => acc.warn += 1,
                _ => acc.info += 1,
            }
            acc
        });

        Ok(AuditResult { findings, summary })
    }
}

impl SearchApi {
    fn revision_stats(&self) -> Result<Option<RevisionStats>> {
        let has_revisions_table: bool = self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM sqlite_master
                 WHERE type = 'table' AND name = 'revisions'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_revisions_table {
            return Ok(None);
        }

        let total_revision_rows: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM revisions", [], |row| row.get(0))?;
        if total_revision_rows == 0 {
            return Ok(None);
        }

        let (
            scripts_with_active_checkouts,
            scripts_with_archive_entries,
            scripts_with_working_versions,
            total_develop_revision_files,
            total_archive_revision_files,
            total_working_revision_files,
        ): (i64, i64, i64, i64, i64, i64) = self.conn.query_row(
            "SELECT
                 COUNT(DISTINCT CASE WHEN revision_type = ?1 THEN logical_path END),
                 COUNT(DISTINCT CASE WHEN revision_type = ?2 THEN logical_path END),
                 COUNT(DISTINCT CASE WHEN revision_type = ?3 THEN logical_path END),
                 COUNT(CASE WHEN revision_type = ?1 THEN 1 END),
                 COUNT(CASE WHEN revision_type = ?2 THEN 1 END),
                 COUNT(CASE WHEN revision_type = ?3 THEN 1 END)
             FROM revisions",
            [
                REVISION_TYPE_DEVELOP,
                REVISION_TYPE_ARCHIVE,
                REVISION_TYPE_WORKING,
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        let scripts_checked_out_by_multiple_users: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM (
                 SELECT logical_path
                 FROM revisions
                 WHERE revision_type = ?1
                 GROUP BY logical_path
                 HAVING COUNT(DISTINCT user) > 1
             )",
            [REVISION_TYPE_DEVELOP],
            |row| row.get(0),
        )?;

        Ok(Some(RevisionStats {
            scripts_with_active_checkouts,
            scripts_with_archive_entries,
            total_develop_revision_files,
            total_archive_revision_files,
            scripts_with_working_versions,
            total_working_revision_files,
            scripts_checked_out_by_multiple_users,
        }))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn should_run(selected: &Option<std::collections::HashSet<String>>, check: &str) -> bool {
    selected
        .as_ref()
        .is_none_or(|checks| checks.contains(check))
}

/// Open a read-only database and construct a [`SearchApi`].
pub fn open_search_api(path: &std::path::Path) -> Result<SearchApi> {
    let conn = crate::core::db::open_db(path)?;
    Ok(SearchApi::new(conn))
}

/// Compare two complete catalog database snapshots.
pub fn compare_catalogs(old_db_path: &Path, new_db_path: &Path) -> Result<CatalogDiff> {
    let old_conn = crate::core::db::open_db(old_db_path)?;
    let new_conn = crate::core::db::open_db(new_db_path)?;

    let old_scripts = scripts_for_diff(&old_conn)?;
    let new_scripts = scripts_for_diff(&new_conn)?;
    let old_deps = dependencies_for_diff(&old_conn)?;
    let new_deps = dependencies_for_diff(&new_conn)?;

    let old_paths: BTreeSet<&String> = old_scripts.keys().collect();
    let new_paths: BTreeSet<&String> = new_scripts.keys().collect();

    let added = new_paths
        .difference(&old_paths)
        .map(|path| script_diff_row(&new_scripts[*path]))
        .collect();
    let removed = old_paths
        .difference(&new_paths)
        .map(|path| script_diff_row(&old_scripts[*path]))
        .collect();

    let mut changed = Vec::new();
    for path in old_paths.intersection(&new_paths) {
        let old_script = &old_scripts[*path];
        let new_script = &new_scripts[*path];
        let mut fields = BTreeMap::new();
        for field in DIFF_FIELDS {
            let old_value = old_script
                .fields
                .get(*field)
                .cloned()
                .unwrap_or(Value::Null);
            let new_value = new_script
                .fields
                .get(*field)
                .cloned()
                .unwrap_or(Value::Null);
            if old_value != new_value {
                fields.insert((*field).to_string(), [old_value, new_value]);
            }
        }

        let empty = BTreeSet::new();
        let old_script_deps = old_deps.get(*path).unwrap_or(&empty);
        let new_script_deps = new_deps.get(*path).unwrap_or(&empty);
        let deps_added = new_script_deps
            .difference(old_script_deps)
            .cloned()
            .collect::<Vec<_>>();
        let deps_removed = old_script_deps
            .difference(new_script_deps)
            .cloned()
            .collect::<Vec<_>>();

        if !fields.is_empty() || !deps_added.is_empty() || !deps_removed.is_empty() {
            changed.push(CatalogDiffChange {
                logical_path: (*path).clone(),
                fields,
                deps_added,
                deps_removed,
            });
        }
    }

    Ok(CatalogDiff {
        added,
        removed,
        changed,
    })
}

const DIFF_FIELDS: &[&str] = &[
    "purpose",
    "techowner",
    "funcowner",
    "owner",
    "language",
    "tags",
    "entry_points",
    "related",
];

struct DiffScript {
    logical_path: String,
    fields: BTreeMap<String, Value>,
}

fn script_diff_row(script: &DiffScript) -> CatalogDiffScript {
    CatalogDiffScript {
        logical_path: script.logical_path.clone(),
        language: script
            .fields
            .get("language")
            .cloned()
            .unwrap_or(Value::Null),
        owner: script.fields.get("owner").cloned().unwrap_or(Value::Null),
    }
}

fn scripts_for_diff(conn: &Connection) -> Result<BTreeMap<String, DiffScript>> {
    let rows = query_rows(
        conn,
        "SELECT logical_path, purpose, owner, language, tags, entry_points, related, metadata_json
         FROM scripts
         ORDER BY logical_path",
        &[],
    )?;
    let mut scripts = BTreeMap::new();
    for row in rows {
        let view = ScriptView::new(&row);
        let logical_path = view.logical_path().to_string();
        let metadata = view.metadata();

        let mut fields = BTreeMap::new();
        fields.insert(
            "purpose".to_string(),
            view.purpose_value().cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "techowner".to_string(),
            metadata.get("techowner").cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "funcowner".to_string(),
            metadata.get("funcowner").cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "owner".to_string(),
            view.owner_value().cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "language".to_string(),
            view.language_value().cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "tags".to_string(),
            view.list_value_or_empty(ListField::Tags),
        );
        fields.insert(
            "entry_points".to_string(),
            view.list_value_or_empty(ListField::EntryPoints),
        );
        fields.insert(
            "related".to_string(),
            view.list_value_or_empty(ListField::Related),
        );

        scripts.insert(
            logical_path.clone(),
            DiffScript {
                logical_path,
                fields,
            },
        );
    }
    Ok(scripts)
}

fn dependencies_for_diff(conn: &Connection) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let rows = query_rows(
        conn,
        "SELECT s.logical_path, d.depends_on_path
         FROM dependencies d
         JOIN scripts s ON s.id = d.script_id
         ORDER BY s.logical_path, d.depends_on_path",
        &[],
    )?;
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        deps.entry(row_string(&row, "logical_path"))
            .or_default()
            .insert(row_string(&row, "depends_on_path"));
    }
    Ok(deps)
}
