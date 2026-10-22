use regex::Regex;
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};

use crate::core::db::{
    JsonRow, append_script_filters, fts_query_filtered, query_rows, row_string,
    validate_search_limit,
};
use crate::core::script_view::logical_parent_dir;
use crate::error::{Error, Result};

use super::{SearchApi, query_script_rows};

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
        let lim = validate_search_limit(limit)?;
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
        validate_search_limit(limit)?;
        let re = Regex::new(pattern)
            .map_err(|e| Error::Validation(format!("invalid regex pattern {pattern:?}: {e}")))?;

        let mut sql = String::from("SELECT id, logical_path, purpose FROM scripts WHERE 1=1");
        let mut params = Vec::new();
        append_script_filters(&mut sql, &mut params, None, language, owner, tag);
        sql.push_str(" ORDER BY logical_path");

        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut rows = stmt.query(params_from_iter(params))?;

        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            // Inspect borrowed text first: rejected rows must not copy script content
            // or serialize every column into JSON.
            let path = row.get_ref("logical_path")?.as_str().unwrap_or("");
            let purpose = row.get_ref("purpose")?.as_str().unwrap_or("");
            if re.is_match(path) || re.is_match(purpose) {
                results.extend(query_script_rows(
                    &self.conn,
                    "SELECT * FROM scripts WHERE id = ?",
                    vec![SqlValue::Integer(row.get("id")?)],
                )?);
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
        let lim = validate_search_limit(limit)?;
        let off = i64::try_from(offset)
            .map_err(|_| Error::Validation("Offset exceeds SQLite range".into()))?;
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
        let prefix = if dir == "/" {
            dir.to_owned()
        } else {
            format!("{dir}/")
        };
        query_script_rows(
            &self.conn,
            "SELECT * FROM scripts
             WHERE substr(logical_path, 1, length(?1)) = ?1 COLLATE BINARY
               AND instr(substr(logical_path, length(?1) + 1), '/') = 0
             ORDER BY logical_path",
            vec![SqlValue::Text(prefix)],
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
        let prefix = if dir == "/" {
            dir.to_owned()
        } else {
            format!("{dir}/")
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT substr(substr(logical_path, length(?1) + 1), 1,
                                    instr(substr(logical_path, length(?1) + 1), '/') - 1) AS name
             FROM scripts
             WHERE substr(logical_path, 1, length(?1)) = ?1 COLLATE BINARY
               AND instr(substr(logical_path, length(?1) + 1), '/') > 0
             ORDER BY name",
        )?;
        let rows: Result<Vec<String>> = stmt
            .query_map(rusqlite::params![prefix], |row| row.get(0))?
            .map(|r| r.map_err(Error::from))
            .collect();
        rows
    }
}
