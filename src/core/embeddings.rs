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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use thiserror::Error;
use tracing::warn;

/// Sidecar layout this scat reads (`script_embeddings`, plus the optional
/// `embedding_metadata` table). `scat-embed` writes the same constant; a
/// sidecar with no `embedding_metadata` table predates versioning and is
/// format 1. Bump it only for a layout older clients cannot read, and update
/// `docs/COMPATIBILITY.md`.
pub const SIDECAR_FORMAT_VERSION: i64 = 1;

/// Why a sidecar that exists could not be used.
#[derive(Debug, Error)]
pub enum SidecarError {
    /// Written by a newer `scat-embed` in a layout this scat does not read.
    #[error(
        "embeddings sidecar {path} is format {found}, but this scat reads format {supported} — \
         this scat is older than the sidecar; upgrade scat on this host (see docs/COMPATIBILITY.md)"
    )]
    UnsupportedFormat {
        /// Sidecar that was opened.
        path: String,
        /// Format recorded in the sidecar.
        found: i64,
        /// Format this scat reads.
        supported: i64,
    },
    /// Not a SQLite database, or missing/odd tables.
    #[error(
        "embeddings sidecar {path} is unreadable ({reason}) — rebuild and republish it with scat-embed"
    )]
    Unreadable {
        /// Sidecar that was opened.
        path: String,
        /// Underlying failure.
        reason: String,
    },
}

/// The sidecar's conventional location: same directory as the published
/// catalog, fixed filename. `scat-embed --output` can point anywhere, but
/// this is the path `scat` looks for.
pub const SIDECAR_FILE_NAME: &str = "embeddings.sqlite";

/// Returns the conventional sidecar path for a published catalog at `catalog_path`.
#[must_use]
pub fn sidecar_path(catalog_path: &Path) -> PathBuf {
    catalog_path.with_file_name(SIDECAR_FILE_NAME)
}

/// One script and its stored embedding vector, scaled to unit length when it
/// is loaded so that the cosine similarity of two embeddings is just their
/// dot product (see [`similarity`]).
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

/// One result from [`EmbeddingsSidecar::near_duplicates`].
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct NearDuplicatePair {
    /// Logical path of one script in the pair (lexicographically first).
    pub a: String,
    /// Logical path of the other script in the pair.
    pub b: String,
    /// Cosine similarity between the pair's embeddings.
    pub score: f32,
}

/// A read-only, fully-loaded view of an `embeddings.sqlite` sidecar.
///
/// Loaded eagerly rather than queried per call: holding every vector in memory
/// and scanning it directly is simpler than, and about as fast as, staging the
/// comparison back through SQLite — and it avoids taking on a vector-index
/// dependency. Vectors are normalised once at load, so a similarity is a dot
/// product; the pairwise checks (`near_duplicates`, `outliers`) are quadratic
/// in the number of scripts and run across all cores; lookups by path go
/// through a hash index.
pub struct EmbeddingsSidecar {
    embeddings: Vec<Embedding>,
    /// `logical_path` → position in `embeddings`.
    by_path: HashMap<String, usize>,
    model: String,
    /// `index_metadata.build_timestamp` of the catalog the sidecar was built
    /// from; `None` for a sidecar written before provenance was recorded.
    catalog_build_timestamp: Option<String>,
}

impl EmbeddingsSidecar {
    /// Open the sidecar at `path` read-only and load it into memory.
    ///
    /// Returns `Ok(None)` when `path` doesn't exist: the sidecar is built
    /// out of band by a separate tool and is never guaranteed to be present,
    /// so a missing file means the feature isn't available yet, not that the
    /// command failed. A sidecar that exists but cannot be used is an
    /// [`SidecarError`] saying why (unsupported newer format, unreadable) so
    /// callers can tell it apart from "absent" instead of pairing with it
    /// silently or hiding it.
    pub fn open_checked(path: &Path) -> Result<Option<Self>, SidecarError> {
        if !path.is_file() {
            return Ok(None);
        }
        let unreadable = |err: &dyn std::fmt::Display| SidecarError::Unreadable {
            path: path.display().to_string(),
            reason: err.to_string(),
        };
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| unreadable(&e))?;
        let (format_version, catalog_build_timestamp) =
            read_metadata(&conn).map_err(|e| unreadable(&e))?;
        if format_version > SIDECAR_FORMAT_VERSION {
            return Err(SidecarError::UnsupportedFormat {
                path: path.display().to_string(),
                found: format_version,
                supported: SIDECAR_FORMAT_VERSION,
            });
        }
        let (embeddings, model) = read_embeddings(&conn).map_err(|e| unreadable(&e))?;
        let by_path = embeddings
            .iter()
            .enumerate()
            .map(|(index, e)| (e.logical_path.clone(), index))
            .collect();
        Ok(Some(Self {
            embeddings,
            by_path,
            model,
            catalog_build_timestamp,
        }))
    }

    /// [`open_checked`](Self::open_checked) for optional consumers: absent,
    /// unsupported and unreadable sidecars all leave the feature unavailable
    /// rather than failing the command, on the same reasoning `core::cache`
    /// uses for the catalog cache. The reason is logged; callers that can
    /// show it to the user should use `open_checked` instead.
    pub fn open(path: &Path) -> Option<Self> {
        match Self::open_checked(path) {
            Ok(sidecar) => sidecar,
            Err(err) => {
                warn!(path = %path.display(), error = %err, "embeddings sidecar not usable");
                None
            }
        }
    }

    /// Note to show the user when the sidecar was built from a different
    /// catalog build than `catalog_build_timestamp` (the one being queried).
    ///
    /// Pairing is by `logical_path`, so a sidecar from another build is still
    /// usable — scat-embed runs out of band and is routinely older than the
    /// nightly catalog — but never silently. `None` when the builds match or
    /// the sidecar records no provenance (published by an older scat-embed).
    #[must_use]
    pub fn provenance_note(&self, catalog_build_timestamp: &str) -> Option<String> {
        let built_from = self.catalog_build_timestamp.as_deref()?;
        (built_from != catalog_build_timestamp).then(|| {
            format!(
                "note: embeddings sidecar ({}) was built from catalog build {built_from}, \
                 but the catalog is build {catalog_build_timestamp}; results may not reflect \
                 scripts changed since — rerun scat-embed against the current catalog",
                self.model,
            )
        })
    }

    /// Catalog build the sidecar was generated from, when it recorded one.
    #[must_use]
    pub fn catalog_build_timestamp(&self) -> Option<&str> {
        self.catalog_build_timestamp.as_deref()
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
        self.by_path.contains_key(logical_path)
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
        let target = &self.embeddings[*self.by_path.get(logical_path)?];

        let mut scored: Vec<SimilarScript> = self
            .embeddings
            .iter()
            .filter(|e| e.script_id != target.script_id)
            .map(|e| SimilarScript {
                logical_path: e.logical_path.clone(),
                score: similarity(&target.vector, &e.vector),
            })
            .collect();
        scored.sort_by(|a, b| b.score.total_cmp(&a.score));
        scored.truncate(limit);
        Some(scored)
    }

    /// All pairs of embedded scripts whose cosine similarity is at or above
    /// `threshold`, sorted by score descending (ties broken by path).
    ///
    /// Each relationship is reported once: `a` is always the
    /// lexicographically smaller of the pair's two logical paths, so a
    /// caller iterating pairs never sees both `(x, y)` and `(y, x)`.
    /// Quadratic in the sidecar's row count (every pair is compared), spread
    /// across all cores; only pairs at or above `threshold` are materialised.
    #[must_use]
    pub fn near_duplicates(&self, threshold: f32) -> Vec<NearDuplicatePair> {
        let embeddings = &self.embeddings;
        let mut pairs: Vec<NearDuplicatePair> = (0..embeddings.len())
            .into_par_iter()
            .flat_map_iter(|i| {
                let x = &embeddings[i];
                embeddings[i + 1..].iter().filter_map(move |y| {
                    let score = similarity(&x.vector, &y.vector);
                    (score >= threshold).then(|| {
                        let (a, b) = if x.logical_path <= y.logical_path {
                            (x.logical_path.clone(), y.logical_path.clone())
                        } else {
                            (y.logical_path.clone(), x.logical_path.clone())
                        };
                        NearDuplicatePair { a, b, score }
                    })
                })
            })
            .collect();
        pairs.sort_by(|p, q| q.score.total_cmp(&p.score).then_with(|| p.a.cmp(&q.a)));
        pairs
    }

    /// Scripts whose *best* match among all other embedded scripts still
    /// scores below `threshold` — the catalog's most one-of-a-kind scripts.
    /// Sorted by score ascending (most unique first).
    ///
    /// A sidecar with fewer than two rows has nothing to compare against
    /// and always returns empty, rather than reporting every script as an
    /// outlier by default.
    #[must_use]
    pub fn outliers(&self, threshold: f32) -> Vec<SimilarScript> {
        if self.embeddings.len() < 2 {
            return Vec::new();
        }
        let mut results: Vec<SimilarScript> = self
            .embeddings
            .par_iter()
            .map(|e| {
                let best = self
                    .embeddings
                    .iter()
                    .filter(|o| o.script_id != e.script_id)
                    .map(|o| similarity(&e.vector, &o.vector))
                    .fold(f32::MIN, f32::max);
                SimilarScript {
                    logical_path: e.logical_path.clone(),
                    score: best,
                }
            })
            .filter(|s| s.score < threshold)
            .collect();
        results.sort_by(|a, b| {
            a.score
                .total_cmp(&b.score)
                .then_with(|| a.logical_path.cmp(&b.logical_path))
        });
        results
    }
}

/// `(format_version, catalog_build_timestamp)` from `embedding_metadata`.
/// A sidecar without that table is a pre-versioning one: format 1, no provenance.
fn read_metadata(conn: &Connection) -> rusqlite::Result<(i64, Option<String>)> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'embedding_metadata')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok((1, None));
    }
    Ok(conn
        .query_row(
            "SELECT format_version, catalog_build_timestamp FROM embedding_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or((1, None)))
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
                vector: normalised(bytes_to_vector(&bytes)),
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((embeddings, model))
}

/// Inverse of `scat-embed`'s `f32::to_le_bytes` encoding.
fn bytes_to_vector(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// `vector` scaled to unit length, so cosine similarity becomes a dot product.
/// An all-zero (or non-finite) vector is kept as zeros: it has no direction,
/// and its similarity to anything is 0 either way.
fn normalised(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 && norm.is_finite() {
        for x in &mut vector {
            *x /= norm;
        }
    } else {
        vector.fill(0.0);
    }
    vector
}

/// Cosine similarity of two vectors already scaled by [`normalised`]. Mismatched
/// lengths (a sidecar mixing embeddings from two different models) or empty
/// vectors never rank as similar, rather than panicking on a length mismatch.
fn similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return f32::MIN;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Cosine similarity of raw (unnormalised) vectors, the way a sidecar's
    /// vectors are compared once loaded.
    fn cos(a: &[f32], b: &[f32]) -> f32 {
        similarity(&normalised(a.to_vec()), &normalised(b.to_vec()))
    }

    /// Reference implementation straight from the definition, no
    /// pre-normalisation: what `similarity` must agree with.
    fn naive_cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (na * nb)
    }

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
    fn near_duplicates_reports_pairs_at_or_above_the_threshold_once_each() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("embeddings.sqlite");
        write_sidecar(
            &path,
            &[
                (1, "/catalog/b.py", &[1.0, 0.0]),
                (2, "/catalog/a.py", &[0.999, 0.001]),
                (3, "/catalog/unrelated.py", &[0.0, 1.0]),
            ],
        );
        let sidecar = EmbeddingsSidecar::open(&path).unwrap();

        let pairs = sidecar.near_duplicates(0.99);
        assert_eq!(pairs.len(), 1);
        // Lexicographically smaller path first, regardless of insertion order.
        assert_eq!(pairs[0].a, "/catalog/a.py");
        assert_eq!(pairs[0].b, "/catalog/b.py");
        assert!(pairs[0].score >= 0.99);
    }

    #[test]
    fn near_duplicates_excludes_pairs_below_the_threshold() {
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

        assert!(sidecar.near_duplicates(0.99).is_empty());
    }

    #[test]
    fn outliers_reports_scripts_whose_best_match_is_below_the_threshold() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("embeddings.sqlite");
        write_sidecar(
            &path,
            &[
                (1, "/catalog/a.py", &[1.0, 0.0]),
                (2, "/catalog/b.py", &[0.99, 0.14]),
                (3, "/catalog/lonely.py", &[0.0, 1.0]),
            ],
        );
        let sidecar = EmbeddingsSidecar::open(&path).unwrap();

        let results = sidecar.outliers(0.5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].logical_path, "/catalog/lonely.py");
    }

    #[test]
    fn outliers_is_empty_with_fewer_than_two_embedded_scripts() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("embeddings.sqlite");
        write_sidecar(&path, &[(1, "/catalog/a.py", &[1.0, 0.0])]);
        let sidecar = EmbeddingsSidecar::open(&path).unwrap();

        assert!(sidecar.outliers(0.99).is_empty());
    }

    #[test]
    fn cosine_similarity_of_identical_vectors_is_one() {
        assert!((cos(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_of_orthogonal_vectors_is_zero() {
        assert!(cos(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_handles_zero_vectors_without_dividing_by_zero() {
        assert!((cos(&[0.0, 0.0], &[1.0, 1.0])).abs() < f32::EPSILON);
    }

    #[test]
    fn cosine_similarity_of_mismatched_lengths_never_ranks_as_similar() {
        assert!(cos(&[1.0, 0.0], &[1.0, 0.0, 0.0]) <= f32::MIN);
    }

    /// A deterministic pseudo-random sidecar, to compare the optimised paths
    /// against the definition.
    fn random_sidecar(
        dir: &TempDir,
        rows: usize,
        dim: usize,
    ) -> (EmbeddingsSidecar, Vec<Vec<f32>>) {
        let mut state = 0x2545_f491_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state % 2000) as f32 / 1000.0 - 1.0
        };
        let vectors: Vec<Vec<f32>> = (0..rows)
            .map(|_| (0..dim).map(|_| next()).collect())
            .collect();
        let paths: Vec<String> = (0..rows).map(|i| format!("/s/{i:04}.py")).collect();
        let sidecar_rows: Vec<(i64, &str, &[f32])> = vectors
            .iter()
            .zip(&paths)
            .enumerate()
            .map(|(i, (v, p))| (i as i64 + 1, p.as_str(), v.as_slice()))
            .collect();
        let path = dir.path().join("embeddings.sqlite");
        write_sidecar(&path, &sidecar_rows);
        (
            EmbeddingsSidecar::open_checked(&path).unwrap().unwrap(),
            vectors,
        )
    }

    #[test]
    fn optimised_similarity_agrees_with_the_definition() {
        let dir = TempDir::new().unwrap();
        let (sidecar, vectors) = random_sidecar(&dir, 60, 16);

        // Pairs above a threshold, as the definition would find them.
        let threshold = 0.4;
        let mut expected = Vec::new();
        for i in 0..vectors.len() {
            for j in (i + 1)..vectors.len() {
                let score = naive_cosine(&vectors[i], &vectors[j]);
                if score >= threshold {
                    expected.push((format!("/s/{i:04}.py"), format!("/s/{j:04}.py"), score));
                }
            }
        }
        assert!(
            !expected.is_empty() && expected.len() < 60 * 59 / 2,
            "a meaningful threshold"
        );
        let pairs = sidecar.near_duplicates(threshold);
        assert_eq!(pairs.len(), expected.len());
        for pair in &pairs {
            let (_, _, score) = expected
                .iter()
                .find(|(a, b, _)| *a == pair.a && *b == pair.b)
                .unwrap_or_else(|| panic!("unexpected pair {pair:?}"));
            assert!((score - pair.score).abs() < 1e-5);
        }
        assert!(pairs.windows(2).all(|w| w[0].score >= w[1].score));

        // Nearest neighbours of one script, and the outlier check.
        let nearest = sidecar.nearest("/s/0007.py", 5).unwrap();
        let mut reference: Vec<(usize, f32)> = (0..vectors.len())
            .filter(|&i| i != 7)
            .map(|i| (i, naive_cosine(&vectors[7], &vectors[i])))
            .collect();
        reference.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (got, (index, score)) in nearest.iter().zip(&reference) {
            assert_eq!(got.logical_path, format!("/s/{index:04}.py"));
            assert!((got.score - score).abs() < 1e-5);
        }
        let outliers = sidecar.outliers(0.6);
        for outlier in &outliers {
            let i: usize = outlier.logical_path[3..7].parse().unwrap();
            let best = (0..vectors.len())
                .filter(|&j| j != i)
                .map(|j| naive_cosine(&vectors[i], &vectors[j]))
                .fold(f32::MIN, f32::max);
            assert!((best - outlier.score).abs() < 1e-5);
            assert!(best < 0.6);
        }
        let expected_outliers = (0..vectors.len())
            .filter(|&i| {
                (0..vectors.len())
                    .filter(|&j| j != i)
                    .map(|j| naive_cosine(&vectors[i], &vectors[j]))
                    .fold(f32::MIN, f32::max)
                    < 0.6
            })
            .count();
        assert_eq!(outliers.len(), expected_outliers);
    }

    #[test]
    fn lookups_by_path_use_the_index() {
        let dir = TempDir::new().unwrap();
        let (sidecar, _) = random_sidecar(&dir, 5, 4);
        assert!(sidecar.contains("/s/0003.py"));
        assert!(!sidecar.contains("/s/9999.py"));
        assert!(sidecar.nearest("/s/9999.py", 3).is_none());
        assert_eq!(sidecar.nearest("/s/0000.py", 100).unwrap().len(), 4);
    }

    #[test]
    fn vectors_are_normalised_once_at_load() {
        let v = normalised(vec![3.0, 4.0]);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert_eq!(normalised(vec![0.0, 0.0]), vec![0.0, 0.0]);
        assert_eq!(normalised(vec![f32::NAN, 1.0]), vec![0.0, 0.0]);
        assert!(normalised(Vec::new()).is_empty());
    }
}
