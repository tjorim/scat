use std::path::Path;

use anyhow::Result;
use scat_core::core::embeddings::{EmbeddingsSidecar, SimilarScript};
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

    // Rank every embedded script, not just the top `limit`: the sidecar can
    // outlive the catalog rows it was built from (a script renamed or
    // removed since the last scat-embed run still has a stored embedding),
    // and filtering those out has to happen *before* truncating to `limit`
    // — otherwise a stale entry ranked ahead of a real one silently costs a
    // valid result instead of just being skipped over. `usize::MAX` is a
    // plain "don't truncate" here: `nearest` never returns more rows than
    // the sidecar has regardless of what's asked for.
    let Some(all_ranked) = sidecar.nearest(path, usize::MAX) else {
        anyhow::bail!(
            "'{path}' has no stored embedding in {} — it may have been added \
             or changed since the embeddings sidecar was last built",
            sidecar_path.display()
        );
    };
    let results = drop_stale_and_truncate(api, all_ranked, limit);

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

/// Drop ranked results for scripts no longer in the catalog, then take the
/// first `limit`. Split out from [`cmd_similar`] so the "filter before
/// truncating" ordering — the reason a stale entry can only ever cost a
/// stale result, never a valid one further down the ranking — is directly
/// testable without capturing stdout.
fn drop_stale_and_truncate(
    api: &SearchApi,
    mut ranked: Vec<SimilarScript>,
    limit: usize,
) -> Vec<SimilarScript> {
    ranked.retain(|r| api.get_script(&r.logical_path).ok().flatten().is_some());
    ranked.truncate(limit);
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;
    use scat_core::core::db::create_db;

    fn make_api_with_scripts(paths: &[&str]) -> (SearchApi, tempfile::NamedTempFile) {
        let file = tempfile::NamedTempFile::new().unwrap();
        let conn = create_db(file.path()).unwrap();
        for path in paths {
            conn.execute(
                "INSERT INTO scripts (logical_path, language) VALUES (?1, 'python')",
                rusqlite::params![path],
            )
            .unwrap();
        }
        (SearchApi::new(conn), file)
    }

    fn scored(logical_path: &str, score: f32) -> SimilarScript {
        SimilarScript {
            logical_path: logical_path.to_string(),
            score,
        }
    }

    #[test]
    fn drop_stale_and_truncate_skips_removed_scripts_without_costing_a_valid_result() {
        let (api, _file) = make_api_with_scripts(&["/catalog/scripts/still-here.py"]);
        let ranked = vec![
            // Ranked ahead of the still-valid result, but no longer in the catalog.
            scored("/catalog/scripts/deleted.py", 0.99),
            scored("/catalog/scripts/still-here.py", 0.9),
        ];

        let results = drop_stale_and_truncate(&api, ranked, 1);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].logical_path, "/catalog/scripts/still-here.py");
    }

    #[test]
    fn drop_stale_and_truncate_respects_the_limit_after_filtering() {
        let (api, _file) =
            make_api_with_scripts(&["/catalog/scripts/a.py", "/catalog/scripts/b.py"]);
        let ranked = vec![
            scored("/catalog/scripts/a.py", 0.9),
            scored("/catalog/scripts/b.py", 0.8),
        ];

        assert_eq!(drop_stale_and_truncate(&api, ranked, 1).len(), 1);
    }

    #[test]
    fn drop_stale_and_truncate_zero_limit_returns_empty() {
        let (api, _file) = make_api_with_scripts(&["/catalog/scripts/a.py"]);
        let ranked = vec![scored("/catalog/scripts/a.py", 0.9)];

        assert!(drop_stale_and_truncate(&api, ranked, 0).is_empty());
    }
}
