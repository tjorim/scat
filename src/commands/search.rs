use anyhow::{Context, Result};
use scat_core::core::script_view::{ScriptView, symlink_target_display};

use crate::cli::SearchOutput;
use crate::output::{
    print_json, print_script_table_with_fields, render_script_csv, script_rows_to_json,
    selected_script_fields,
};

pub struct SearchOpts<'a> {
    pub(crate) text: Option<String>,
    pub(crate) regex: Option<String>,
    pub(crate) function: Option<String>,
    pub(crate) lang: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) limit: usize,
    pub(crate) fields: &'a [String],
    pub(crate) output: SearchOutput,
    pub(crate) no_color: bool,
}

pub fn cmd_search(api: &scat_core::core::search::SearchApi, opts: SearchOpts<'_>) -> Result<()> {
    let SearchOpts {
        text,
        regex,
        function,
        lang,
        owner,
        tag,
        limit,
        fields,
        output,
        no_color,
    } = opts;

    let (results, is_fts) = if let Some(ref name) = function {
        let rows = api
            .search_scripts_by_function_with_filters(
                name,
                limit,
                lang.as_deref(),
                owner.as_deref(),
                tag.as_deref(),
            )
            .with_context(|| format!("Function search failed for {name:?}"))?;
        (rows, false)
    } else if let Some(ref pattern) = regex {
        let rows = api
            .search_by_regex_with_filters(
                pattern,
                limit,
                lang.as_deref(),
                owner.as_deref(),
                tag.as_deref(),
            )
            .with_context(|| format!("Regex search failed for pattern {pattern:?}"))?;
        (rows, false)
    } else if let Some(ref q) = text {
        let use_fts = query_uses_fts(q);
        let rows = if use_fts {
            api.search_with_filters(q, limit, lang.as_deref(), owner.as_deref(), tag.as_deref())?
        } else {
            // The INSTR path search matches `/`-separated logical paths, so
            // normalise Windows separators from the query first.
            let path_query = q.replace('\\', "/");
            api.search_by_path_with_filters(
                &path_query,
                limit,
                lang.as_deref(),
                owner.as_deref(),
                tag.as_deref(),
            )?
        };
        (rows, use_fts)
    } else {
        (
            api.list_scripts(lang.as_deref(), owner.as_deref(), tag.as_deref(), limit, 0)?,
            false,
        )
    };

    let ranked = if is_fts {
        sort_by_name_relevance(results, text.as_deref().unwrap_or(""))
    } else {
        results
    };

    if output == SearchOutput::Json {
        print_json(&script_rows_to_json(&ranked, fields));
        return Ok(());
    }

    if output == SearchOutput::Csv {
        print!("{}", render_script_csv(&ranked, fields));
        return Ok(());
    }

    if ranked.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    let selected_fields = selected_script_fields(fields);
    print_script_table_with_fields(&group_by_symlinks(ranked), &selected_fields, no_color);
    Ok(())
}

/// Route a text query: FTS for plain words, INSTR-based path search when the
/// query looks like a (partial) path, since `/` and `.` are FTS5 syntax.
/// Backslashes count as path separators so Windows-style path fragments
/// (`scripts\foo`) route to path search too.
pub fn query_uses_fts(query: &str) -> bool {
    !query.contains('/') && !query.contains('\\') && !query.contains('.')
}

/// Re-sort results so exact and prefix filename matches appear first.
/// Preserves the original (BM25) order within each tier.
pub fn sort_by_name_relevance(
    mut results: Vec<scat_core::core::db::JsonRow>,
    query: &str,
) -> Vec<scat_core::core::db::JsonRow> {
    let q = query.to_lowercase();
    results.sort_by_cached_key(|row| {
        let path = ScriptView::new(row).logical_path();
        // Split on both separators so basenames resolve correctly even if a
        // path carries Windows-style backslashes.
        let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let stem = basename.rsplit_once('.').map_or(basename, |(s, _)| s);
        let stem_lower = stem.to_lowercase();
        let base_lower = basename.to_lowercase();
        if stem_lower == q {
            0 // exact stem match  (checkmc.py for query "checkmc")
        } else if base_lower == q {
            1 // exact basename    (checkmc for query "checkmc")
        } else if stem_lower.starts_with(&q) {
            2 // prefix stem match (checkmc_v2.py for query "checkmc")
        } else if base_lower.contains(&q) {
            3 // anywhere in basename
        } else {
            4 // only in path/content/metadata
        }
    });
    results
}

/// Group symlinked scripts with their targets: each symlink stays a primary
/// table row and gains a `↳` sub-row showing its target path.
///
/// If the target also appears in the result set it is suppressed as a
/// standalone entry — it is already visible via the ↳ row.  If the target is
/// not in the result set the symlink still appears as primary with a ↳ row
/// pointing at the (absent) target.
fn group_by_symlinks(
    results: Vec<scat_core::core::db::JsonRow>,
) -> Vec<scat_core::core::db::JsonRow> {
    use std::collections::HashSet;

    // Collect target paths covered by symlinks present in these results.
    let covered_targets: HashSet<String> = results
        .iter()
        .map(ScriptView::new)
        .map(|view| view.symlink_target())
        .filter(|target| !target.is_empty())
        .map(str::to_string)
        .collect();

    let mut output: Vec<scat_core::core::db::JsonRow> = Vec::new();

    for row in results {
        let target = {
            let target = ScriptView::new(&row).symlink_target();
            (!target.is_empty()).then(|| target.to_string())
        };

        if let Some(target_path) = target {
            // Symlink is the primary entry; show its target as a sub-row.
            // The path column truncates from the left to keep filenames
            // visible, which would eat a leading marker, so the sub-row is
            // written as a short sibling name wherever it can be — the arrow
            // is what makes the row mean anything.
            let shown =
                symlink_target_display(ScriptView::new(&row).logical_path(), target_path.as_str());
            let sub_row = format!("  ↳ {shown}");
            output.push(row);
            let mut sub = scat_core::core::db::JsonRow::new();
            sub.insert("logical_path".into(), sub_row.into());
            output.push(sub);
        } else {
            // Non-symlink: skip if it's already shown as a ↳ sub-row above.
            if !covered_targets.contains(ScriptView::new(&row).logical_path()) {
                output.push(row);
            }
        }
    }

    output
}
