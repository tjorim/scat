use anyhow::{Context, Result};

use crate::cli::OutputFormat;
use crate::output::{
    dep_entry_to_json, print_json, print_script_table, render_table, used_by_row_to_json,
};

/// Default traversal depth for `scat deps --tree` without an explicit --depth.
const DEFAULT_TREE_DEPTH: usize = 5;

pub fn cmd_deps(
    api: &scat_core::core::search::SearchApi,
    path: &str,
    tree: bool,
    depth: Option<usize>,
    output: OutputFormat,
    no_color: bool,
) -> Result<()> {
    if api.get_script(path)?.is_none() {
        // Not a cataloged script, but a header may still declare it
        // (`@inputfile config.yaml`): list who does, so a declared file is
        // navigable from the file side too.
        let declared_by = api.declared_by(path)?;
        if declared_by.is_empty() {
            anyhow::bail!("script '{path}' not found in catalog");
        }
        cmd_deps_declared_file(path, &declared_by, output, no_color);
        return Ok(());
    }

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
    print_shared_declared_files(api, path, &graph.uses, no_color)?;
    if !graph.used_by.is_empty() {
        println!("Used by:");
        print_script_table(&graph.used_by, no_color);
    }
    Ok(())
}

/// Short display label for a dependency edge kind. `referenced` edges (a script
/// invoked by path rather than imported) show as `ref`; header-declared files
/// show their keyword without the `file` suffix (`@inputfile` → `input`).
fn kind_label(kind: &str) -> &str {
    match kind {
        "referenced" => "ref",
        "import" => "import",
        other => other.strip_suffix("file").unwrap_or(other),
    }
}

/// For each declared file that is not an indexed script, list the *other*
/// scripts that declare the same file — the "who else relies on this" view for
/// data files (feeds, reports) that scat deliberately doesn't catalog.
fn print_shared_declared_files(
    api: &scat_core::core::search::SearchApi,
    path: &str,
    uses: &[scat_core::core::search::DependencyEntry],
    no_color: bool,
) -> Result<()> {
    use scat_core::core::db::{is_declared_file_kind, row_str};

    let mut rows: Vec<Vec<String>> = Vec::new();
    for entry in uses
        .iter()
        .filter(|u| !u.indexed && is_declared_file_kind(&u.kind))
    {
        for other in api.declared_by(&entry.depends_on_path)? {
            let other_path = row_str(&other, "logical_path");
            if other_path == path {
                continue;
            }
            rows.push(vec![
                entry.depends_on_path.clone(),
                kind_label(&entry.kind).to_string(),
                other_path.to_string(),
                kind_label(row_str(&other, "kind")).to_string(),
            ]);
        }
    }
    if !rows.is_empty() {
        println!("Shared with other scripts (declared files not in the catalog):");
        println!(
            "{}",
            render_table(&["File", "Here", "Other script", "There"], &rows, no_color)
        );
    }
    Ok(())
}

/// `scat deps <path>` for a path that isn't a cataloged script but is declared
/// by one or more `@…file` headers.
fn cmd_deps_declared_file(
    path: &str,
    declared_by: &[scat_core::core::db::JsonRow],
    output: OutputFormat,
    no_color: bool,
) {
    use scat_core::core::db::row_str;

    if output == OutputFormat::Json {
        let rows: Vec<serde_json::Value> = declared_by
            .iter()
            .map(|row| {
                serde_json::json!({
                    "path": row_str(row, "logical_path"),
                    "role": row_str(row, "kind"),
                    "language": row.get("language").cloned().unwrap_or_default(),
                    "owner": row.get("owner").cloned().unwrap_or_default(),
                    "purpose": row.get("purpose").cloned().unwrap_or_default(),
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "path": path,
            "indexed": false,
            "declared_by": rows,
        }));
        return;
    }

    println!("{path} is not a cataloged script; it is declared in script headers.");
    println!("Declared by:");
    let rows = declared_by
        .iter()
        .map(|row| {
            vec![
                row_str(row, "logical_path").to_string(),
                kind_label(row_str(row, "kind")).to_string(),
                dash_if_empty(row_str(row, "owner")),
                dash_if_empty(row_str(row, "purpose")),
            ]
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        render_table(&["Script", "Role", "Owner", "Purpose"], &rows, no_color)
    );
}

fn dash_if_empty(value: &str) -> String {
    if value.is_empty() { "—" } else { value }.to_string()
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
    let mut legend_declared = false;
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
        legend_declared |= tree_has(tree, &|n| n.via_kind.as_deref().is_some_and(is_declared));
    }
    if legend_ref {
        println!("(ref) referenced by path (copied/executed/manifested), not imported");
    }
    if legend_declared {
        println!("(input/output/parent/child/param) declared in an @…file header");
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

fn is_declared(kind: &str) -> bool {
    scat_core::core::db::is_declared_file_kind(kind)
}

fn tree_has(
    node: &scat_core::core::search::DepsTreeNode,
    predicate: &impl Fn(&scat_core::core::search::DepsTreeNode) -> bool,
) -> bool {
    predicate(node) || node.children.iter().any(|child| tree_has(child, predicate))
}

pub fn render_tree_lines(root: &scat_core::core::search::DepsTreeNode) -> Vec<String> {
    fn node_label(node: &scat_core::core::search::DepsTreeNode) -> String {
        let mut label = node.logical_path.clone();
        match node.via_kind.as_deref() {
            Some("referenced") => label.push_str(" (ref)"),
            Some(kind) if is_declared(kind) => {
                label.push_str(&format!(" ({})", kind_label(kind)));
            }
            _ => {}
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
