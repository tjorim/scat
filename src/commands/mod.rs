use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};
use scat_core::core::diff::{
    ScriptDiffResult, diff_catalog_vs_checkout, diff_catalog_vs_file, diff_files, render_diff_text,
};
use scat_core::core::script_view::{ListField, ScriptView};
use scat_core::core::search::{CatalogDiff, compare_catalogs};
use scat_core::core::vc::{compare_revision_rows, relative_age};
use scat_core::indexer::builder::{BuildOptions, build_index};
use scat_core::indexer::scanner::max_mtime_in_roots_with_shutdown;
use tracing::warn;

use crate::cli::{OutputFormat, SearchOutput};
use crate::output::{
    canonicalize_row_keys, dash_or_empty, dep_entry_to_json, json_script_field, list_field_display,
    mtime_field, print_json, print_script_table, print_script_table_with_fields, render_script_csv,
    render_table, script_rows_to_json, selected_script_fields, size_field, str_field,
    used_by_row_to_json, warning_kinds,
};
use crate::runtime::audit_exit_code;

pub(crate) struct SearchOpts<'a> {
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

pub(crate) fn cmd_search(
    api: &scat_core::core::search::SearchApi,
    opts: SearchOpts<'_>,
) -> Result<()> {
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
            .with_context(|| format!("Function search failed for {:?}", name))?;
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
            .with_context(|| format!("Regex search failed for pattern {:?}", pattern))?;
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
pub(crate) fn query_uses_fts(query: &str) -> bool {
    !query.contains('/') && !query.contains('\\') && !query.contains('.')
}

/// Re-sort results so exact and prefix filename matches appear first.
/// Preserves the original (BM25) order within each tier.
fn sort_by_name_relevance(
    mut results: Vec<scat_core::core::db::JsonRow>,
    query: &str,
) -> Vec<scat_core::core::db::JsonRow> {
    let q = query.to_lowercase();
    results.sort_by_cached_key(|row| {
        let path = ScriptView::new(row).logical_path();
        // Split on both separators so basenames resolve correctly even if a
        // path carries Windows-style backslashes.
        let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let stem = basename
            .rsplit_once('.')
            .map(|(s, _)| s)
            .unwrap_or(basename);
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

/// showing its target path.
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
            output.push(row);
            let mut sub = scat_core::core::db::JsonRow::new();
            sub.insert("logical_path".into(), format!("  ↳ {target_path}").into());
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

const DEFAULT_SHOW_FIELDS: &[&str] = &[
    "language", "owner", "purpose", "checkout", "size", "indexed", "uses", "used_by",
];

pub(crate) fn cmd_show(
    api: &scat_core::core::search::SearchApi,
    path: &str,
    fields: &[String],
    output: OutputFormat,
    vc_configured: bool,
    show_functions: bool,
) -> Result<()> {
    let raw = match api.get_script(path)? {
        Some(s) => s,
        None => anyhow::bail!("script '{path}' not found in catalog"),
    };

    // If the script is a symlink, resolve to the target and show that instead.
    let symlink_note = {
        let target = ScriptView::new(&raw).symlink_target();
        (!target.is_empty()).then(|| target.to_string())
    };
    let resolved = symlink_note
        .as_deref()
        .map(|t| api.get_script(t))
        .transpose()?
        .flatten();
    let (script, resolved_path): (&scat_core::core::db::JsonRow, &str) =
        match (&resolved, &symlink_note) {
            (Some(t), Some(target)) => (t, target.as_str()),
            _ => (&raw, path),
        };
    let view = ScriptView::new(script);

    // --functions: list function definitions table and return early.
    if show_functions {
        let fns = api.get_functions_defined_in(resolved_path)?;
        if output == OutputFormat::Json {
            print_json(&fns);
            return Ok(());
        }
        if fns.is_empty() {
            println!("No function definitions indexed for {resolved_path}.");
            return Ok(());
        }
        let rows: Vec<Vec<String>> = fns
            .iter()
            .map(|f| {
                vec![
                    str_field(f, "name"),
                    str_field(f, "kind"),
                    f.get("line")
                        .and_then(|v| v.as_i64())
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| str_field(f, "line")),
                    str_field(f, "docstring"),
                ]
            })
            .collect();
        println!(
            "{}",
            render_table(&["Name", "Kind", "Line", "Docstring"], &rows, false)
        );
        return Ok(());
    }

    let effective: Vec<&str> = if fields.is_empty() {
        DEFAULT_SHOW_FIELDS.to_vec()
    } else {
        fields.iter().map(String::as_str).collect()
    };
    let revisions = if vc_configured {
        api.checkouts_for(resolved_path)?
    } else {
        Vec::new()
    };

    if output == OutputFormat::Json {
        let needs_graph = effective.iter().any(|f| *f == "uses" || *f == "used_by");
        let graph = if needs_graph {
            Some(api.dependency_graph(resolved_path)?)
        } else {
            None
        };

        let mut out = scat_core::core::db::JsonRow::new();
        // Always include path as the record identifier, mirroring how the table
        // branch always prints the path before the field list.
        out.insert("path".to_string(), json_script_field(view, "path"));
        for &field in &effective {
            match field {
                "uses" => {
                    let uses: Vec<serde_json::Value> = graph
                        .as_ref()
                        .map(|g| g.uses.iter().map(dep_entry_to_json).collect())
                        .unwrap_or_default();
                    out.insert("uses".to_string(), serde_json::json!(uses));
                }
                "used_by" => {
                    let used_by: Vec<serde_json::Value> = graph
                        .as_ref()
                        .map(|g| g.used_by.iter().map(used_by_row_to_json).collect())
                        .unwrap_or_default();
                    out.insert("used_by".to_string(), serde_json::json!(used_by));
                }
                "contributors" => {
                    let contribs = view.contributors();
                    out.insert("contributors".to_string(), serde_json::json!(contribs));
                }
                _ => {
                    out.entry(field.to_string())
                        .or_insert_with(|| json_script_field(view, field));
                }
            }
        }
        if vc_configured {
            out.insert("revisions".to_string(), revisions_to_json(revisions));
        }
        print_json(&out);
        return Ok(());
    }

    if let Some(ref target) = symlink_note {
        println!("{path}");
        println!("  symlink → {target}");
        println!();
    }
    println!("{}", dash_or_empty(view.logical_path()));

    let needs_deps = effective.iter().any(|f| *f == "uses" || *f == "used_by");
    let graph = needs_deps
        .then(|| api.dependency_graph(resolved_path))
        .transpose()?;

    for field in &effective {
        match *field {
            "language" => println!("  Language     : {}", dash_or_empty(view.language())),
            "owner" => println!("  Owner        : {}", dash_or_empty(view.owner())),
            "purpose" => println!("  Purpose      : {}", dash_or_empty(view.purpose())),
            "checkout" => println!("  Checkout     : {}", view.checkout_label()),
            "size" => println!("  Size         : {}", size_field(view)),
            "indexed" => println!("  Indexed      : {}", dash_or_empty(view.indexed_at())),
            "symlink" => println!("  Symlink      : {}", dash_or_empty(view.symlink_target())),
            "uses" => {
                if let Some(g) = &graph {
                    println!("  Uses         : {}", g.uses.len());
                }
            }
            "used_by" => {
                if let Some(g) = &graph {
                    println!("  Used by      : {}", g.used_by.len());
                }
            }
            "mtime" => println!("  Modified     : {}", mtime_field(view)),
            "tags" => println!(
                "  Tags         : {}",
                list_field_display(view, ListField::Tags)
            ),
            "entry_points" => {
                println!(
                    "  Entries      : {}",
                    list_field_display(view, ListField::EntryPoints)
                )
            }
            "related" => println!(
                "  Related      : {}",
                list_field_display(view, ListField::Related)
            ),
            "contributors" => {
                let contribs = view.contributors();
                println!("  Contributors : {}", contribs.join(", "));
            }
            unknown => eprintln!("warning: unknown field '{unknown}'"),
        }
    }
    if vc_configured && !revisions.is_empty() {
        println!();
        println!("Revisions");
        for line in render_revision_lines(revisions) {
            println!("{line}");
        }
    }
    Ok(())
}

fn render_revision_lines(mut revisions: Vec<scat_core::core::db::JsonRow>) -> Vec<String> {
    revisions.sort_by(compare_revision_rows);

    revisions
        .into_iter()
        .map(|row| {
            let revision_type = str_field(&row, "revision_type");
            let os = str_field(&row, "os_flavor");
            let user = str_field(&row, "user");
            let timestamp = str_field(&row, "timestamp");
            let age = row
                .get("age_seconds")
                .and_then(serde_json::Value::as_f64)
                .map(relative_age);
            let age_suffix = age.map(|value| format!("   ({value})")).unwrap_or_default();
            format!("  {revision_type:<7} {os:<7} {user:<12} {timestamp:<13}{age_suffix}")
        })
        .collect()
}

fn revisions_to_json(revisions: Vec<scat_core::core::db::JsonRow>) -> serde_json::Value {
    serde_json::Value::Array(
        revisions
            .into_iter()
            .map(serde_json::Value::Object)
            .collect(),
    )
}

pub(crate) fn cmd_status(
    api: &scat_core::core::search::SearchApi,
    path: Option<String>,
    all: bool,
    output: OutputFormat,
    no_color: bool,
) -> Result<()> {
    if path.is_none() && !all {
        anyhow::bail!("Provide a script path or use --all.");
    }

    let rows = api.checkout_status(path.as_deref())?;

    if output == OutputFormat::Json {
        let canonical: Vec<scat_core::core::db::JsonRow> =
            rows.iter().map(canonicalize_row_keys).collect();
        print_json(&canonical);
        return Ok(());
    }

    if rows.is_empty() {
        println!("No vc checkout state found.");
        return Ok(());
    }

    let table_rows = rows
        .iter()
        .map(|row| {
            let view = ScriptView::new(row);
            vec![
                dash_or_empty(view.logical_path()),
                view.checkout_label(),
                dash_or_empty(view.checkout_os()),
                warning_kinds(view),
            ]
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        render_table(
            &["Path", "Checkout", "OS", "Warnings"],
            &table_rows,
            no_color
        )
    );
    Ok(())
}

/// Default traversal depth for `scat deps --tree` without an explicit --depth.
const DEFAULT_TREE_DEPTH: usize = 5;

pub(crate) fn cmd_deps(
    api: &scat_core::core::search::SearchApi,
    path: &str,
    tree: bool,
    depth: Option<usize>,
    output: OutputFormat,
    no_color: bool,
) -> Result<()> {
    let _ = match api.get_script(path)? {
        Some(s) => s,
        None => anyhow::bail!("script '{path}' not found in catalog"),
    };

    if tree || depth.is_some() {
        return cmd_deps_tree(api, path, depth.unwrap_or(DEFAULT_TREE_DEPTH), output);
    }

    if output == OutputFormat::Json {
        let graph = api.dependency_graph(path)?;
        let uses: Vec<serde_json::Value> = graph.uses.iter().map(dep_entry_to_json).collect();
        let used_by: Vec<serde_json::Value> =
            graph.used_by.iter().map(used_by_row_to_json).collect();
        print_json(&serde_json::json!({ "uses": uses, "used_by": used_by }));
        return Ok(());
    }

    let related = api.related_scripts(path)?;
    let graph = api.dependency_graph(path)?;
    if graph.uses.is_empty() && graph.used_by.is_empty() && related.is_empty() {
        println!("No dependencies found for {path}.");
        return Ok(());
    }

    if !graph.uses.is_empty() {
        println!("Uses:");
        let rows = graph
            .uses
            .iter()
            .map(|u| {
                vec![
                    u.logical_path.clone(),
                    kind_label(&u.kind).to_string(),
                    u.language.as_str().unwrap_or("—").to_string(),
                    if u.indexed { "yes" } else { "no" }.to_string(),
                ]
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            render_table(&["Path", "Kind", "Language", "Indexed"], &rows, no_color)
        );
    }
    if !graph.used_by.is_empty() {
        println!("Used by:");
        print_script_table(&graph.used_by, no_color);
    }
    Ok(())
}

/// Short display label for a dependency edge kind. `referenced` edges (a script
/// invoked by path rather than imported) show as `ref`.
fn kind_label(kind: &str) -> &str {
    match kind {
        "referenced" => "ref",
        "import" => "import",
        other => other,
    }
}

fn cmd_deps_tree(
    api: &scat_core::core::search::SearchApi,
    path: &str,
    depth: usize,
    output: OutputFormat,
) -> Result<()> {
    use scat_core::core::search::TreeDirection;

    let uses = api
        .dependency_tree(path, TreeDirection::Uses, depth)?
        .with_context(|| format!("script '{path}' not found in catalog"))?;
    let used_by = api
        .dependency_tree(path, TreeDirection::UsedBy, depth)?
        .with_context(|| format!("script '{path}' not found in catalog"))?;

    if output == OutputFormat::Json {
        print_json(&serde_json::json!({
            "depth": depth,
            "uses": uses,
            "used_by": used_by,
        }));
        return Ok(());
    }

    if uses.children.is_empty() && used_by.children.is_empty() {
        println!("No dependencies found for {path}.");
        return Ok(());
    }

    let mut legend_repeat = false;
    let mut legend_cycle = false;
    let mut legend_truncated = false;
    let mut legend_ref = false;
    for (heading, tree) in [("Uses", &uses), ("Used by", &used_by)] {
        if tree.children.is_empty() {
            continue;
        }
        println!("{heading} (depth ≤ {depth}):");
        for line in render_tree_lines(tree) {
            println!("{line}");
        }
        println!();
        legend_repeat |= tree_has(tree, &|n| n.repeated);
        legend_cycle |= tree_has(tree, &|n| n.cycle);
        legend_truncated |= tree_has(tree, &|n| n.truncated);
        legend_ref |= tree_has(tree, &|n| n.via_kind.as_deref() == Some("referenced"));
    }
    if legend_ref {
        println!("(ref) referenced by path (copied/executed/manifested), not imported");
    }
    if legend_repeat {
        println!("(*) subtree already shown above");
    }
    if legend_cycle {
        println!("(cycle) path depends on itself through this chain");
    }
    if legend_truncated {
        println!("(…) children beyond the depth limit; raise with --depth");
    }
    Ok(())
}

fn tree_has(
    node: &scat_core::core::search::DepsTreeNode,
    predicate: &impl Fn(&scat_core::core::search::DepsTreeNode) -> bool,
) -> bool {
    predicate(node) || node.children.iter().any(|child| tree_has(child, predicate))
}

fn render_tree_lines(root: &scat_core::core::search::DepsTreeNode) -> Vec<String> {
    fn node_label(node: &scat_core::core::search::DepsTreeNode) -> String {
        let mut label = node.logical_path.clone();
        if node.via_kind.as_deref() == Some("referenced") {
            label.push_str(" (ref)");
        }
        if !node.indexed {
            label.push_str(" (not indexed)");
        }
        if node.cycle {
            label.push_str(" (cycle)");
        }
        if node.repeated {
            label.push_str(" (*)");
        }
        if node.truncated {
            label.push_str(" (…)");
        }
        label
    }

    fn push_children(
        node: &scat_core::core::search::DepsTreeNode,
        prefix: &str,
        out: &mut Vec<String>,
    ) {
        let last_index = node.children.len().saturating_sub(1);
        for (index, child) in node.children.iter().enumerate() {
            let (branch, continuation) = if index == last_index {
                ("└── ", "    ")
            } else {
                ("├── ", "│   ")
            };
            out.push(format!("{prefix}{branch}{}", node_label(child)));
            push_children(child, &format!("{prefix}{continuation}"), out);
        }
    }

    let mut out = vec![node_label(root)];
    push_children(root, "", &mut out);
    out
}

pub(crate) fn cmd_stats(
    api: &scat_core::core::search::SearchApi,
    json: bool,
    no_color: bool,
    vc_configured: bool,
) -> Result<()> {
    let mut data = api.stats()?;
    if !vc_configured {
        data.revisions = None;
    }

    if json {
        print_json(&data);
        return Ok(());
    }

    println!("Total scripts: {}", data.total_scripts);
    println!("\nBy language:");
    let by_language = data
        .by_language
        .iter()
        .map(|row| vec![row.language.clone(), row.count.to_string()])
        .collect::<Vec<_>>();
    println!(
        "{}",
        render_table(&["Language", "Count"], &by_language, no_color)
    );
    println!("\nBy owner:");
    let by_owner = data
        .by_owner
        .iter()
        .map(|row| vec![row.owner.clone(), row.count.to_string()])
        .collect::<Vec<_>>();
    println!("{}", render_table(&["Owner", "Count"], &by_owner, no_color));
    if let Some(revisions) = &data.revisions {
        println!("\nRevision statistics");
        for line in render_revision_stats_lines(revisions) {
            println!("{line}");
        }
    }
    Ok(())
}

fn render_revision_stats_lines(stats: &scat_core::core::search::RevisionStats) -> Vec<String> {
    vec![
        format!(
            "  Scripts with active checkouts: {}",
            stats.scripts_with_active_checkouts
        ),
        format!(
            "  Scripts with archive entries: {}",
            stats.scripts_with_archive_entries
        ),
        format!(
            "  Total DEVELOP revision files: {}",
            stats.total_develop_revision_files
        ),
        format!(
            "  Total ARCHIVE revision files: {}",
            stats.total_archive_revision_files
        ),
        format!(
            "  Scripts checked out by >1 user: {}",
            stats.scripts_checked_out_by_multiple_users
        ),
    ]
}

pub(crate) fn cmd_symlinks(
    api: &scat_core::core::search::SearchApi,
    path: &str,
    output: OutputFormat,
    no_color: bool,
) -> Result<()> {
    let inbound = api.symlinks_to(path)?;
    let script = api.get_script(path)?;

    let outbound_target: Option<String> = script.as_ref().and_then(|r| {
        let target = ScriptView::new(r).symlink_target();
        (!target.is_empty()).then(|| target.to_string())
    });
    let outbound_row: Option<scat_core::core::db::JsonRow> = outbound_target
        .as_deref()
        .map(|t| api.get_script(t))
        .transpose()?
        .flatten();

    if output == OutputFormat::Json {
        let combined = serde_json::json!({
            "points_to": outbound_row.as_ref().map(canonicalize_row_keys),
            "pointed_to_by": inbound.iter().map(canonicalize_row_keys).collect::<Vec<_>>(),
        });
        print_json(&combined);
        return Ok(());
    }

    let has_outbound = outbound_target.is_some();
    let has_inbound = !inbound.is_empty();

    if !has_outbound && !has_inbound {
        println!("No symlink relationships for {path}.");
        return Ok(());
    }

    if let Some(ref target) = outbound_target {
        if let Some(ref row) = outbound_row {
            println!("{path} points to:");
            print_script_table(std::slice::from_ref(row), no_color);
        } else {
            println!("{path} points to: {target} (not indexed)");
        }
    }

    if has_outbound && has_inbound {
        println!();
    }

    if has_inbound {
        println!("Symlinks pointing to {path}:");
        print_script_table(&inbound, no_color);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Script diff command handlers
// ---------------------------------------------------------------------------

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

pub(crate) fn cmd_info(api: &scat_core::core::search::SearchApi, json: bool) -> Result<()> {
    let meta = api.index_metadata()?;

    if json {
        print_json(&meta);
        return Ok(());
    }

    println!("Last indexed       : {}", meta.build_timestamp);
    println!("Schema version (DB): {}", meta.schema_version);
    println!("Schema version (app): {}", meta.current_schema_version);
    Ok(())
}

pub(crate) fn cmd_audit(
    api: &scat_core::core::search::SearchApi,
    checks: &[String],
    strict: bool,
    stale_days: i64,
    json: bool,
) -> Result<()> {
    let selected = (!checks.is_empty()).then_some(checks);
    let result = api.audit(selected, stale_days)?;

    if json {
        print_json(&result);
    } else {
        println!("scat audit — {} findings", result.findings.len());
        println!();
        for finding in &result.findings {
            println!(
                "{:<5} {:<16} {} — {}",
                finding.severity.to_uppercase(),
                finding.check,
                finding.logical_path,
                finding.detail
            );
        }
        if !result.findings.is_empty() {
            println!();
        }
        println!(
            "Summary: {} error, {} warn, {} info",
            result.summary.error, result.summary.warn, result.summary.info
        );
    }

    let exit_code = audit_exit_code(&result.summary, strict);
    if exit_code != 0 {
        process::exit(exit_code);
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_index(
    scan_roots: &[PathBuf],
    db_path: &Path,
    logical_prefix: &str,
    head_lines: usize,
    ignore_files: &[PathBuf],
    keep_copies: usize,
    dry_run: bool,
    config: scat_core::core::vc::VcConfig,
    json: bool,
    quiet: bool,
    no_resume: bool,
    force: bool,
) -> Result<()> {
    let effective_scan_roots: Vec<PathBuf> = if scan_roots.is_empty() {
        config.scan_roots.clone()
    } else {
        scan_roots.to_vec()
    };
    if effective_scan_roots.is_empty() {
        anyhow::bail!(
            "No scan roots specified. Use --scan-root or add scan_roots to the config file."
        );
    }

    // Start with CLI-supplied ignore files, then append a temp file for any
    // inline patterns from the config.  The temp file must stay alive until
    // after build_index returns, so we keep it in scope here.
    let mut effective_ignore: Vec<PathBuf> = ignore_files.to_vec();
    let _config_ignore_tmp: Option<tempfile::NamedTempFile> = if config.ignore_patterns.is_empty() {
        None
    } else {
        use std::io::Write;
        let mut tmp =
            tempfile::NamedTempFile::new().with_context(|| "Failed to create temp ignore file")?;
        for pattern in &config.ignore_patterns {
            writeln!(tmp, "{pattern}")?;
        }
        effective_ignore.push(tmp.path().to_path_buf());
        Some(tmp)
    };

    // Register Ctrl-C handler at the application entry point. The same signal
    // is checked by pre-build change detection and the index build itself.
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let shutdown_clone = std::sync::Arc::clone(&shutdown);
        let _ = ctrlc::set_handler(move || {
            shutdown_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }

    // ---------------------------------------------------------------------------
    // Change-detection: skip rebuild when nothing has changed.
    // ---------------------------------------------------------------------------
    if !force
        && !dry_run
        && db_path.exists()
        && let Some(indexed_at_secs) = read_indexed_at(db_path)
    {
        let checkout_dirs: Vec<&str> = config.all_checkout_dirs().collect();
        let max_mtime = max_mtime_in_roots_with_shutdown(
            &effective_scan_roots,
            &effective_ignore,
            &checkout_dirs,
            &shutdown,
        )
        .with_context(|| "Failed to check scan root modification times")?;
        if should_skip_catalog_rebuild(indexed_at_secs, max_mtime) {
            if json {
                print_json(&serde_json::json!({
                    "up_to_date": true,
                    "db_path": db_path.display().to_string(),
                }));
            } else {
                println!("catalog is up to date — skipping rebuild");
            }
            return Ok(());
        }
    }

    let opts = BuildOptions {
        logical_prefix: logical_prefix.to_string(),
        head_lines,
        ignore_files: effective_ignore,
        keep_copies,
        dry_run,
        vc_config: Some(config),
        quiet: quiet || json,
        no_resume,
        shutdown: Some(shutdown),
    };

    let result = build_index(&effective_scan_roots, db_path, opts)
        .with_context(|| format!("Failed to build index at {}", db_path.display()))?;

    if json {
        print_json(&serde_json::json!({
            "scripts_indexed": result.scripts_indexed,
            "dependencies_indexed": result.dependencies_indexed,
            "db_path": result.db_path.display().to_string(),
            "dry_run": result.dry_run,
            "errors": result.errors.iter().map(|(p, e)| serde_json::json!({"path": p, "error": e})).collect::<Vec<_>>(),
        }));
    } else {
        println!(
            "Indexed {} scripts in total ({} dependencies) → {}{}",
            result.scripts_indexed,
            result.dependencies_indexed,
            result.db_path.display(),
            if dry_run { " (dry run)" } else { "" }
        );
        if !result.errors.is_empty() {
            warn!(
                error_count = result.errors.len(),
                "file(s) failed during indexing"
            );
            for (path, err) in &result.errors {
                warn!(path = %path, error = %err, "indexing file failed");
            }
        }
    }
    Ok(())
}

pub(crate) fn should_skip_catalog_rebuild(indexed_at_secs: f64, max_mtime: Option<f64>) -> bool {
    max_mtime.is_some_and(|mtime| mtime.floor() < indexed_at_secs)
}

/// Open the existing catalog database and return the `build_timestamp` as UNIX
/// epoch seconds, or `None` if the DB cannot be read, has no metadata row, or
/// has a schema-version mismatch (all of which should trigger a rebuild).
fn read_indexed_at(db_path: &Path) -> Option<f64> {
    use rusqlite::{Connection, OpenFlags};
    use scat_core::core::db::SCHEMA_VERSION;

    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let (ts, schema_ver): (Option<String>, i64) = conn
        .query_row(
            "SELECT build_timestamp, schema_version FROM index_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;

    // Schema mismatch → always rebuild.
    if schema_ver != SCHEMA_VERSION {
        return None;
    }

    let ts = ts?;
    chrono::DateTime::parse_from_rfc3339(&ts)
        .ok()
        .map(|dt| dt.timestamp() as f64)
}

#[cfg(test)]
mod tests {
    use super::{
        cmd_deps, cmd_show, query_uses_fts, relative_age, render_revision_lines,
        render_revision_stats_lines, revisions_to_json,
    };
    use scat_core::core::db::{JsonRow, SCHEMA_VERSION, create_db};
    use scat_core::core::search::{RevisionStats, SearchApi};
    use tempfile::NamedTempFile;

    fn make_api() -> (SearchApi, NamedTempFile) {
        let file = NamedTempFile::new().unwrap();
        let conn = create_db(file.path()).unwrap();
        conn.execute(
            "INSERT INTO index_metadata (id, build_timestamp, schema_version)
             VALUES (1, '2024-01-01T00:00:00', ?1)",
            rusqlite::params![SCHEMA_VERSION],
        )
        .unwrap();

        (SearchApi::new(conn), file)
    }

    fn revision_row(
        os_flavor: &str,
        user: &str,
        timestamp: &str,
        age_seconds: Option<f64>,
    ) -> JsonRow {
        let mut row = JsonRow::new();
        row.insert("logical_path".to_string(), "/catalog/scripts/foo.py".into());
        row.insert("physical_path".to_string(), "/tmp/foo_checkout".into());
        row.insert("revision_type".to_string(), "DEVELOP".into());
        row.insert("os_flavor".to_string(), os_flavor.into());
        row.insert("user".to_string(), user.into());
        row.insert("timestamp".to_string(), timestamp.into());
        row.insert(
            "age_seconds".to_string(),
            age_seconds
                .map(serde_json::Value::from)
                .unwrap_or(serde_json::Value::Null),
        );
        row
    }

    #[test]
    fn render_revision_lines_groups_by_os_and_sorts_newest_first() {
        let rows = vec![
            revision_row("ZOS", "alice", "20240101_1000", Some(3_600.0)),
            revision_row("LINUX", "jdoe", "20240102_0900", Some(7_200.0)),
            revision_row("LINUX", "bob", "20240101_0900", Some(86_400.0)),
        ];

        let lines = render_revision_lines(rows);

        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("LINUX"));
        assert!(lines[0].contains("jdoe"));
        assert!(lines[1].contains("LINUX"));
        assert!(lines[1].contains("bob"));
        assert!(lines[2].contains("ZOS"));
    }

    #[test]
    fn revisions_to_json_emits_structured_array() {
        let rows = vec![revision_row("LINUX", "jdoe", "20240102_0900", Some(120.0))];

        let value = revisions_to_json(rows);
        let array = value.as_array().expect("revisions should be an array");

        assert_eq!(array.len(), 1);
        assert_eq!(array[0]["os_flavor"], "LINUX");
        assert_eq!(array[0]["user"], "jdoe");
        assert_eq!(array[0]["timestamp"], "20240102_0900");
        assert_eq!(array[0]["age_seconds"], 120.0);
    }

    #[test]
    fn query_uses_fts_excludes_path_like_queries() {
        assert!(super::query_uses_fts("checkmc"));
        assert!(!super::query_uses_fts("scripts/checkmc"));
        assert!(!super::query_uses_fts("checkmc.py"));
    }

    #[test]
    fn skip_rebuild_requires_existing_older_file_mtime() {
        assert!(super::should_skip_catalog_rebuild(100.0, Some(99.9)));
        assert!(!super::should_skip_catalog_rebuild(100.0, Some(100.0)));
        assert!(!super::should_skip_catalog_rebuild(100.0, Some(100.1)));
        assert!(!super::should_skip_catalog_rebuild(100.0, None));
    }

    #[test]
    fn relative_age_clamps_negative_values() {
        assert_eq!(relative_age(-120.0), "0m ago");
    }

    #[test]
    fn cmd_show_missing_script_returns_plain_error() {
        let (api, _file) = make_api();
        let err = cmd_show(
            &api,
            "/catalog/scripts/missing.py",
            &[],
            super::OutputFormat::Table,
            false,
            false,
        )
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "script '/catalog/scripts/missing.py' not found in catalog"
        );
    }

    #[test]
    fn cmd_deps_missing_script_returns_plain_error() {
        let (api, _file) = make_api();
        let err = cmd_deps(
            &api,
            "/catalog/scripts/missing.py",
            false,
            None,
            super::OutputFormat::Table,
            true,
        )
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "script '/catalog/scripts/missing.py' not found in catalog"
        );
    }

    #[test]
    fn render_tree_lines_draws_branches_and_markers() {
        use scat_core::core::search::DepsTreeNode;

        let node = |path: &str, children: Vec<DepsTreeNode>| DepsTreeNode {
            logical_path: path.to_string(),
            indexed: true,
            cycle: false,
            repeated: false,
            truncated: false,
            via_kind: Some("import".to_string()),
            children,
        };

        let mut external = node("/external/lib.py", vec![]);
        external.indexed = false;
        let mut shared_again = node("/catalog/shared.py", vec![]);
        shared_again.repeated = true;
        let mut referenced = node("/catalog/runner.sh", vec![]);
        referenced.via_kind = Some("referenced".to_string());
        let root = node(
            "/catalog/a.py",
            vec![
                node(
                    "/catalog/b.py",
                    vec![node(
                        "/catalog/shared.py",
                        vec![node("/catalog/leaf.py", vec![])],
                    )],
                ),
                node("/catalog/c.py", vec![shared_again, external, referenced]),
            ],
        );

        assert_eq!(
            super::render_tree_lines(&root),
            vec![
                "/catalog/a.py",
                "├── /catalog/b.py",
                "│   └── /catalog/shared.py",
                "│       └── /catalog/leaf.py",
                "└── /catalog/c.py",
                "    ├── /catalog/shared.py (*)",
                "    ├── /external/lib.py (not indexed)",
                "    └── /catalog/runner.sh (ref)",
            ]
        );
    }

    #[test]
    fn query_uses_fts_routes_paths_including_backslashes() {
        assert!(query_uses_fts("patch"), "plain word → FTS");
        assert!(
            !query_uses_fts("jobs/nightly"),
            "forward-slash path → path search"
        );
        assert!(!query_uses_fts("foo.py"), "dotted name → path search");
        assert!(
            !query_uses_fts("scripts\\foo"),
            "Windows backslash path → path search"
        );
    }

    #[test]
    fn render_revision_stats_lines_matches_catalog_stats_labels() {
        let lines = render_revision_stats_lines(&RevisionStats {
            scripts_with_active_checkouts: 12,
            scripts_with_archive_entries: 47,
            total_develop_revision_files: 18,
            total_archive_revision_files: 134,
            scripts_checked_out_by_multiple_users: 2,
        });

        assert_eq!(
            lines,
            vec![
                "  Scripts with active checkouts: 12",
                "  Scripts with archive entries: 47",
                "  Total DEVELOP revision files: 18",
                "  Total ARCHIVE revision files: 134",
                "  Scripts checked out by >1 user: 2",
            ]
        );
    }
}
