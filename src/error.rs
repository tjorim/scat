use thiserror::Error;

/// Unified error type used by `scat_core`.
#[derive(Debug, Error)]
pub enum Error {
    /// Wrapper for SQLite-related failures.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    /// Requested database path does not exist.
    #[error("database not found: {0}")]
    NotFound(String),
    /// Mapping/parsing error while resolving paths or language mappings.
    #[error("mapping file error: {0}")]
    Mapping(String),
    /// Wrapper for I/O failures.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Wrapper for JSON serialization/deserialization failures.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// Wrapper for YAML serialization/deserialization failures.
    #[error("yaml error: {0}")]
    Yaml(#[from] yaml_serde::Error),
    /// Generic input or state validation failure.
    #[error("validation failed: {0}")]
    Validation(String),
    /// Indexed database schema version differs from current binary schema.
    ///
    /// The advice depends on which side is older: only a *newer* scat (the
    /// publisher) can fix an older catalog, and only upgrading the client
    /// can fix a newer one — clients never rebuild the shared catalog.
    #[error("catalog schema is version {found}, expected {expected} — {}", schema_mismatch_advice(*found, *expected))]
    SchemaMismatch {
        /// Schema version found in the opened database.
        found: i64,
        /// Schema version expected by the running binary.
        expected: i64,
    },
    /// Indexing was interrupted and resumable checkpoint data was written.
    #[error("interrupted — checkpoint saved; re-run to resume")]
    Interrupted,
}

/// Convenience result type using [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Actionable next step for a [`Error::SchemaMismatch`], by direction.
fn schema_mismatch_advice(found: i64, expected: i64) -> &'static str {
    if found < expected {
        "the catalog is older than this scat; the catalog publisher must upgrade scat \
         and run `scat catalog build` (clients cannot rebuild it; see docs/COMPATIBILITY.md)"
    } else {
        "this scat is older than the catalog; upgrade scat on this host \
         (do not rebuild the shared catalog; see docs/COMPATIBILITY.md)"
    }
}
