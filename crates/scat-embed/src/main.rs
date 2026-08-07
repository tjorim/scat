//! Generates semantic-search embeddings for a scat catalog.
//!
//! Reads a `scripts.sqlite` catalog (a plain read-only copy is fine — no
//! network share access needed) and writes a sidecar `embeddings.sqlite`
//! keyed by script id, meant to be published next to the catalog and
//! consumed read-only by scat's Linux clients. This crate is deliberately
//! kept out of the `scat` binary and its `scat_core` library, which stay
//! Unix-only: it's meant to run wherever is convenient (e.g. a laptop with
//! internet access), not on the RHEL fleet.
//!
//! First run downloads the chosen ONNX model (needs internet); it's then
//! cached under `./.fastembed_cache` (override with `FASTEMBED_CACHE_DIR`)
//! and later runs are offline.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use rusqlite::{Connection, OpenFlags};

#[derive(Parser)]
struct Args {
    /// Path to the scripts.sqlite catalog to read.
    #[arg(long)]
    input: PathBuf,

    /// Path to write the embeddings sidecar to. Overwritten if it exists.
    #[arg(long)]
    output: PathBuf,

    /// Embedding model to use.
    #[arg(long, value_enum, default_value_t = ModelChoice::JinaCode)]
    model: ModelChoice,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ModelChoice {
    JinaCode,
    BgeSmall,
    BgeBase,
    Nomic,
}

impl ModelChoice {
    fn embedding_model(self) -> EmbeddingModel {
        match self {
            Self::JinaCode => EmbeddingModel::JinaEmbeddingsV2BaseCode,
            Self::BgeSmall => EmbeddingModel::BGESmallENV15,
            Self::BgeBase => EmbeddingModel::BGEBaseENV15,
            Self::Nomic => EmbeddingModel::NomicEmbedTextV15,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::JinaCode => "jina-embeddings-v2-base-code",
            Self::BgeSmall => "bge-small-en-v1.5",
            Self::BgeBase => "bge-base-en-v1.5",
            Self::Nomic => "nomic-embed-text-v1.5",
        }
    }
}

struct ScriptRow {
    id: i64,
    logical_path: String,
    text: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let rows = read_scripts(&args.input)?;
    if rows.is_empty() {
        println!("no scripts found in {}", args.input.display());
        return Ok(());
    }

    println!(
        "embedding {} scripts with {}...",
        rows.len(),
        args.model.label()
    );
    let mut model = TextEmbedding::try_new(
        TextInitOptions::new(args.model.embedding_model()).with_show_download_progress(true),
    )
    .context("failed to load embedding model")?;

    let documents: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
    let embeddings = model
        .embed(documents, None)
        .context("embedding generation failed")?;

    write_embeddings(&args.output, &rows, &embeddings, args.model.label())?;
    println!(
        "wrote {} embeddings to {}",
        rows.len(),
        args.output.display()
    );
    Ok(())
}

/// Reads the fields worth embedding out of a read-only `scripts.sqlite` copy.
fn read_scripts(path: &PathBuf) -> Result<Vec<ScriptRow>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open {}", path.display()))?;

    let mut stmt = conn.prepare(
        "SELECT id, logical_path, COALESCE(purpose, ''), COALESCE(tags, ''), COALESCE(owner, '') \
         FROM scripts",
    )?;

    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let logical_path: String = row.get(1)?;
            let purpose: String = row.get(2)?;
            let tags: String = row.get(3)?;
            let owner: String = row.get(4)?;

            let text = format!("{logical_path}\n{purpose}\ntags: {tags}\nowner: {owner}");
            Ok(ScriptRow {
                id,
                logical_path,
                text,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(rows)
}

/// Writes a fresh embeddings sidecar and atomically publishes it to `path`.
///
/// The database is built entirely under a scratch path beside `path` — schema
/// and data both — and only made visible at `path` via a single `rename(2)`
/// once it is complete. Without that, a plain `Connection::open(path)`
/// followed by `CREATE TABLE` and a separate insert transaction commits the
/// (empty) table before the data, so a reader on the share that opens `path`
/// mid-write could observe a `script_embeddings` table with zero rows instead
/// of either the previous complete sidecar or the new one.
fn write_embeddings(
    path: &PathBuf,
    rows: &[ScriptRow],
    embeddings: &[Vec<f32>],
    model_label: &str,
) -> Result<()> {
    let tmp_path = scratch_path(path);
    // A leftover from a previous crashed run; SQLite would otherwise append
    // to it instead of starting fresh.
    let _ = std::fs::remove_file(&tmp_path);

    let write_result = (|| -> Result<()> {
        let mut conn = Connection::open(&tmp_path)
            .with_context(|| format!("failed to create {}", tmp_path.display()))?;

        conn.execute_batch(
            "CREATE TABLE script_embeddings (
                script_id     INTEGER PRIMARY KEY,
                logical_path  TEXT    NOT NULL,
                model         TEXT    NOT NULL,
                dim           INTEGER NOT NULL,
                embedding     BLOB    NOT NULL
            );",
        )?;

        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO script_embeddings (script_id, logical_path, model, dim, embedding) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (row, embedding) in rows.iter().zip(embeddings) {
                let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
                stmt.execute(rusqlite::params![
                    row.id,
                    row.logical_path,
                    model_label,
                    i64::try_from(embedding.len()).unwrap_or(i64::MAX),
                    bytes,
                ])?;
            }
        }
        tx.commit()?;
        drop(conn);
        Ok(())
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        return write_result;
    }

    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to publish {} to {}",
            tmp_path.display(),
            path.display()
        )
    })
}

/// A scratch path beside `path`, used to build the sidecar before it is
/// atomically renamed into place.
fn scratch_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|n| format!("{}.tmp", n.to_string_lossy()))
        .unwrap_or_else(|| "embeddings.sqlite.tmp".to_string());
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_row(id: i64, logical_path: &str) -> ScriptRow {
        ScriptRow {
            id,
            logical_path: logical_path.to_string(),
            text: String::new(),
        }
    }

    #[test]
    fn write_embeddings_leaves_no_scratch_file_behind() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        let rows = vec![sample_row(1, "/catalog/scripts/a.py")];
        let embeddings = vec![vec![0.1_f32, 0.2, 0.3]];

        write_embeddings(&output, &rows, &embeddings, "test-model").unwrap();

        assert!(output.is_file());
        assert!(!scratch_path(&output).exists());

        let conn = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM script_embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn republish_replaces_the_existing_sidecar_without_a_visible_empty_table() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");

        write_embeddings(
            &output,
            &[sample_row(1, "/catalog/scripts/a.py")],
            &[vec![0.1_f32]],
            "test-model",
        )
        .unwrap();

        // Republishing must never leave `output` pointing at an empty table:
        // the old complete file stays visible right up until the rename that
        // swaps in the new complete one.
        write_embeddings(
            &output,
            &[
                sample_row(1, "/catalog/scripts/a.py"),
                sample_row(2, "/catalog/scripts/b.py"),
            ],
            &[vec![0.1_f32], vec![0.2_f32]],
            "test-model",
        )
        .unwrap();

        let conn = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM script_embeddings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
        assert!(!scratch_path(&output).exists());
    }

    #[test]
    fn leftover_scratch_file_from_a_crashed_run_does_not_break_the_next_write() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        std::fs::write(scratch_path(&output), b"partial garbage from a killed run").unwrap();

        write_embeddings(
            &output,
            &[sample_row(1, "/catalog/scripts/a.py")],
            &[vec![0.1_f32]],
            "test-model",
        )
        .unwrap();

        assert!(output.is_file());
        assert!(!scratch_path(&output).exists());
    }
}
