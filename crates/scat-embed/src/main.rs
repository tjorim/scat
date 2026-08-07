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

use std::path::PathBuf;

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

/// Writes a fresh embeddings sidecar, replacing any file already at `path`.
fn write_embeddings(
    path: &PathBuf,
    rows: &[ScriptRow],
    embeddings: &[Vec<f32>],
    model_label: &str,
) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove existing {}", path.display()))?;
    }
    let mut conn =
        Connection::open(path).with_context(|| format!("failed to create {}", path.display()))?;

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

    Ok(())
}
