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

pub fn render_tree_lines(root: &scat_core::core::search::DepsTreeNode) -> Vec<String> {
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
