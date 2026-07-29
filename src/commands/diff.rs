use anyhow::{Context, Result};
use scat_core::core::diff::{
    ScriptDiffResult, diff_catalog_vs_checkout, diff_catalog_vs_file, diff_files, render_diff_text,
};
use scat_core::core::search::{CatalogDiff, compare_catalogs};

use crate::output::print_json;

/// `scat diff /catalog/foo.py` or `scat diff /catalog/foo.py --against <file>`
pub(crate) fn cmd_script_diff_catalog(
    api: &scat_core::core::search::SearchApi,
    logical_path: &str,
    against: Option<&std::path::Path>,
    json: bool,
) -> Result<()> {
    let result = if let Some(file) = against {
        diff_catalog_vs_file(&api.conn, logical_path, file)
    } else {
        diff_catalog_vs_checkout(&api.conn, logical_path)
    }
    .with_context(|| format!("Script diff failed for '{logical_path}'"))?;

    print_script_diff(&result, json)
}

/// `scat diff --old <file> --new <file>`
pub(crate) fn cmd_script_diff_explicit(
    old: &std::path::Path,
    new: &std::path::Path,
    json: bool,
) -> Result<()> {
    let result =
        diff_files(old, new).with_context(|| "Script diff failed for explicit file pair")?;
    print_script_diff(&result, json)
}

fn print_script_diff(result: &ScriptDiffResult, json: bool) -> Result<()> {
    if json {
        print_json(result);
    } else {
        print!("{}", render_diff_text(result));
    }
    Ok(())
}

pub(crate) fn cmd_diff(
    db_path: &std::path::Path,
    against: Option<std::path::PathBuf>,
    old: Option<std::path::PathBuf>,
    new: Option<std::path::PathBuf>,
    json: bool,
) -> Result<()> {
    if against.is_some() && (old.is_some() || new.is_some()) {
        anyhow::bail!("Cannot combine --against with --old/--new.");
    }
    if old.is_some() != new.is_some() {
        anyhow::bail!("Provide both --old and --new together.");
    }

    let (old_db, new_db) = match (old, new) {
        (Some(old), Some(new)) => (old, new),
        _ => {
            let new_db = db_path.to_path_buf();
            let old_db = against
                .unwrap_or_else(|| std::path::PathBuf::from(format!("{}.1", db_path.display())));
            if !old_db.exists() {
                anyhow::bail!(
                    "No previous snapshot found at {}. Run index again or pass --against / --old and --new.",
                    old_db.display()
                );
            }
            (old_db, new_db)
        }
    };

    let diff = compare_catalogs(&old_db, &new_db)?;
    if json {
        print_json(&diff);
    } else {
        print_catalog_diff(&diff);
    }
    Ok(())
}

fn print_catalog_diff(diff: &CatalogDiff) {
    println!("Added ({})", diff.added.len());
    for row in &diff.added {
        println!(
            "  + {}  {}  {}",
            row.logical_path,
            json_display(&row.language),
            json_display(&row.owner)
        );
    }

    println!("\nRemoved ({})", diff.removed.len());
    for row in &diff.removed {
        println!(
            "  - {}  {}  {}",
            row.logical_path,
            json_display(&row.language),
            json_display(&row.owner)
        );
    }

    println!("\nChanged ({})", diff.changed.len());
    for row in &diff.changed {
        println!("  ~ {}", row.logical_path);
        for (field, values) in &row.fields {
            println!(
                "      {field}: {}  ->  {}",
                json_display(&values[0]),
                json_display(&values[1])
            );
        }
        if !row.deps_added.is_empty() {
            println!("      deps added:   {}", row.deps_added.join(", "));
        }
        if !row.deps_removed.is_empty() {
            println!("      deps removed: {}", row.deps_removed.join(", "));
        }
        println!();
    }
}

fn json_display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "-".to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
