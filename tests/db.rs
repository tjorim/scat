use scat_core::core::db::{SCHEMA_VERSION, create_db, open_db};
use scat_core::error::Error;
use tempfile::NamedTempFile;

fn insert_index_metadata(conn: &rusqlite::Connection, schema_version: i64) {
    conn.execute(
        "INSERT INTO index_metadata (id, build_timestamp, schema_version) VALUES (1, '2024-01-01T00:00:00', ?1)",
        rusqlite::params![schema_version],
    )
    .unwrap();
}

#[test]
fn open_db_rejects_missing_index_metadata() {
    let db = NamedTempFile::new().unwrap();
    create_db(db.path()).unwrap().close().unwrap();

    let err = open_db(db.path()).unwrap_err();
    assert!(matches!(&err, Error::Validation(_)));
    assert!(err.to_string().contains("index_metadata"));
    assert!(
        err.to_string()
            .contains("re-index required (scat catalog build)")
    );
}

#[test]
fn create_db_creates_revisions_table() {
    let db = NamedTempFile::new().unwrap();
    let conn = create_db(db.path()).unwrap();
    let revisions_table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'revisions'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(revisions_table_exists, 1);
}

#[test]
fn open_db_rejects_schema_mismatch() {
    let db = NamedTempFile::new().unwrap();
    let conn = create_db(db.path()).unwrap();
    insert_index_metadata(&conn, SCHEMA_VERSION - 1);
    conn.close().unwrap();

    let err = open_db(db.path()).unwrap_err();
    assert!(matches!(
        &err,
        Error::SchemaMismatch {
            found,
            expected
        } if *found == SCHEMA_VERSION - 1 && *expected == SCHEMA_VERSION
    ));
    assert!(err.to_string().contains(&format!(
        "catalog schema is version {}, expected {}",
        SCHEMA_VERSION - 1,
        SCHEMA_VERSION
    )));
    assert!(
        err.to_string()
            .contains("re-index required (scat catalog build)")
    );
}
