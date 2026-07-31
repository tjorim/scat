use std::path::Path;

use rusqlite::{Connection, OpenFlags, params_from_iter, types::Value as SqlValue};
use tracing::{debug, trace};

use crate::error::{Error, Result};

/// Current SQLite schema version expected by this binary.
pub const SCHEMA_VERSION: i64 = 13;

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

pub(crate) const DDL: &str = r"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS scripts (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    logical_path         TEXT    NOT NULL UNIQUE,
    language             TEXT,
    size                 INTEGER,
    mtime                REAL,
    content_hash         TEXT,
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

CREATE INDEX IF NOT EXISTS idx_scripts_language
ON scripts(language)
WHERE language IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_scripts_owner
ON scripts(owner)
WHERE owner IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_scripts_logical_path
ON scripts(logical_path);

CREATE INDEX IF NOT EXISTS idx_scripts_checkout_timestamp
ON scripts(checkout_timestamp)
WHERE checkout_timestamp IS NOT NULL;

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
    -- 'import': a language-level import/source edge (module name in
    -- depends_on_path). 'referenced': a path-literal edge (a full logical
    -- path found in the script body, e.g. a manifest entry or an
    -- `ssh host python3 <path>` invocation), resolved by exact path match.
    kind               TEXT    NOT NULL DEFAULT 'import',
    UNIQUE (script_id, depends_on_path, kind)
);

CREATE INDEX IF NOT EXISTS idx_dependencies_resolved_script_id
ON dependencies(resolved_script_id);

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
";

// ---------------------------------------------------------------------------
// Bulk-build FTS trigger management
// ---------------------------------------------------------------------------
//
// The FTS-sync triggers below mirror `scripts_ai`/`scripts_ad`/`scripts_au`
// in `DDL` above and must be kept in sync with them. They exist as a
// separate pair of statements (rather than being derived from `DDL`) so a
// bulk build can drop them for the duration of the insert-heavy phase —
// firing one FTS write per row during a full catalog rebuild is far slower
// than a single bulk `INSERT INTO script_fts(script_fts) VALUES('rebuild')`
// once all rows are in place — and restore them (verbatim) afterwards so the
// live, swapped-in database keeps incremental FTS maintenance.

/// Drops the FTS-sync triggers. Safe to run even if they're already absent.
pub const DROP_FTS_TRIGGERS_SQL: &str = "
DROP TRIGGER IF EXISTS scripts_ai;
DROP TRIGGER IF EXISTS scripts_ad;
DROP TRIGGER IF EXISTS scripts_au;
";

/// Bulk-rebuilds the FTS index from current `scripts` content, then restores
/// the triggers dropped by [`DROP_FTS_TRIGGERS_SQL`].
pub const REBUILD_FTS_AND_RESTORE_TRIGGERS_SQL: &str = "
INSERT INTO script_fts(script_fts) VALUES('rebuild');

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
";

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
    // `temp_store = MEMORY` keeps temp b-trees (e.g. the `ORDER BY
    // bm25(...)` sort in `fts_query_filtered`) off disk; it's a
    // per-connection setting SQLite doesn't persist in the database file, so
    // every connection — including this read-only one — must set it itself.
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY;")?;
    // SearchApi re-issues the same handful of query shapes (search, list,
    // stats, deps) many times over a connection's life — e.g. once per
    // keystroke in the TUI's live search. `prepare_cached` (used throughout
    // `core::search`) skips re-parsing and re-planning those on repeat calls;
    // raise the cache above rusqlite's default of 16 so the filter
    // combinations (language/owner/tag) across all query shapes still fit.
    conn.set_prepared_statement_cache_capacity(64);
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
///
/// The database is created in WAL mode, which suits the only thing that ever
/// writes one: the indexer's insert-heavy build into a local WIP file. It is
/// *not* the mode the catalog is published in — WAL needs an `mmap`-able
/// `-shm` file even for read-only connections, which a network share
/// generally can't provide — so `indexer::builder` switches the finished
/// build back to a rollback journal before swapping it into place.
pub fn create_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    // journal_mode = WAL returns a row; must be consumed, not execute_batch'd.
    let wal_mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    // `synchronous = NORMAL` is the recommended pairing with WAL: SQLite
    // skips the fsync on every commit (safe under WAL, since a crash can
    // only roll back the last transaction rather than corrupt the database)
    // while still fsyncing at checkpoints. `temp_store = MEMORY` keeps temp
    // b-trees off disk. Both are per-connection settings that SQLite does
    // not persist in the database file, so every connection sets them; a
    // bulk-build connection later relaxes `synchronous` further via
    // `apply_bulk_build_pragmas`.
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY;")?;
    conn.execute_batch(DDL)?;
    debug!(
        path = %path.display(),
        schema_version = SCHEMA_VERSION,
        wal_mode = %wal_mode,
        "initialized database"
    );
    Ok(conn)
}

/// Returns the `schema_version` stored in an existing database's
/// `index_metadata` row, or `None` if `path` doesn't exist, isn't a valid
/// SQLite database, or has no metadata row yet.
///
/// Used to decide whether a previous completed build is eligible to seed an
/// incremental rebuild (see `indexer::builder::build_index`) — a schema
/// mismatch means the stored rows may not match the current column set, so
/// callers should treat any outcome other than `Some(SCHEMA_VERSION)` as
/// ineligible and fall back to a full rebuild.
pub fn schema_version_of(path: &Path) -> Option<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.query_row(
        "SELECT schema_version FROM index_metadata WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .ok()
}

/// Relax durability and grow caches for a connection used to build a
/// throwaway index database.
///
/// The build always writes into a temp/WIP file that is `PRAGMA
/// integrity_check`-validated before being atomically swapped in as the
/// live database (see `indexer::builder::build_index`); if the process
/// dies mid-build, the WIP file is either resumed from its checkpoint or
/// discarded and rebuilt from scratch, and the previously-swapped-in live
/// database is untouched either way. That makes `synchronous = OFF` free
/// speed here — there is no live data whose durability this could
/// compromise — unlike on a connection to a database other code writes to
/// directly.
///
/// `synchronous`, `cache_size`, and `temp_store` are per-connection
/// settings that SQLite does not persist in the database file, so this must
/// be called on every connection used for a build (both a freshly created
/// one and one reopened to resume a checkpointed build), not just once at
/// schema-creation time.
pub fn apply_bulk_build_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        PRAGMA synchronous = OFF;
        PRAGMA temp_store = MEMORY;
        PRAGMA cache_size = -65536;
        ",
    )?;
    conn.set_prepared_statement_cache_capacity(64);
    Ok(())
}

/// Rewrite a free-text search query into an FTS5 MATCH expression that can
/// never produce a syntax error, regardless of what the user typed.
///
/// FTS5's query grammar treats `"` (phrase), `-`/`:`/`(`/`)`/`*`/`^` and the
/// barewords `AND`/`OR`/`NOT`/`NEAR` as operators. Passed straight through to
/// `MATCH`, an unbalanced quote, a hyphenated word, or a reference to a
/// nonexistent column name (`foo:bar`) all raise a hard error — for example
/// a plain search for `nightly-backup` (with no `.` or `/` to route it to
/// path search instead) would otherwise fail outright.
///
/// Every term — whether or not the user quoted it themselves — is
/// re-emitted here as an explicit, escaped phrase, so none of that syntax is
/// ever interpreted as an operator. One consequence: the AND/OR/NOT/NEAR
/// boolean query language becomes unreachable from free-text search, which
/// is the right tradeoff for a "search for this script" tool rather than a
/// query console — a user typing `helm and kubectl` almost certainly wants
/// scripts mentioning both words literally, not a boolean filter. A
/// trailing `*` — bare (`foo*`) or on a user-quoted phrase (`"foo bar"*`) —
/// is preserved as FTS5's prefix-match operator, since `MATCH '"foo"*'` is
/// valid syntax and prefix search is worth keeping. Terms separated by
/// whitespace combine with FTS5's implicit AND, same as the original
/// unsanitized query would have.
///
/// Returns an empty string when there are no real search terms left (the
/// input was empty, whitespace-only, or e.g. a lone unterminated `"`) —
/// `MATCH ''` is itself a syntax error, so callers must check for this and
/// skip the FTS query entirely rather than pass an empty string through.
pub fn sanitize_fts_query(input: &str) -> String {
    // NUL bytes can't occur in practice (shell argv and JSON strings can't
    // carry one), but FTS5's phrase parser reads a quoted term as a C string
    // and stops at the first NUL, turning it into an "unterminated string"
    // syntax error even though every `"` was escaped correctly. Stripping
    // them keeps the "no syntax error, ever" contract exact rather than
    // "almost always".
    let chars: Vec<char> = input.chars().filter(|&c| c != '\0').collect();
    let n = chars.len();
    let mut i = 0;
    let mut terms: Vec<String> = Vec::new();

    while i < n {
        while i < n && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= n {
            break;
        }

        let (content, prefix): (String, bool) = if chars[i] == '"' {
            i += 1;
            let start = i;
            while i < n && chars[i] != '"' {
                i += 1;
            }
            let content: String = chars[start..i].iter().collect();
            if i < n {
                i += 1; // consume the closing quote
            }
            let prefix = i < n && chars[i] == '*';
            if prefix {
                i += 1;
            }
            (content, prefix)
        } else {
            let start = i;
            while i < n && !chars[i].is_whitespace() && chars[i] != '"' {
                i += 1;
            }
            let word = &chars[start..i];
            if word.len() > 1 && word.last() == Some(&'*') {
                (word[..word.len() - 1].iter().collect(), true)
            } else {
                (word.iter().collect(), false)
            }
        };

        if content.trim().is_empty() {
            continue;
        }

        let escaped = content.replace('"', "\"\"");
        let mut term = format!("\"{escaped}\"");
        if prefix {
            term.push('*');
        }
        terms.push(term);
    }

    terms.join(" ")
}

/// Run a full-text search with optional language, owner, and tag filters.
///
/// `query` is rewritten through [`sanitize_fts_query`] before being matched,
/// so it can contain any text — including FTS5 operator characters — without
/// risk of a syntax error.
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
    let sanitized = sanitize_fts_query(query);
    if sanitized.is_empty() {
        // No real search terms after sanitizing — `MATCH ''` is itself a
        // syntax error, and there's nothing meaningful to search for anyway.
        return Ok(vec![]);
    }

    debug!(
        query = %query,
        sanitized = %sanitized,
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
    let mut params = vec![SqlValue::Text(sanitized)];

    append_script_filters(&mut sql, &mut params, Some("s."), language, owner, tag);
    sql.push_str(" ORDER BY bm25(script_fts) LIMIT ?");
    params.push(SqlValue::Integer(lim));

    let mut stmt = conn.prepare_cached(&sql)?;
    let cols: Vec<String> = stmt
        .column_names()
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
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
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
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
    let mut stmt = conn.prepare_cached(sql)?;
    let cols: Vec<String> = stmt
        .column_names()
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    let rows: Result<Vec<JsonRow>> = stmt
        .query_map(params, |row| Ok(row_to_map(row, &cols)))?
        .map(|r| r.map_err(Error::from))
        .collect();
    rows
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
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

    // -----------------------------------------------------------------------
    // sanitize_fts_query
    // -----------------------------------------------------------------------

    #[test]
    fn sanitize_quotes_plain_words_preserving_implicit_and() {
        assert_eq!(sanitize_fts_query("foo"), "\"foo\"");
        assert_eq!(sanitize_fts_query("foo bar"), "\"foo\" \"bar\"");
        assert_eq!(sanitize_fts_query("  foo   bar  "), "\"foo\" \"bar\"");
    }

    #[test]
    fn sanitize_neutralises_operator_characters() {
        // Hyphen: FTS5 treats it as a column-exclusion operator and errors
        // ("no such column: bar") on `foo-bar` unquoted.
        assert_eq!(sanitize_fts_query("nightly-backup"), "\"nightly-backup\"");
        // Colon: a valid-looking-but-wrong column filter errors too.
        assert_eq!(sanitize_fts_query("owner:bob"), "\"owner:bob\"");
        // Parens and boolean keywords are neutralised to literal text.
        assert_eq!(sanitize_fts_query("(foo)"), "\"(foo)\"");
        assert_eq!(sanitize_fts_query("AND"), "\"AND\"");
        assert_eq!(sanitize_fts_query("foo AND bar"), "\"foo\" \"AND\" \"bar\"");
    }

    #[test]
    fn sanitize_preserves_prefix_search() {
        assert_eq!(sanitize_fts_query("check*"), "\"check\"*");
        assert_eq!(sanitize_fts_query("\"check mc\"*"), "\"check mc\"*");
        // A bare lone `*` has nothing to strip a prefix from; keep it as a
        // harmless literal rather than emitting an empty term.
        assert_eq!(sanitize_fts_query("*"), "\"*\"");
        // Leading `*` is not an FTS5 operator position and is treated as
        // ordinary (if unmatchable-as-a-prefix) literal text, not an error.
        assert_eq!(sanitize_fts_query("*foo"), "\"*foo\"");
    }

    #[test]
    fn sanitize_preserves_user_supplied_phrases() {
        assert_eq!(sanitize_fts_query("\"foo bar\""), "\"foo bar\"");
        assert_eq!(sanitize_fts_query("\"foo bar\" baz"), "\"foo bar\" \"baz\"");
    }

    #[test]
    fn sanitize_treats_a_stray_mid_word_quote_as_a_hard_delimiter() {
        // Both the bare-word and phrase readers stop exactly at a `"`, so a
        // quote can never survive into a single term's content — a `"`
        // appearing mid-word just ends that word and starts a new (here,
        // empty and dropped) phrase. The result is still always valid,
        // non-erroring FTS5 syntax, just as two implicit-AND terms rather
        // than one term with an embedded quote.
        assert_eq!(sanitize_fts_query("foo\"bar"), "\"foo\" \"bar\"");
    }

    #[test]
    fn sanitize_handles_unbalanced_and_empty_input_without_panicking() {
        assert_eq!(sanitize_fts_query("\""), "");
        assert_eq!(sanitize_fts_query("\"\""), "");
        assert_eq!(sanitize_fts_query(""), "");
        assert_eq!(sanitize_fts_query("   "), "");
        assert_eq!(sanitize_fts_query("\"unterminated"), "\"unterminated\"");
    }

    #[test]
    fn sanitize_strips_embedded_nul_bytes() {
        // Regression guard (found by the proptest below): FTS5 reads a
        // quoted term as a C string, so an embedded NUL truncates it and
        // raises "unterminated string" even though every `"` is escaped.
        assert_eq!(sanitize_fts_query("foo\0bar"), "\"foobar\"");
        assert_eq!(sanitize_fts_query("\0"), "");
    }

    #[test]
    fn sanitize_handles_multibyte_input_without_panicking() {
        // Regression guard: an earlier char-indexing approach that sliced by
        // byte offset instead of char offset could panic on non-ASCII input.
        assert_eq!(sanitize_fts_query("café"), "\"café\"");
        assert_eq!(sanitize_fts_query("日本語*"), "\"日本語\"*");
    }

    // -----------------------------------------------------------------------
    // fts_query_filtered: end-to-end, previously-erroring inputs
    // -----------------------------------------------------------------------

    fn make_fts_test_db() -> (Connection, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let conn = create_db(&dir.path().join("t.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose)
             VALUES ('/catalog/scripts/nightly-backup.sh', 'shell', 'nightly backup job', 'alice', '')",
            [],
        )
        .unwrap();
        (conn, dir)
    }

    #[test]
    fn fts_query_filtered_no_longer_errors_on_hyphenated_query() {
        let (conn, _dir) = make_fts_test_db();
        let rows = fts_query_filtered(&conn, "nightly-backup", 10, None, None, None).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn fts_query_filtered_returns_empty_instead_of_erroring_on_lone_quote() {
        let (conn, _dir) = make_fts_test_db();
        let rows = fts_query_filtered(&conn, "\"", 10, None, None, None).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn fts_query_filtered_no_longer_errors_on_leading_asterisk_or_bare_boolean_keyword() {
        let (conn, _dir) = make_fts_test_db();
        assert!(fts_query_filtered(&conn, "*nightly", 10, None, None, None).is_ok());
        assert!(fts_query_filtered(&conn, "AND", 10, None, None, None).is_ok());
        assert!(fts_query_filtered(&conn, "(nightly)", 10, None, None, None).is_ok());
    }

    // -----------------------------------------------------------------------
    // Property-based tests: sanitize_fts_query must turn *any* input into
    // either an empty string or syntactically valid FTS5 MATCH syntax. These
    // complement the fixed-case unit tests above by throwing arbitrary
    // (including adversarial and multibyte) input at the sanitizer rather
    // than a hand-picked set of operator characters.
    // -----------------------------------------------------------------------

    proptest! {
        #[test]
        fn sanitize_fts_query_never_panics(s in ".{0,120}") {
            let _ = sanitize_fts_query(&s);
        }

        #[test]
        fn sanitized_query_never_causes_an_fts5_syntax_error(s in ".{0,120}") {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(DDL).unwrap();
            conn.execute(
                "INSERT INTO scripts (logical_path, language, content, owner, purpose)
                 VALUES ('/catalog/scripts/nightly-backup.sh', 'shell', 'nightly backup job', 'alice', '')",
                [],
            )
            .unwrap();
            // A prior bug let unescaped FTS5 operator/boolean syntax reach
            // MATCH and error out; this exercises every code point sanitize
            // can be handed rather than a fixed list of "known bad" inputs.
            prop_assert!(fts_query_filtered(&conn, &s, 10, None, None, None).is_ok());
        }

        #[test]
        fn sanitized_terms_are_always_double_quoted(s in "[a-zA-Z0-9]{1,20}") {
            // A plain alphanumeric word (no whitespace/quotes/operators) always
            // round-trips as a single quoted term.
            prop_assert_eq!(sanitize_fts_query(&s), format!("\"{s}\""));
        }
    }
}
