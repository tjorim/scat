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
    pub(crate) declares: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) lang: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) tag: Option<String>,
    pub(crate) limit: usize,
    pub(crate) fields: &'a [String],
    pub(crate) output: SearchOutput,
    pub(crate) no_color: bool,
}

/// Run the query described by `opts` and return the ranked rows, exactly as
/// `scat search` would print them. Shared with the parity tests that compare
/// the TUI's search worker against the CLI on one fixture.
pub fn collect_results(
    api: &scat_core::core::search::SearchApi,
    opts: &SearchOpts<'_>,
) -> Result<Vec<scat_core::core::db::JsonRow>> {
    let SearchOpts {
        text,
        regex,
        function,
        declares,
        role,
        lang,
        owner,
        tag,
        limit,
        ..
    } = opts;
    let limit = *limit;
    scat_core::core::db::validate_search_limit(limit)?;

    let (results, is_fts) = if let Some(file) = declares {
        let rows = api
            .search_scripts_by_declared_file_with_filters(
                file,
                role.as_deref(),
                limit,
                lang.as_deref(),
                owner.as_deref(),
                tag.as_deref(),
            )
            .with_context(|| format!("Declared-file search failed for {file:?}"))?;
        (rows, false)
    } else if let Some(name) = function {
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
    } else if let Some(pattern) = regex {
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
    } else if let Some(q) = text {
        (
            search_text(
                api,
                q,
                q,
                limit,
                lang.as_deref(),
                owner.as_deref(),
                tag.as_deref(),
            )?,
            false,
        )
    } else {
        (
            api.list_scripts(lang.as_deref(), owner.as_deref(), tag.as_deref(), limit, 0)?,
            false,
        )
    };

    Ok(if is_fts {
        sort_by_name_relevance(results, text.as_deref().unwrap_or(""))
    } else {
        results
    })
}

pub fn cmd_search(api: &scat_core::core::search::SearchApi, opts: SearchOpts<'_>) -> Result<()> {
    let ranked = collect_results(api, &opts)?;
    let SearchOpts {
        fields,
        output,
        no_color,
        ..
    } = opts;

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

/// Whether `query` looks like a (partial) path or filename — it contains a
/// path separator (`/`, or `\` for Windows-style fragments) or a dot.
/// Such queries get the INSTR path search *in addition to* full-text search;
/// they are never FTS-exempt, because dotted text (`10.0.0.1`, `config.yaml`
/// mentioned inside a script) is just as likely to be a content search.
pub fn query_looks_like_path(query: &str) -> bool {
    query.contains('/') || query.contains('\\') || query.contains('.')
}

/// Run a free-text query. Plain words use FTS (re-ranked so name matches come
/// first). Path-like queries (see [`query_looks_like_path`]) run the INSTR
/// path search first, then append FTS hits on the same text — FTS5's
/// tokenizer splits on punctuation, so `"10.0.0.1"` is matched as the phrase
/// `10 0 0 1` — skipping scripts already found by path, up to `limit`.
///
/// `fts_query` is the text handed to FTS; the TUI passes a live-typing
/// variant with a trailing prefix `*`, the CLI passes the query unchanged.
pub fn search_text(
    api: &scat_core::core::search::SearchApi,
    query: &str,
    fts_query: &str,
    limit: usize,
    lang: Option<&str>,
    owner: Option<&str>,
    tag: Option<&str>,
) -> Result<Vec<scat_core::core::db::JsonRow>> {
    if !query_looks_like_path(query) {
        let rows = api.search_with_filters(fts_query, limit, lang, owner, tag)?;
        // Raw BM25 order buries the script actually named like the query
        // under its own retained versions and near-namesakes.
        return Ok(sort_by_name_relevance(rows, query));
    }
    // The INSTR path search matches `/`-separated logical paths, so
    // normalise Windows separators from the query first.
    let path_query = query.replace('\\', "/");
    let mut rows = api.search_by_path_with_filters(&path_query, limit, lang, owner, tag)?;
    if rows.len() < limit {
        let seen: std::collections::HashSet<String> = rows
            .iter()
            .map(|r| ScriptView::new(r).logical_path().to_owned())
            .collect();
        let content_rows = api.search_with_filters(fts_query, limit, lang, owner, tag)?;
        rows.extend(
            content_rows
                .into_iter()
                .filter(|r| !seen.contains(ScriptView::new(r).logical_path()))
                .take(limit - rows.len()),
        );
    }
    Ok(rows)
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
