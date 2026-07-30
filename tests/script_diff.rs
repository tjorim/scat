/// Integration tests for `scat diff` — script-level diff command.
///
/// Each test builds a minimal `SQLite` database with the production schema and
/// exercises the public diff API directly (without spawning the CLI binary).
use scat_core::core::db::{SCHEMA_VERSION, create_db};
use scat_core::core::diff::{
    DiffLineKind, ScriptDiffResult, SourceKind, compute_diff, diff_catalog_vs_checkout,
    diff_catalog_vs_file, diff_files,
};
use tempfile::NamedTempFile;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct TestDb {
    api_conn: rusqlite::Connection,
    _file: NamedTempFile,
}

fn make_db() -> TestDb {
    let f = NamedTempFile::new().unwrap();
    let conn = create_db(f.path()).unwrap();
    conn.execute(
        "INSERT INTO index_metadata (id, build_timestamp, schema_version) VALUES (1, '2024-01-01T00:00:00', ?)",
        rusqlite::params![SCHEMA_VERSION],
    ).unwrap();
    TestDb {
        api_conn: conn,
        _file: f,
    }
}

fn insert_script(db: &TestDb, logical_path: &str, content: &str) {
    db.api_conn
        .execute(
            "INSERT INTO scripts (logical_path, language, content) VALUES (?1, 'python', ?2)",
            rusqlite::params![logical_path, content],
        )
        .unwrap();
}

fn insert_checkout(db: &TestDb, logical_path: &str, physical_path: &str, timestamp: &str) {
    db.api_conn
        .execute(
            "INSERT INTO revisions
             (logical_path, physical_path, revision_type, os_flavor, user, timestamp)
             VALUES (?1, ?2, 'DEVELOP', 'LINUX', 'testuser', ?3)",
            rusqlite::params![logical_path, physical_path, timestamp],
        )
        .unwrap();
}

fn write_tmp_file(content: &str) -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), content).unwrap();
    f
}

// ---------------------------------------------------------------------------
// diff_files — explicit old/new comparison
// ---------------------------------------------------------------------------

#[test]
fn diff_files_identical_produces_no_hunks() {
    let old = write_tmp_file("line1\nline2\nline3\n");
    let new = write_tmp_file("line1\nline2\nline3\n");

    let result = diff_files(old.path(), new.path()).unwrap();
    assert!(result.logical_path.is_none(), "logical_path should be None");
    assert_eq!(result.old_kind, SourceKind::Explicit);
    assert_eq!(result.new_kind, SourceKind::Explicit);
    assert!(result.hunks.is_empty(), "no hunks for identical files");
}

#[test]
fn diff_files_changed_line() {
    let old = write_tmp_file("a\nb\nc\n");
    let new = write_tmp_file("a\nB\nc\n");

    let result = diff_files(old.path(), new.path()).unwrap();
    assert_eq!(result.hunks.len(), 1);
    let hunk = &result.hunks[0];
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
fn diff_files_missing_old_returns_error() {
    let new = write_tmp_file("content\n");
    let result = diff_files(std::path::Path::new("/no/such/file.py"), new.path());
    assert!(result.is_err(), "should error on missing old file");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("/no/such/file.py"),
        "error should mention the path"
    );
}

#[test]
fn diff_files_missing_new_returns_error() {
    let old = write_tmp_file("content\n");
    let result = diff_files(old.path(), std::path::Path::new("/no/such/new.py"));
    assert!(result.is_err(), "should error on missing new file");
}

#[test]
fn diff_files_json_output_has_null_logical_path() {
    let old = write_tmp_file("x\n");
    let new = write_tmp_file("y\n");
    let result = diff_files(old.path(), new.path()).unwrap();
    let json = serde_json::to_value(&result).unwrap();
    assert!(json["logical_path"].is_null(), "logical_path must be null");
}

// ---------------------------------------------------------------------------
// diff_catalog_vs_checkout — automatic checkout discovery
// ---------------------------------------------------------------------------

#[test]
fn diff_catalog_vs_checkout_basic() {
    let db = make_db();
    let checkout_file = write_tmp_file("def foo():\n    pass\n");

    insert_script(&db, "/catalog/scripts/foo.py", "def foo():\n    return 1\n");
    insert_checkout(
        &db,
        "/catalog/scripts/foo.py",
        checkout_file.path().to_str().unwrap(),
        "20240315_1430",
    );

    let result = diff_catalog_vs_checkout(&db.api_conn, "/catalog/scripts/foo.py").unwrap();
    assert_eq!(
        result.logical_path.as_deref(),
        Some("/catalog/scripts/foo.py")
    );
    assert_eq!(result.old_kind, SourceKind::Active);
    assert_eq!(result.new_kind, SourceKind::Checkout);
    assert!(
        result.old_path.contains("/catalog/scripts/foo.py"),
        "old_path should reference logical path"
    );
}

#[test]
fn diff_catalog_vs_checkout_picks_most_recent() {
    let db = make_db();

    // Two checkouts — the one with the later timestamp should win.
    let older_file = write_tmp_file("# older checkout\n");
    let newer_file = write_tmp_file("# newer checkout\n");

    insert_script(&db, "/catalog/scripts/bar.py", "# catalog content\n");
    insert_checkout(
        &db,
        "/catalog/scripts/bar.py",
        older_file.path().to_str().unwrap(),
        "20240101_0900",
    );
    insert_checkout(
        &db,
        "/catalog/scripts/bar.py",
        newer_file.path().to_str().unwrap(),
        "20240315_1430",
    );

    let result = diff_catalog_vs_checkout(&db.api_conn, "/catalog/scripts/bar.py").unwrap();
    assert!(
        result
            .new_path
            .contains(newer_file.path().to_str().unwrap()),
        "most-recent checkout should be used; got: {}",
        result.new_path
    );
}

#[test]
fn diff_catalog_vs_checkout_no_checkouts_errors_with_hints() {
    let db = make_db();
    insert_script(&db, "/catalog/scripts/nocheckout.py", "x = 1\n");

    let err = diff_catalog_vs_checkout(&db.api_conn, "/catalog/scripts/nocheckout.py").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("No vc checkouts found"),
        "error should mention missing checkouts: {msg}"
    );
    assert!(
        msg.contains("--against"),
        "error should hint at --against: {msg}"
    );
}

#[test]
fn diff_catalog_vs_checkout_script_not_in_catalog_errors() {
    let db = make_db();
    let err = diff_catalog_vs_checkout(&db.api_conn, "/catalog/scripts/missing.py").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not found in catalog"),
        "error should mention catalog miss: {msg}"
    );
}

#[test]
fn diff_catalog_vs_checkout_missing_checkout_file_errors() {
    let db = make_db();
    insert_script(&db, "/catalog/scripts/ghost.py", "x = 1\n");
    insert_checkout(
        &db,
        "/catalog/scripts/ghost.py",
        "/no/such/checkout_20240315_1430_user",
        "20240315_1430",
    );

    let err = diff_catalog_vs_checkout(&db.api_conn, "/catalog/scripts/ghost.py").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Cannot read") || msg.contains("not found"),
        "error should mention unreadable file: {msg}"
    );
}

// ---------------------------------------------------------------------------
// diff_catalog_vs_file — --against mode
// ---------------------------------------------------------------------------

#[test]
fn diff_catalog_vs_file_basic() {
    let db = make_db();
    let against = write_tmp_file("x = 2\n");
    insert_script(&db, "/catalog/scripts/x.py", "x = 1\n");

    let result =
        diff_catalog_vs_file(&db.api_conn, "/catalog/scripts/x.py", against.path()).unwrap();
    assert_eq!(
        result.logical_path.as_deref(),
        Some("/catalog/scripts/x.py")
    );
    assert_eq!(result.old_kind, SourceKind::Active);
    assert_eq!(result.new_kind, SourceKind::Explicit);
    assert_eq!(result.hunks.len(), 1);
}

#[test]
fn diff_catalog_vs_file_missing_against_errors() {
    let db = make_db();
    insert_script(&db, "/catalog/scripts/y.py", "y = 1\n");

    let err = diff_catalog_vs_file(
        &db.api_conn,
        "/catalog/scripts/y.py",
        std::path::Path::new("/no/such/file.py"),
    )
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Cannot read") || msg.contains("not found"),
        "error should mention unreadable file: {msg}"
    );
}

// ---------------------------------------------------------------------------
// JSON output shape
// ---------------------------------------------------------------------------

#[test]
fn diff_result_json_has_required_fields() {
    let old = write_tmp_file("a\nb\n");
    let new = write_tmp_file("a\nB\n");
    let result = diff_files(old.path(), new.path()).unwrap();

    let json = serde_json::to_value(&result).unwrap();
    assert!(json.get("logical_path").is_some(), "logical_path field");
    assert!(json.get("old_path").is_some(), "old_path field");
    assert!(json.get("new_path").is_some(), "new_path field");
    assert!(json.get("old_kind").is_some(), "old_kind field");
    assert!(json.get("new_kind").is_some(), "new_kind field");
    assert!(json.get("hunks").is_some(), "hunks field");
}

#[test]
fn diff_hunk_json_has_required_fields() {
    let old = write_tmp_file("a\nb\n");
    let new = write_tmp_file("a\nB\n");
    let result: ScriptDiffResult = diff_files(old.path(), new.path()).unwrap();

    let json = serde_json::to_value(&result).unwrap();
    let hunk = &json["hunks"][0];
    assert!(hunk.get("old_start").is_some(), "old_start");
    assert!(hunk.get("old_count").is_some(), "old_count");
    assert!(hunk.get("new_start").is_some(), "new_start");
    assert!(hunk.get("new_count").is_some(), "new_count");
    assert!(hunk.get("lines").is_some(), "lines");

    let line = &hunk["lines"][0];
    assert!(line.get("kind").is_some(), "line.kind");
    assert!(line.get("content").is_some(), "line.content");
}

// ---------------------------------------------------------------------------
// compute_diff edge cases
// ---------------------------------------------------------------------------

#[test]
fn compute_diff_no_trailing_newline() {
    // Files without trailing newlines should still diff correctly.
    let hunks = compute_diff("a\nb", "a\nB");
    assert_eq!(hunks.len(), 1);
}

#[test]
fn compute_diff_hunk_counts_correct() {
    let hunks = compute_diff("a\nb\nc\n", "a\nB\nc\n");
    assert_eq!(hunks.len(), 1);
    let h = &hunks[0];
    // 1 context (a) + 1 delete (b) + 1 insert (B) + 1 context (c)
    assert_eq!(h.old_count, 3); // a + b + c
    assert_eq!(h.new_count, 3); // a + B + c
}
