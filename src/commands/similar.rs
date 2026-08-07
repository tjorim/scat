use std::path::Path;

use anyhow::Result;
use scat_core::core::embeddings::EmbeddingsSidecar;
use scat_core::core::search::SearchApi;

use crate::cli::OutputFormat;
use crate::output::{print_json, render_table};

pub fn cmd_similar(
    api: &SearchApi,
    sidecar_path: &Path,
    path: &str,
    limit: usize,
    output: OutputFormat,
) -> Result<()> {
    if api.get_script(path)?.is_none() {
        anyhow::bail!("script '{path}' not found in catalog");
    }

    let Some(sidecar) = EmbeddingsSidecar::open(sidecar_path) else {
        anyhow::bail!(
            "no embeddings sidecar found at {} — generate one with scat-embed \
             (see crates/scat-embed) and publish it there to use `scat similar`",
            sidecar_path.display()
        );
    };

    let Some(results) = sidecar.nearest(path, limit) else {
        anyhow::bail!(
            "'{path}' has no stored embedding in {} — it may have been added \
             or changed since the embeddings sidecar was last built",
            sidecar_path.display()
        );
    };

    let total_scripts = api.stats()?.total_scripts;
    if (sidecar.len() as i64) < total_scripts {
        eprintln!(
            "note: embeddings sidecar ({}) covers {}/{total_scripts} cataloged scripts; \
             results may miss scripts added since the last scat-embed run",
            sidecar.model(),
            sidecar.len(),
        );
    }

    if output == OutputFormat::Json {
        print_json(&results);
        return Ok(());
    }

    if results.is_empty() {
        println!("No similar scripts found.");
        return Ok(());
    }

    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|r| vec![r.logical_path.clone(), format!("{:.3}", r.score)])
        .collect();
    println!("{}", render_table(&["Path", "Score"], &rows, false));
    Ok(())
}
