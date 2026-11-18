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
//!
//! Runs are incremental. Each embedded row records a hash of the exact text
//! that was embedded; a later run reuses the vector of every script whose path,
//! text and model are unchanged (from `--previous`, by default the existing
//! `--output`) and only embeds the rest. The sidecar is built batch by batch in
//! a scratch file beside `--output`, which a killed run leaves behind and the
//! next run resumes from.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

#[derive(Parser)]
struct Args {
    /// Path to the scripts.sqlite catalog to read.
    #[arg(long)]
    input: PathBuf,

    /// Path to write the embeddings sidecar to. Replaced atomically if it exists.
    #[arg(long)]
    output: PathBuf,

    /// An earlier sidecar to reuse vectors from (default: the existing
    /// `--output`). A script is reused when its path, embedded text and model
    /// are unchanged.
    #[arg(long)]
    previous: Option<PathBuf>,

    /// Also embed data files (csv, text and lst scripts, of which the catalog
    /// keeps only a leading sample). They are skipped by default: the sample
    /// says little about a data file and the vectors add noise and cost to
    /// `similar`, `near-duplicates` and `outliers`.
    #[arg(long)]
    include_data_files: bool,

    /// Scripts embedded per batch; each batch is committed to the scratch
    /// file, so a crash loses at most one.
    #[arg(long, default_value_t = 64)]
    batch_size: usize,

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

/// Layout of the sidecar this tool writes. Must equal
/// `scat_core::core::embeddings::SIDECAR_FORMAT_VERSION` (a root-crate test
/// checks it; this crate cannot depend on `scat_core`, which is Unix-only).
/// Bump it only for a layout older scat clients cannot read — see
/// `docs/COMPATIBILITY.md`.
const SIDECAR_FORMAT_VERSION: i64 = 1;

/// Languages whose catalog `content` is only a leading sample (data files).
/// Must equal `scat_core::core::db::SAMPLED_CONTENT_LANGUAGES` (a root-crate
/// test checks it, as for the format version above).
const SAMPLED_CONTENT_LANGUAGES: [&str; 3] = ["csv", "text", "lst"];

struct ScriptRow {
    id: i64,
    logical_path: String,
    language: String,
    text: String,
}

impl ScriptRow {
    /// Hash of the exact text that gets embedded. It covers the path, purpose,
    /// tags, owner and (truncated) content, so a change to any of them — not
    /// just to the file body — invalidates the stored vector.
    fn text_hash(&self) -> String {
        let digest = Sha256::digest(self.text.as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Character budget for a script's `content` in the embedded text.
///
/// Bounds memory and embedding time for oversized scripts; it isn't there to
/// avoid a model error; a tokenizer truncates to its own max sequence length
/// regardless, so content beyond that is already dropped from the model's
/// point of view. This budget keeps that truncation from happening deep
/// inside `fastembed` on the full (possibly huge) script body instead.
///
/// Sized against the default model's window rather than the smallest one:
/// `jina-embeddings-v2-base-code` (the default) and `nomic-embed-text-v1.5`
/// have an 8192-token context, code tokenizes to roughly 2-3 chars/token, so
/// ~20000 chars stays under that for the great majority of scripts without
/// leaving headroom unused. The BGE models (`--model bge-small`/`bge-base`)
/// have a much smaller 512-token window and will truncate well before this
/// regardless — that's the tokenizer's own truncation, not something this
/// budget can avoid, so it isn't worth sizing this constant down for them.
const MAX_CONTENT_CHARS: usize = 20_000;

fn main() -> Result<()> {
    let args = Args::parse();

    let mut rows = read_scripts(&args.input)?;
    let catalog_build = read_catalog_build(&args.input)?;
    let skipped = skip_data_files(&mut rows, args.include_data_files);
    if skipped > 0 {
        println!(
            "skipping {skipped} data files (csv/text/lst); pass --include-data-files to embed them"
        );
    }
    if rows.is_empty() {
        println!("no scripts to embed in {}", args.input.display());
        return Ok(());
    }

    let previous = args.previous.as_deref().unwrap_or(&args.output);
    let mut model = LazyModel::new(args.model);
    let stats = build_sidecar(
        &args.output,
        &SidecarInputs {
            rows: &rows,
            previous,
            model_label: args.model.label(),
            catalog_build: catalog_build.as_deref(),
            batch_size: args.batch_size.max(1),
        },
        &mut |texts| model.embed(texts),
        &mut |done, total| println!("embedded {done}/{total} new scripts..."),
    )?;
    println!(
        "wrote {} embeddings to {} ({} reused, {} embedded)",
        stats.reused + stats.embedded,
        args.output.display(),
        stats.reused,
        stats.embedded,
    );
    Ok(())
}

/// Drop data files from `rows` unless `include` is set; returns how many went.
fn skip_data_files(rows: &mut Vec<ScriptRow>, include: bool) -> usize {
    if include {
        return 0;
    }
    let before = rows.len();
    rows.retain(|r| !SAMPLED_CONTENT_LANGUAGES.contains(&r.language.as_str()));
    before - rows.len()
}

/// The embedding model, loaded the first time something needs embedding — a
/// run that reuses every vector never loads (or downloads) it.
struct LazyModel {
    choice: ModelChoice,
    model: Option<TextEmbedding>,
}

impl LazyModel {
    fn new(choice: ModelChoice) -> Self {
        Self {
            choice,
            model: None,
        }
    }

    fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if self.model.is_none() {
            println!("loading {}...", self.choice.label());
            self.model = Some(
                TextEmbedding::try_new(
                    TextInitOptions::new(self.choice.embedding_model())
                        .with_show_download_progress(true),
                )
                .context("failed to load embedding model")?,
            );
        }
        self.model
            .as_mut()
            .expect("loaded above")
            .embed(texts, None)
            .context("embedding generation failed")
    }
}

/// Reads the fields worth embedding out of a read-only `scripts.sqlite` copy.
fn read_scripts(path: &PathBuf) -> Result<Vec<ScriptRow>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open {}", path.display()))?;

    let mut stmt = conn.prepare(
        "SELECT id, logical_path, COALESCE(purpose, ''), COALESCE(tags, ''), COALESCE(owner, ''), \
                COALESCE(content, ''), COALESCE(language, '') \
         FROM scripts \
         ORDER BY id",
    )?;

    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let logical_path: String = row.get(1)?;
            let purpose: String = row.get(2)?;
            let tags: String = row.get(3)?;
            let owner: String = row.get(4)?;
            let content: String = row.get(5)?;
            let language: String = row.get(6)?;

            // Content (what the script actually does) matters most for a
            // code-aware model, so it goes last: metadata first keeps the
            // path/purpose/tags/owner intact even if a downstream tokenizer
            // truncates from the end.
            let text = format!(
                "{logical_path}\n{purpose}\ntags: {tags}\nowner: {owner}\n\n{}",
                truncate_chars(&content, MAX_CONTENT_CHARS)
            );
            Ok(ScriptRow {
                id,
                logical_path,
                language,
                text,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(rows)
}

/// The input catalog's `index_metadata.build_timestamp`, recorded in the
/// sidecar so scat can tell which catalog build the embeddings describe.
/// `None` for an input without that table (a hand-made or very old catalog).
fn read_catalog_build(path: &PathBuf) -> Result<Option<String>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let has_table: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'index_metadata')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT build_timestamp FROM index_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .ok())
}

/// Truncate `s` to at most `max_chars` characters, respecting UTF-8
/// boundaries (a plain byte-length truncation could split a multi-byte
/// character and produce invalid UTF-8).
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Turns a batch of texts into one vector each.
type Embedder<'a> = dyn FnMut(&[&str]) -> Result<Vec<Vec<f32>>> + 'a;

/// What goes into a sidecar build besides the output path.
struct SidecarInputs<'a> {
    rows: &'a [ScriptRow],
    /// An earlier sidecar to reuse vectors from; may not exist.
    previous: &'a Path,
    model_label: &'a str,
    catalog_build: Option<&'a str>,
    batch_size: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct BuildStats {
    /// Scripts whose stored vector was reused.
    reused: usize,
    /// Scripts embedded by this run.
    embedded: usize,
}

/// A stored vector, as written: little-endian `f32`s.
struct StoredVector {
    dim: i64,
    bytes: Vec<u8>,
}

/// The vectors that can be reused, by `(logical_path, text_hash)`.
type Reusable = HashMap<(String, String), StoredVector>;

/// Vectors of `model` in the sidecar (or half-built scratch file) at `path`,
/// keyed by what they were computed from. Rows without a recorded hash (made by
/// an earlier scat-embed) and files that cannot be read yield nothing: they are
/// recomputed, never trusted.
fn load_reusable(path: &Path, model_label: &str) -> Reusable {
    let mut reusable = Reusable::new();
    let Ok(conn) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return reusable;
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT logical_path, text_hash, dim, embedding FROM script_embeddings \
         WHERE model = ?1 AND text_hash IS NOT NULL",
    ) else {
        return reusable;
    };
    let Ok(rows) = stmt.query_map([model_label], |row| {
        Ok((
            (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
            StoredVector {
                dim: row.get(2)?,
                bytes: row.get(3)?,
            },
        ))
    }) else {
        return reusable;
    };
    for (key, vector) in rows.flatten() {
        reusable.insert(key, vector);
    }
    reusable
}

fn insert_row(
    stmt: &mut rusqlite::Statement<'_>,
    row: &ScriptRow,
    model_label: &str,
    vector: &StoredVector,
) -> Result<()> {
    stmt.execute(rusqlite::params![
        row.id,
        row.logical_path,
        model_label,
        vector.dim,
        vector.bytes,
        row.text_hash(),
    ])?;
    Ok(())
}

/// Builds the sidecar for `inputs.rows` and atomically publishes it to `path`.
///
/// Vectors whose path, text and model match one in the previous sidecar — or in
/// the scratch file a killed run left behind — are copied; only the rest go to
/// `embed`, in batches, each committed to the scratch file as it completes (so
/// a crash costs at most one batch). `progress(done, total)` reports the
/// batches. The scratch file is the only thing written until the very end,
/// when it is renamed over `path`: a reader on the share sees the previous
/// complete sidecar or the new one, never an empty table.
fn build_sidecar(
    path: &Path,
    inputs: &SidecarInputs<'_>,
    embed: &mut Embedder<'_>,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<BuildStats> {
    let tmp_path = scratch_path(path);

    // Read everything reusable *before* the scratch file is recreated: it may
    // be the resume point of a killed run.
    let mut reusable = load_reusable(inputs.previous, inputs.model_label);
    for (key, vector) in load_reusable(&tmp_path, inputs.model_label) {
        reusable.entry(key).or_insert(vector);
    }
    let _ = std::fs::remove_file(&tmp_path);

    let write_result = (|| -> Result<BuildStats> {
        let mut conn = Connection::open(&tmp_path)
            .with_context(|| format!("failed to create {}", tmp_path.display()))?;
        conn.execute_batch(
            "CREATE TABLE script_embeddings (
                script_id     INTEGER PRIMARY KEY,
                logical_path  TEXT    NOT NULL,
                model         TEXT    NOT NULL,
                dim           INTEGER NOT NULL,
                embedding     BLOB    NOT NULL,
                text_hash     TEXT
            );",
        )?;

        let mut stats = BuildStats::default();
        let mut todo: Vec<&ScriptRow> = Vec::new();
        {
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO script_embeddings \
                     (script_id, logical_path, model, dim, embedding, text_hash) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;
                for row in inputs.rows {
                    match reusable.remove(&(row.logical_path.clone(), row.text_hash())) {
                        Some(vector) => {
                            insert_row(&mut stmt, row, inputs.model_label, &vector)?;
                            stats.reused += 1;
                        }
                        None => todo.push(row),
                    }
                }
            }
            tx.commit()?;
        }

        for batch in todo.chunks(inputs.batch_size) {
            let texts: Vec<&str> = batch.iter().map(|r| r.text.as_str()).collect();
            let vectors = embed(&texts)?;
            anyhow::ensure!(
                vectors.len() == batch.len(),
                "the model returned {} vectors for {} scripts",
                vectors.len(),
                batch.len()
            );
            let tx = conn.transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO script_embeddings \
                     (script_id, logical_path, model, dim, embedding, text_hash) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;
                for (row, vector) in batch.iter().zip(&vectors) {
                    let stored = StoredVector {
                        dim: i64::try_from(vector.len()).unwrap_or(i64::MAX),
                        bytes: vector.iter().flat_map(|f| f.to_le_bytes()).collect(),
                    };
                    insert_row(&mut stmt, row, inputs.model_label, &stored)?;
                }
            }
            tx.commit()?;
            stats.embedded += batch.len();
            progress(stats.embedded, todo.len());
        }

        // Only a finished sidecar carries its metadata: an interrupted scratch
        // file is a resume point, not something to publish.
        conn.execute_batch(
            "CREATE TABLE embedding_metadata (
                id                      INTEGER PRIMARY KEY CHECK (id = 1),
                format_version          INTEGER NOT NULL,
                catalog_build_timestamp TEXT
            );",
        )?;
        conn.execute(
            "INSERT INTO embedding_metadata (id, format_version, catalog_build_timestamp)
             VALUES (1, ?1, ?2)",
            rusqlite::params![SIDECAR_FORMAT_VERSION, inputs.catalog_build],
        )?;
        drop(conn);
        Ok(stats)
    })();

    // On failure the scratch file stays: its committed batches are what the
    // next run resumes from.
    let stats = write_result?;

    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to publish {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(stats)
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
        script(id, logical_path, "python", "")
    }

    fn script(id: i64, logical_path: &str, language: &str, text: &str) -> ScriptRow {
        ScriptRow {
            id,
            logical_path: logical_path.to_string(),
            language: language.to_string(),
            text: text.to_string(),
        }
    }

    /// Write a minimal `scripts.sqlite` catalog with one row.
    fn write_catalog(
        path: &PathBuf,
        logical_path: &str,
        purpose: &str,
        tags: &str,
        owner: &str,
        content: &str,
    ) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE scripts (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                logical_path  TEXT NOT NULL UNIQUE,
                purpose       TEXT,
                tags          TEXT,
                owner         TEXT,
                content       TEXT,
                language      TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, purpose, tags, owner, content)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![logical_path, purpose, tags, owner, content],
        )
        .unwrap();
    }

    #[test]
    fn truncate_chars_leaves_short_strings_untouched() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn truncate_chars_cuts_at_the_character_limit() {
        assert_eq!(truncate_chars("hello world", 5), "hello");
    }

    #[test]
    fn truncate_chars_respects_utf8_boundaries() {
        // Each "é" is a two-byte UTF-8 character; a byte-length truncation
        // at an odd offset would split one and produce invalid UTF-8.
        let s = "éééé";
        assert_eq!(truncate_chars(s, 2), "éé");
    }

    #[test]
    fn read_scripts_includes_content_after_metadata() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scripts.sqlite");
        write_catalog(
            &path,
            "/catalog/scripts/deploy.py",
            "Deploy the service",
            "deploy,prod",
            "alice",
            "def deploy():\n    run_deployment()",
        );

        let rows = read_scripts(&path).unwrap();
        assert_eq!(rows.len(), 1);
        let text = &rows[0].text;
        assert!(text.contains("/catalog/scripts/deploy.py"));
        assert!(text.contains("Deploy the service"));
        assert!(text.contains("tags: deploy,prod"));
        assert!(text.contains("owner: alice"));
        assert!(text.contains("def deploy():"));
        assert!(text.contains("run_deployment()"));
    }

    #[test]
    fn read_scripts_truncates_oversized_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scripts.sqlite");
        let huge_content = "x".repeat(MAX_CONTENT_CHARS + 5000);
        write_catalog(&path, "/catalog/scripts/big.py", "", "", "", &huge_content);

        let rows = read_scripts(&path).unwrap();
        // Truncated content plus the metadata scaffolding around it, well
        // under the untruncated original.
        assert!(rows[0].text.len() < huge_content.len());
        assert!(rows[0].text.len() <= MAX_CONTENT_CHARS + 200);
    }

    // -----------------------------------------------------------------------
    // Building the sidecar
    // -----------------------------------------------------------------------

    /// A deterministic stand-in for the model that records what it was asked
    /// to embed.
    #[derive(Default)]
    struct FakeModel {
        calls: Vec<Vec<String>>,
        /// Fail the n-th call (0-based), as a crashed run would.
        fail_on_call: Option<usize>,
    }

    impl FakeModel {
        fn embed(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            let n = self.calls.len();
            self.calls
                .push(texts.iter().map(ToString::to_string).collect());
            if self.fail_on_call == Some(n) {
                anyhow::bail!("simulated crash in batch {n}");
            }
            Ok(texts
                .iter()
                .map(|t| vec![t.len() as f32, 1.0, t.bytes().map(f32::from).sum::<f32>()])
                .collect())
        }

        fn embedded_texts(&self) -> Vec<&str> {
            self.calls.iter().flatten().map(String::as_str).collect()
        }
    }

    fn run(
        output: &Path,
        previous: &Path,
        rows: &[ScriptRow],
        model: &mut FakeModel,
        label: &str,
        batch_size: usize,
    ) -> Result<BuildStats> {
        build_sidecar(
            output,
            &SidecarInputs {
                rows,
                previous,
                model_label: label,
                catalog_build: Some("2026-06-01T00:00:00Z"),
                batch_size,
            },
            &mut |texts| model.embed(texts),
            &mut |_, _| {},
        )
    }

    fn sample_rows() -> Vec<ScriptRow> {
        vec![
            script(1, "/s/a.py", "python", "alpha"),
            script(2, "/s/b.py", "python", "bravo"),
            script(3, "/s/c.sh", "shell", "charlie"),
        ]
    }

    fn count_rows(path: &Path) -> i64 {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        conn.query_row("SELECT COUNT(*) FROM script_embeddings", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn first_run_embeds_everything_and_leaves_no_scratch_file() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        let mut model = FakeModel::default();

        let stats = run(&output, &output, &sample_rows(), &mut model, "m", 64).unwrap();
        assert_eq!(
            stats,
            BuildStats {
                reused: 0,
                embedded: 3
            }
        );
        assert_eq!(model.calls.len(), 1, "one batch");
        assert!(output.is_file());
        assert!(!scratch_path(&output).exists());
        assert_eq!(count_rows(&output), 3);

        let conn = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let hashes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM script_embeddings WHERE text_hash IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hashes, 3);
    }

    #[test]
    fn an_unchanged_catalog_reuses_every_vector_and_never_calls_the_model() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        run(
            &output,
            &output,
            &sample_rows(),
            &mut FakeModel::default(),
            "m",
            64,
        )
        .unwrap();
        let first = std::fs::read(&output).unwrap();
        assert!(!first.is_empty());

        let mut again = FakeModel::default();
        let stats = run(&output, &output, &sample_rows(), &mut again, "m", 64).unwrap();
        assert_eq!(
            stats,
            BuildStats {
                reused: 3,
                embedded: 0
            }
        );
        assert!(again.calls.is_empty());
        assert_eq!(count_rows(&output), 3);
    }

    #[test]
    fn only_changed_scripts_are_embedded_and_renumbered_ones_keep_their_vectors() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        run(
            &output,
            &output,
            &sample_rows(),
            &mut FakeModel::default(),
            "m",
            64,
        )
        .unwrap();

        // b.py's text changes, a new script appears, c.sh moves to another id
        // (a rebuilt catalog renumbers), and a.py is untouched.
        let rows = vec![
            script(1, "/s/a.py", "python", "alpha"),
            script(2, "/s/b.py", "python", "bravo, edited"),
            script(7, "/s/c.sh", "shell", "charlie"),
            script(8, "/s/d.py", "python", "delta"),
        ];
        let mut model = FakeModel::default();
        let stats = run(&output, &output, &rows, &mut model, "m", 64).unwrap();
        assert_eq!(
            stats,
            BuildStats {
                reused: 2,
                embedded: 2
            }
        );
        assert_eq!(model.embedded_texts(), ["bravo, edited", "delta"]);

        let conn = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let ids: Vec<i64> = conn
            .prepare("SELECT script_id FROM script_embeddings ORDER BY script_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(ids, [1, 2, 7, 8], "rows follow the current catalog's ids");
    }

    #[test]
    fn a_different_model_reuses_nothing() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        run(
            &output,
            &output,
            &sample_rows(),
            &mut FakeModel::default(),
            "old-model",
            64,
        )
        .unwrap();
        let mut model = FakeModel::default();
        let stats = run(
            &output,
            &output,
            &sample_rows(),
            &mut model,
            "new-model",
            64,
        )
        .unwrap();
        assert_eq!(
            stats,
            BuildStats {
                reused: 0,
                embedded: 3
            }
        );
    }

    #[test]
    fn previous_can_be_a_different_file_and_may_be_missing() {
        let dir = TempDir::new().unwrap();
        let published = dir.path().join("published.sqlite");
        run(
            &published,
            &published,
            &sample_rows(),
            &mut FakeModel::default(),
            "m",
            64,
        )
        .unwrap();

        let output = dir.path().join("new.sqlite");
        let mut model = FakeModel::default();
        let stats = run(&output, &published, &sample_rows(), &mut model, "m", 64).unwrap();
        assert_eq!(stats.reused, 3);
        assert!(model.calls.is_empty());

        let mut model = FakeModel::default();
        let stats = run(
            &dir.path().join("other.sqlite"),
            &dir.path().join("nope.sqlite"),
            &sample_rows(),
            &mut model,
            "m",
            64,
        )
        .unwrap();
        assert_eq!(stats.embedded, 3);
    }

    #[test]
    fn a_sidecar_without_recorded_hashes_is_recomputed_not_trusted() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        // What an earlier scat-embed wrote: no text_hash column.
        let conn = Connection::open(&output).unwrap();
        conn.execute_batch(
            "CREATE TABLE script_embeddings (
                script_id INTEGER PRIMARY KEY, logical_path TEXT NOT NULL,
                model TEXT NOT NULL, dim INTEGER NOT NULL, embedding BLOB NOT NULL);
             INSERT INTO script_embeddings VALUES (1, '/s/a.py', 'm', 1, x'00000000');",
        )
        .unwrap();
        drop(conn);

        let mut model = FakeModel::default();
        let stats = run(&output, &output, &sample_rows(), &mut model, "m", 64).unwrap();
        assert_eq!(
            stats,
            BuildStats {
                reused: 0,
                embedded: 3
            }
        );
        assert_eq!(count_rows(&output), 3, "the new sidecar replaced it");
    }

    #[test]
    fn a_crashed_run_resumes_from_its_committed_batches() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        let rows = vec![
            script(1, "/s/a.py", "python", "alpha"),
            script(2, "/s/b.py", "python", "bravo"),
            script(3, "/s/c.py", "python", "charlie"),
            script(4, "/s/d.py", "python", "delta"),
            script(5, "/s/e.py", "python", "echo"),
        ];

        // Batches of two; the second one dies.
        let mut crashing = FakeModel {
            fail_on_call: Some(1),
            ..FakeModel::default()
        };
        let err = run(&output, &output, &rows, &mut crashing, "m", 2).unwrap_err();
        assert!(err.to_string().contains("simulated crash"), "{err}");
        assert!(!output.exists(), "nothing is published by a failed run");
        let scratch = scratch_path(&output);
        assert_eq!(count_rows(&scratch), 2, "the first batch survived");

        // The rerun reuses it and embeds only the remaining three.
        let mut model = FakeModel::default();
        let stats = run(&output, &output, &rows, &mut model, "m", 2).unwrap();
        assert_eq!(
            stats,
            BuildStats {
                reused: 2,
                embedded: 3
            }
        );
        assert_eq!(model.embedded_texts(), ["charlie", "delta", "echo"]);
        assert_eq!(count_rows(&output), 5);
        assert!(!scratch.exists());
    }

    #[test]
    fn progress_is_reported_per_batch() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        let mut seen = Vec::new();
        let mut model = FakeModel::default();
        build_sidecar(
            &output,
            &SidecarInputs {
                rows: &sample_rows(),
                previous: &output,
                model_label: "m",
                catalog_build: None,
                batch_size: 2,
            },
            &mut |texts| model.embed(texts),
            &mut |done, total| seen.push((done, total)),
        )
        .unwrap();
        assert_eq!(seen, [(2, 3), (3, 3)]);
    }

    #[test]
    fn republish_replaces_the_existing_sidecar_without_a_visible_empty_table() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        run(
            &output,
            &output,
            &[sample_row(1, "/s/a.py")],
            &mut FakeModel::default(),
            "m",
            64,
        )
        .unwrap();

        // The old complete file stays visible right up until the rename.
        run(
            &output,
            &output,
            &[sample_row(1, "/s/a.py"), sample_row(2, "/s/b.py")],
            &mut FakeModel::default(),
            "m",
            64,
        )
        .unwrap();
        assert_eq!(count_rows(&output), 2);
        assert!(!scratch_path(&output).exists());
    }

    #[test]
    fn sidecar_records_its_format_and_the_catalog_build_it_describes() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        run(
            &output,
            &output,
            &[sample_row(1, "/s/a.py")],
            &mut FakeModel::default(),
            "m",
            64,
        )
        .unwrap();

        let conn = Connection::open_with_flags(&output, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let (format, build): (i64, Option<String>) = conn
            .query_row(
                "SELECT format_version, catalog_build_timestamp FROM embedding_metadata WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(format, SIDECAR_FORMAT_VERSION);
        assert_eq!(build.as_deref(), Some("2026-06-01T00:00:00Z"));
    }

    #[test]
    fn data_files_are_skipped_unless_asked_for() {
        let make = || {
            vec![
                script(1, "/s/a.py", "python", "x"),
                script(2, "/s/feed.csv", "csv", "x"),
                script(3, "/s/notes.txt", "text", "x"),
                script(4, "/s/hosts.lst", "lst", "x"),
                script(5, "/s/c.sh", "shell", "x"),
            ]
        };
        let mut rows = make();
        assert_eq!(skip_data_files(&mut rows, false), 3);
        let paths: Vec<&str> = rows.iter().map(|r| r.logical_path.as_str()).collect();
        assert_eq!(paths, ["/s/a.py", "/s/c.sh"]);

        let mut rows = make();
        assert_eq!(skip_data_files(&mut rows, true), 0);
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn the_text_hash_covers_everything_that_is_embedded() {
        let a = script(1, "/s/a.py", "python", "same text");
        let b = script(9, "/s/other.py", "shell", "same text");
        assert_eq!(a.text_hash(), b.text_hash(), "only the text counts");
        assert_ne!(
            a.text_hash(),
            script(1, "/s/a.py", "python", "same text!").text_hash()
        );
        assert_eq!(a.text_hash().len(), 64);
    }

    #[test]
    fn read_catalog_build_reads_index_metadata_when_present() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scripts.sqlite");
        write_catalog(&path, "/catalog/scripts/a.py", "", "", "", "");
        // A catalog with no index_metadata (hand-made, very old) has no build to record.
        assert_eq!(read_catalog_build(&path).unwrap(), None);

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE index_metadata (id INTEGER PRIMARY KEY, build_timestamp TEXT NOT NULL, schema_version INTEGER NOT NULL);
             INSERT INTO index_metadata VALUES (1, '2026-06-01T00:00:00Z', 14);",
        )
        .unwrap();
        assert_eq!(
            read_catalog_build(&path).unwrap().as_deref(),
            Some("2026-06-01T00:00:00Z")
        );
    }

    #[test]
    fn leftover_scratch_file_that_is_not_a_database_does_not_break_the_next_run() {
        let dir = TempDir::new().unwrap();
        let output = dir.path().join("embeddings.sqlite");
        std::fs::write(scratch_path(&output), b"partial garbage from a killed run").unwrap();

        run(
            &output,
            &output,
            &[sample_row(1, "/s/a.py")],
            &mut FakeModel::default(),
            "m",
            64,
        )
        .unwrap();

        assert!(output.is_file());
        assert!(!scratch_path(&output).exists());
    }
}
