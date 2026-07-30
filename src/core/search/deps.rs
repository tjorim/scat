use rusqlite::{OptionalExtension, types::Value as SqlValue};
use serde_json::Value;
use std::collections::BTreeSet;

use crate::core::db::{JsonRow, append_script_filters, query_rows, row_string, row_to_map};
use crate::error::{Error, Result};

use super::{
    DependencyEntry, DependencyGraph, DepsTreeNode, SearchApi, TreeDirection, query_script_rows,
};

impl SearchApi {
    // ------------------------------------------------------------------
    // Related scripts
    // ------------------------------------------------------------------

    /// Return related scripts from explicit relations and dependency edges.
    pub fn related_scripts(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        const SQLITE_PARAM_LIMIT: usize = 999;

        let script = match self.get_script(logical_path)? {
            Some(s) => s,
            None => return Ok(vec![]),
        };

        let mut related: BTreeSet<String> = BTreeSet::new();

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
            let cols: Vec<String> = stmt
                .column_names()
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
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
}
