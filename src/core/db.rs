use std::path::Path;

use rusqlite::{Connection, OpenFlags, params_from_iter, types::Value as SqlValue};
use tracing::{debug, trace};

use crate::error::{Error, Result};

/// Current SQLite schema version expected by this binary.
pub const SCHEMA_VERSION: i64 = 9;

/// A database row serialised as a JSON object — every column becomes a key.
pub type JsonRow = serde_json::Map<String, serde_json::Value>;

/// Return a string column value from a [`JsonRow`], or `""` when the field is
/// missing, null, or not stored as a JSON string.
pub fn row_str<'a>(row: &'a JsonRow, key: &str) -> &'a str {
    row.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Return a string column value from a [`JsonRow`] as an owned [`String`].
///
/// Missing, null, and non-string fields become an empty string.
pub fn row_string(row: &JsonRow, key: &str) -> String {
    row_str(row, key).to_string()
}

/// Return a display-oriented field value from a [`JsonRow`].
///
/// Non-empty strings are returned as-is. Numbers and booleans are stringified.
/// Missing, null, empty string, and other value types use `fallback`.
pub fn row_display(row: &JsonRow, key: &str, fallback: &str) -> String {
    match row.get(key) {
        Some(serde_json::Value::String(value)) if !value.is_empty() => value.clone(),
        Some(serde_json::Value::Number(value)) => value.to_string(),
        Some(serde_json::Value::Bool(value)) => value.to_string(),
        _ => fallback.to_string(),
    }
}

/// Append optional script metadata filters to a SQL `WHERE` clause.
///
/// `column_prefix` qualifies `language`, `owner`, and `tags` when the query
/// uses a table alias, for example `Some("s.")` for `s.language`.
pub fn append_script_filters(
    sql: &mut String,
    params: &mut Vec<SqlValue>,
    column_prefix: Option<&str>,
    language: Option<&str>,
    owner: Option<&str>,
    tag: Option<&str>,
) {
    let prefix = column_prefix.unwrap_or("");
    let language_col = format!("{prefix}language");
    let owner_col = format!("{prefix}owner");
    let tags_col = format!("{prefix}tags");

    if let Some(lang) = language {
        sql.push_str(&format!(" AND LOWER({language_col}) = LOWER(?)"));
        params.push(SqlValue::Text(lang.to_string()));
    }
    if let Some(own) = owner {
        sql.push_str(&format!(" AND INSTR(LOWER({owner_col}), LOWER(?)) > 0"));
        params.push(SqlValue::Text(own.to_string()));
    }
    if let Some(t) = tag {
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM json_each({tags_col}) AS je WHERE LOWER(je.value) = LOWER(?))"
        ));
        params.push(SqlValue::Text(t.to_string()));
    }
}

// ---------------------------------------------------------------------------
// DDL (see SCHEMA_VERSION)
// ---------------------------------------------------------------------------

const DDL: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS scripts (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    logical_path         TEXT    NOT NULL UNIQUE,
    language             TEXT,
    size                 INTEGER,
    mtime                REAL,
    content              TEXT,
    owner                TEXT,
    purpose              TEXT,
    tags                 TEXT,
    entry_points         TEXT,
    related              TEXT,
    symlink_target       TEXT,
    metadata_json        TEXT,
    checkout_user        TEXT,
    checkout_timestamp   TEXT,
    checkout_os          TEXT,
    checkout_age_seconds REAL,
    vc_warnings          TEXT,
    indexed_at           TEXT
);

CREATE INDEX IF NOT EXISTS idx_scripts_symlink_target
ON scripts(symlink_target)
WHERE symlink_target IS NOT NULL;

CREATE VIRTUAL TABLE IF NOT EXISTS script_fts USING fts5(
    logical_path,
    content,
    owner,
    purpose,
    tags,
    content='scripts',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS scripts_ai AFTER INSERT ON scripts BEGIN
    INSERT INTO script_fts(rowid, logical_path, content, owner, purpose, tags)
    VALUES (new.id, new.logical_path, new.content, new.owner, new.purpose, new.tags);
END;

CREATE TRIGGER IF NOT EXISTS scripts_ad AFTER DELETE ON scripts BEGIN
    INSERT INTO script_fts(script_fts, rowid, logical_path, content, owner, purpose, tags)
    VALUES ('delete', old.id, old.logical_path, old.content, old.owner, old.purpose, old.tags);
END;

CREATE TRIGGER IF NOT EXISTS scripts_au AFTER UPDATE ON scripts BEGIN
    INSERT INTO script_fts(script_fts, rowid, logical_path, content, owner, purpose, tags)
    VALUES ('delete', old.id, old.logical_path, old.content, old.owner, old.purpose, old.tags);
    INSERT INTO script_fts(rowid, logical_path, content, owner, purpose, tags)
    VALUES (new.id, new.logical_path, new.content, new.owner, new.purpose, new.tags);
END;

CREATE TABLE IF NOT EXISTS dependencies (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    script_id          INTEGER NOT NULL REFERENCES scripts(id) ON DELETE CASCADE,
    depends_on_path    TEXT    NOT NULL,
    resolved_script_id INTEGER REFERENCES scripts(id) ON DELETE SET NULL,
    UNIQUE (script_id, depends_on_path)
);

CREATE INDEX IF NOT EXISTS idx_dependencies_resolved_script_id
ON dependencies(resolved_script_id);

-- Derived compatibility summary maintained by the indexer. User-facing
-- revision reads should prefer the first-class revisions table below.
CREATE TABLE IF NOT EXISTS vc_checkouts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    logical_path  TEXT NOT NULL,
    physical_path TEXT NOT NULL,
    os_flavor     TEXT NOT NULL,
    user          TEXT NOT NULL,
    timestamp     TEXT NOT NULL,
    age_seconds   REAL,
    UNIQUE (physical_path)
);

CREATE INDEX IF NOT EXISTS idx_vc_checkouts_logical_path
ON vc_checkouts(logical_path);

CREATE TABLE IF NOT EXISTS revisions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    logical_path  TEXT NOT NULL,
    physical_path TEXT NOT NULL,
    revision_type TEXT NOT NULL,
    os_flavor     TEXT NOT NULL,
    user          TEXT NOT NULL,
    timestamp     TEXT NOT NULL,
    age_seconds   REAL,
    UNIQUE (physical_path)
);

CREATE INDEX IF NOT EXISTS idx_revisions_logical_path
ON revisions(logical_path);

CREATE INDEX IF NOT EXISTS idx_revisions_type_logical_path
ON revisions(revision_type, logical_path);

CREATE TABLE IF NOT EXISTS function_definitions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    script_id   INTEGER NOT NULL REFERENCES scripts(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    kind        TEXT    NOT NULL,
    line        INTEGER NOT NULL,
    docstring   TEXT,
    decorators  TEXT,
    UNIQUE (script_id, kind, name, line)
);

CREATE INDEX IF NOT EXISTS idx_function_definitions_script_id
ON function_definitions(script_id);

CREATE INDEX IF NOT EXISTS idx_function_definitions_name
ON function_definitions(name);

CREATE TABLE IF NOT EXISTS function_calls (
    id                        INTEGER PRIMARY KEY AUTOINCREMENT,
    script_id                 INTEGER NOT NULL REFERENCES scripts(id) ON DELETE CASCADE,
    caller                    TEXT    NOT NULL,
    callee                    TEXT    NOT NULL,
    line                      INTEGER NOT NULL,
    resolved_target_name      TEXT,
    resolved_target_script_id INTEGER REFERENCES scripts(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_function_calls_script_id
ON function_calls(script_id);

CREATE INDEX IF NOT EXISTS idx_function_calls_resolved_target_script_id
ON function_calls(resolved_target_script_id);

CREATE INDEX IF NOT EXISTS idx_function_calls_callee
ON function_calls(callee);

CREATE INDEX IF NOT EXISTS idx_function_calls_resolved_target_name
ON function_calls(resolved_target_name);

CREATE TABLE IF NOT EXISTS index_metadata (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    build_timestamp TEXT    NOT NULL,
    schema_version  INTEGER NOT NULL
);
"#;

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

/// Open an existing database at `path` in **read-only** mode.
pub fn open_db(path: &Path) -> Result<Connection> {
    if !path.exists() {
        return Err(Error::NotFound(path.display().to_string()));
    }
    debug!(path = %path.display(), "opening read-only database");
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    let schema_version: i64 = conn
        .query_row(
            "SELECT schema_version FROM index_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| {
            Error::Validation(format!(
                "database has no index_metadata ({e}) — re-index required (scat catalog build)"
            ))
        })?;
    if schema_version != SCHEMA_VERSION {
        return Err(Error::SchemaMismatch {
            found: schema_version,
            expected: SCHEMA_VERSION,
        });
    }
    Ok(conn)
}

/// Create (or open) a writable database at `path` and apply the schema.
pub fn create_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    // journal_mode = WAL returns a row; must be consumed, not execute_batch'd.
    let wal_mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    conn.execute_batch(DDL)?;
    debug!(
        path = %path.display(),
        schema_version = SCHEMA_VERSION,
        wal_mode = %wal_mode,
        "initialized database"
    );
    Ok(conn)
}

/// Run a full-text search against `script_fts` and return BM25-ranked rows.
///
/// Mirrors `core/db.py::fts_query` exactly.
pub fn fts_query(
    conn: &Connection,
    query: &str,
    limit: usize,
    language: Option<&str>,
) -> Result<Vec<JsonRow>> {
    fts_query_filtered(conn, query, limit, language, None, None)
}

/// Run a full-text search with optional language, owner, and tag filters.
///
/// Filters are applied in SQLite before `LIMIT` so callers receive up to
/// `limit` rows that match all requested criteria.
pub fn fts_query_filtered(
    conn: &Connection,
    query: &str,
    limit: usize,
    language: Option<&str>,
    owner: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<JsonRow>> {
    debug!(
        query = %query,
        limit,
        language = ?language,
        owner = ?owner,
        tag = ?tag,
        "executing FTS query"
    );

    let mut sql = String::from(
        "
            SELECT s.*
            FROM script_fts f
            JOIN scripts s ON s.id = f.rowid
            WHERE script_fts MATCH ?
        ",
    );
    let lim = limit as i64;
    let mut params = vec![SqlValue::Text(query.to_string())];

    append_script_filters(&mut sql, &mut params, Some("s."), language, owner, tag);
    sql.push_str(" ORDER BY bm25(script_fts) LIMIT ?");
    params.push(SqlValue::Integer(lim));

    let mut stmt = conn.prepare(&sql)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    stmt.query_map(params_from_iter(params), |row| Ok(row_to_map(row, &cols)))?
        .map(|r| r.map_err(Error::from))
        .collect()
}

// ---------------------------------------------------------------------------
// Row conversion
// ---------------------------------------------------------------------------

/// Convert a rusqlite `Row` into a JSON object, preserving column order.
/// NULL values become `null`, integers stay integers, reals stay numbers.
/// This produces the same output as Python's `dict(row)` + `json.dumps`.
pub fn row_to_map(row: &rusqlite::Row, col_names: &[String]) -> JsonRow {
    use rusqlite::types::ValueRef;
    let mut map = serde_json::Map::new();
    for (i, col) in col_names.iter().enumerate() {
        let val = match row.get_ref(i).unwrap_or(ValueRef::Null) {
            ValueRef::Null => serde_json::Value::Null,
            ValueRef::Integer(n) => serde_json::Value::Number(n.into()),
            ValueRef::Real(f) => serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            ValueRef::Text(s) => serde_json::Value::String(String::from_utf8_lossy(s).into_owned()),
            ValueRef::Blob(_) => serde_json::Value::Null,
        };
        map.insert(col.clone(), val);
    }
    trace!(row = ?map, "serialized database row");
    map
}

/// Execute a SQL statement and collect all rows as JSON objects.
pub fn query_rows(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<JsonRow>> {
    let mut stmt = conn.prepare(sql)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows: Result<Vec<JsonRow>> = stmt
        .query_map(params, |row| Ok(row_to_map(row, &cols)))?
        .map(|r| r.map_err(Error::from))
        .collect();
    rows
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn row_str_returns_empty_for_missing_null_and_non_string_values() {
        let mut row = JsonRow::new();
        row.insert("null".into(), serde_json::Value::Null);
        row.insert("number".into(), json!(42));
        row.insert("string".into(), json!("value"));

        assert_eq!(row_str(&row, "missing"), "");
        assert_eq!(row_str(&row, "null"), "");
        assert_eq!(row_str(&row, "number"), "");
        assert_eq!(row_str(&row, "string"), "value");
    }

    #[test]
    fn row_string_owns_raw_string_value() {
        let mut row = JsonRow::new();
        row.insert("string".into(), json!("value"));

        assert_eq!(row_string(&row, "string"), "value");
        assert_eq!(row_string(&row, "missing"), "");
    }

    #[test]
    fn row_display_stringifies_scalars_and_uses_fallback() {
        let mut row = JsonRow::new();
        row.insert("string".into(), json!("value"));
        row.insert("empty".into(), json!(""));
        row.insert("number".into(), json!(42));
        row.insert("bool".into(), json!(true));
        row.insert("array".into(), json!(["x"]));

        assert_eq!(row_display(&row, "string", "-"), "value");
        assert_eq!(row_display(&row, "empty", "-"), "-");
        assert_eq!(row_display(&row, "number", "-"), "42");
        assert_eq!(row_display(&row, "bool", "-"), "true");
        assert_eq!(row_display(&row, "array", "-"), "-");
        assert_eq!(row_display(&row, "missing", "-"), "-");
    }

    #[test]
    fn append_script_filters_uses_unqualified_columns_and_parameter_order() {
        let mut sql = String::from("SELECT * FROM scripts WHERE 1=1");
        let mut params = Vec::new();

        append_script_filters(
            &mut sql,
            &mut params,
            None,
            Some("python"),
            Some("alice"),
            Some("deploy"),
        );

        assert!(sql.contains("LOWER(language) = LOWER(?)"));
        assert!(sql.contains("INSTR(LOWER(owner), LOWER(?)) > 0"));
        assert!(sql.contains("json_each(tags)"));
        assert_eq!(
            params,
            vec![
                SqlValue::Text("python".to_string()),
                SqlValue::Text("alice".to_string()),
                SqlValue::Text("deploy".to_string()),
            ]
        );
    }

    #[test]
    fn append_script_filters_uses_qualified_columns() {
        let mut sql = String::from("SELECT s.* FROM scripts s WHERE 1=1");
        let mut params = Vec::new();

        append_script_filters(
            &mut sql,
            &mut params,
            Some("s."),
            Some("python"),
            Some("alice"),
            Some("deploy"),
        );

        assert!(sql.contains("LOWER(s.language) = LOWER(?)"));
        assert!(sql.contains("INSTR(LOWER(s.owner), LOWER(?)) > 0"));
        assert!(sql.contains("json_each(s.tags)"));
        assert_eq!(
            params,
            vec![
                SqlValue::Text("python".to_string()),
                SqlValue::Text("alice".to_string()),
                SqlValue::Text("deploy".to_string()),
            ]
        );
    }
}
