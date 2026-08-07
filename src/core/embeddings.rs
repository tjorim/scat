//! Reads the optional `embeddings.sqlite` sidecar produced by
//! `crates/scat-embed` and finds scripts whose embedding is most similar to
//! a given script's, by cosine similarity.
//!
//! This is deliberately consumption-only: nothing here computes an
//! embedding, which is what lets it live in `scat_core` without pulling in
//! an embedding-model runtime or any internet-dependent code path (see
//! `scat_core`'s Unix-only, offline-first design in `docs/ARCHITECTURE.md`).
//! The tradeoff that buys is scope — [`EmbeddingsSidecar::nearest`] can only
//! recommend scripts *related to one already in the catalog*, never answer
//! an arbitrary free-text query, since that would require embedding the
//! query itself at search time. Turning a typed query into a vector without
//! giving `scat_core` a model dependency is the open design question tracked
//! in <https://github.com/tjorim/scat/issues/73>; this module sidesteps it
//! rather than answering it.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use tracing::warn;

/// The sidecar's conventional location: same directory as the published
/// catalog, fixed filename. `scat-embed --output` can point anywhere, but
/// this is the path `scat` looks for.
pub const SIDECAR_FILE_NAME: &str = "embeddings.sqlite";

/// Returns the conventional sidecar path for a published catalog at `catalog_path`.
#[must_use]
pub fn sidecar_path(catalog_path: &Path) -> PathBuf {
    catalog_path.with_file_name(SIDECAR_FILE_NAME)
}

/// One script and its stored embedding vector.
struct Embedding {
    script_id: i64,
    logical_path: String,
    vector: Vec<f32>,
}

/// One result from [`EmbeddingsSidecar::nearest`].
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct SimilarScript {
    /// Logical path of the similar script.
    pub logical_path: String,
    /// Cosine similarity to the query script's embedding, in `[-1.0, 1.0]`
    /// (in practice close to `[0.0, 1.0]` for the embedding models
    /// `scat-embed` uses, which produce non-negative components).
    pub score: f32,
}

/// A read-only, fully-loaded view of an `embeddings.sqlite` sidecar.
///
/// Loaded eagerly rather than queried per call: sidecars hold one row per
/// cataloged script (a few thousand at most), so holding every vector in
/// memory and scanning it directly is simpler than, and about as fast as,
/// staging the comparison back through SQLite — and it avoids taking on a
/// vector-index dependency for a table this size.
pub struct EmbeddingsSidecar {
    embeddings: Vec<Embedding>,
    model: String,
}

impl EmbeddingsSidecar {
    /// Open the sidecar at `path` read-only and load it into memory.
    ///
    /// Returns `Ok(None)` when `path` doesn't exist: the sidecar is built
    /// out of band by a separate tool and is never guaranteed to be present,
    /// so a missing file means the feature isn't available yet, not that the
    /// command failed. Any other failure to open or read it (not a SQLite
    /// database, missing table) is logged and also folded into `Ok(None)`,
    /// on the same reasoning `core::cache` uses for the catalog cache: a
    /// broken or half-configured sidecar shouldn't take down a command that
    /// depends on it, only leave the feature unavailable.
    pub fn open(path: &Path) -> Option<Self> {
        if !path.is_file() {
            return None;
        }
        let conn = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(conn) => conn,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "failed to open embeddings sidecar");
                return None;
            }
        };
        match read_embeddings(&conn) {
            Ok((embeddings, model)) => Some(Self { embeddings, model }),
            Err(err) => {
                warn!(path = %path.display(), error = %err, "embeddings sidecar is unreadable");
                None
            }
        }
    }

    /// Embedding model recorded in the sidecar (every row shares one, since
    /// a `scat-embed` run writes a single model's output). Empty when the
    /// sidecar itself has no rows.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Number of scripts with a stored embedding.
    #[must_use]
    pub fn len(&self) -> usize {
        self.embeddings.len()
    }

    /// True when the sidecar has no embedded scripts at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.embeddings.is_empty()
    }

    /// True when `logical_path` has a stored embedding.
    #[must_use]
    pub fn contains(&self, logical_path: &str) -> bool {
        self.embeddings
            .iter()
            .any(|e| e.logical_path == logical_path)
    }

    /// The up to `limit` scripts whose embedding is most similar to
    /// `logical_path`'s (highest cosine similarity first), excluding itself.
    ///
    /// Returns `None` when `logical_path` has no stored embedding — for
    /// example a script added to the catalog after the sidecar was last
    /// built. Callers should treat that as "no embedding for this script
    /// yet", not "no similar scripts".
    #[must_use]
    pub fn nearest(&self, logical_path: &str, limit: usize) -> Option<Vec<SimilarScript>> {
        let target = self
            .embeddings
            .iter()
            .find(|e| e.logical_path == logical_path)?;

        let mut scored: Vec<SimilarScript> = self
            .embeddings
            .iter()
            .filter(|e| e.script_id != target.script_id)
            .map(|e| SimilarScript {
                logical_path: e.logical_path.clone(),
                score: cosine_similarity(&target.vector, &e.vector),
            })
            .collect();
        scored.sort_by(|a, b| b.score.total_cmp(&a.score));
        scored.truncate(limit);
        Some(scored)
    }
}

fn read_embeddings(conn: &Connection) -> rusqlite::Result<(Vec<Embedding>, String)> {
    let mut stmt =
        conn.prepare("SELECT script_id, logical_path, model, embedding FROM script_embeddings")?;
    let mut model = String::new();
    let embeddings = stmt
        .query_map([], |row| {
            let script_id: i64 = row.get(0)?;
            let logical_path: String = row.get(1)?;
            let row_model: String = row.get(2)?;
            let bytes: Vec<u8> = row.get(3)?;
            Ok((script_id, logical_path, row_model, bytes))
        })?
        .map(|r| {
            let (script_id, logical_path, row_model, bytes) = r?;
            model = row_model;
            Ok(Embedding {
                script_id,
                logical_path,
                vector: bytes_to_vector(&bytes),
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((embeddings, model))
}

/// Inverse of `scat-embed`'s `f32::to_le_bytes` encoding.
fn bytes_to_vector(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity between two vectors. Mismatched lengths (a sidecar
/// mixing embeddings from two different models) or an all-zero vector never
/// rank as similar, rather than dividing by zero or panicking on a length
/// mismatch.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return f32::MIN;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_sidecar(path: &Path, rows: &[(i64, &str, &[f32])]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE script_embeddings (
                script_id     INTEGER PRIMARY KEY,
                logical_path  TEXT    NOT NULL,
                model         TEXT    NOT NULL,
                dim           INTEGER NOT NULL,
                embedding     BLOB    NOT NULL
            );",
        )
        .unwrap();
        for (id, path, vector) in rows {
            let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
            conn.execute(
                "INSERT INTO script_embeddings (script_id, logical_path, model, dim, embedding)
                 VALUES (?1, ?2, 'test-model', ?3, ?4)",
                rusqlite::params![id, path, vector.len() as i64, bytes],
            )
            .unwrap();
        }
    }

    #[test]
    fn sidecar_path_replaces_the_catalog_file_name() {
        let catalog = Path::new("/share/catalog/scripts.sqlite");
        assert_eq!(
            sidecar_path(catalog),
            PathBuf::from("/share/catalog/embeddings.sqlite")
        );
    }

    #[test]
    fn open_returns_none_for_a_missing_file() {
        let dir = TempDir::new().unwrap();
        assert!(EmbeddingsSidecar::open(&dir.path().join("nope.sqlite")).is_none());
    }

    #[test]
    fn open_returns_none_for_an_unreadable_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("garbage.sqlite");
        std::fs::write(&path, b"not a database").unwrap();
        assert!(EmbeddingsSidecar::open(&path).is_none());
    }

    #[test]
    fn loads_rows_and_reports_the_model() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("embeddings.sqlite");
        write_sidecar(
            &path,
            &[
                (1, "/catalog/a.py", &[1.0, 0.0]),
                (2, "/catalog/b.py", &[0.0, 1.0]),
            ],
        );

        let sidecar = EmbeddingsSidecar::open(&path).unwrap();
        assert_eq!(sidecar.len(), 2);
        assert!(!sidecar.is_empty());
        assert_eq!(sidecar.model(), "test-model");
        assert!(sidecar.contains("/catalog/a.py"));
        assert!(!sidecar.contains("/catalog/missing.py"));
    }

    #[test]
    fn nearest_ranks_by_cosine_similarity_and_excludes_the_query_script() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("embeddings.sqlite");
        write_sidecar(
            &path,
            &[
                (1, "/catalog/query.py", &[1.0, 0.0]),
                (2, "/catalog/close.py", &[0.9, 0.1]),
                (3, "/catalog/far.py", &[0.0, 1.0]),
            ],
        );
        let sidecar = EmbeddingsSidecar::open(&path).unwrap();

        let results = sidecar.nearest("/catalog/query.py", 10).unwrap();
        let paths: Vec<&str> = results.iter().map(|r| r.logical_path.as_str()).collect();
        assert_eq!(paths, vec!["/catalog/close.py", "/catalog/far.py"]);
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn nearest_respects_the_limit() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("embeddings.sqlite");
        write_sidecar(
            &path,
            &[
                (1, "/catalog/query.py", &[1.0, 0.0]),
                (2, "/catalog/a.py", &[0.9, 0.1]),
                (3, "/catalog/b.py", &[0.8, 0.2]),
                (4, "/catalog/c.py", &[0.7, 0.3]),
            ],
        );
        let sidecar = EmbeddingsSidecar::open(&path).unwrap();

        let results = sidecar.nearest("/catalog/query.py", 2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn nearest_returns_none_for_an_unembedded_script() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("embeddings.sqlite");
        write_sidecar(&path, &[(1, "/catalog/a.py", &[1.0, 0.0])]);
        let sidecar = EmbeddingsSidecar::open(&path).unwrap();

        assert!(sidecar.nearest("/catalog/never-embedded.py", 10).is_none());
    }

    #[test]
    fn cosine_similarity_of_identical_vectors_is_one() {
        assert!((cosine_similarity(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_of_orthogonal_vectors_is_zero() {
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_handles_zero_vectors_without_dividing_by_zero() {
        assert!((cosine_similarity(&[0.0, 0.0], &[1.0, 1.0])).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_similarity_of_mismatched_lengths_never_ranks_as_similar() {
        assert!(cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]) <= f32::MIN);
    }
}
