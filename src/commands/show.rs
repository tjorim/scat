use anyhow::Result;
use scat_core::core::script_view::{ListField, ScriptView};
use scat_core::core::vc::{compare_revision_rows, relative_age};

use crate::cli::OutputFormat;
use crate::output::{
    dash_or_empty, dep_entry_to_json, json_script_field, list_field_display, mtime_field,
    print_json, render_table, sibling_row_to_json, size_field, str_field, used_by_row_to_json,
};

const DEFAULT_SHOW_FIELDS: &[&str] = &[
    "language", "owner", "purpose", "checkout", "size", "indexed", "uses", "used_by",
];

pub fn cmd_show(
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
                        .and_then(serde_json::Value::as_i64)
                        .map_or_else(|| str_field(f, "line"), |n| n.to_string()),
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
        let needs_siblings = effective.contains(&"siblings");
        let siblings = needs_siblings
            .then(|| api.siblings(resolved_path))
            .transpose()?;

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
                "folder" => {
                    let folder = view.parent_dir();
                    let value = if folder.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(folder.to_string())
                    };
                    out.insert("folder".to_string(), value);
                }
                "siblings" => {
                    let rows: Vec<serde_json::Value> = siblings
                        .as_ref()
                        .map(|s| s.iter().map(sibling_row_to_json).collect())
                        .unwrap_or_default();
                    out.insert("siblings".to_string(), serde_json::json!(rows));
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
    let needs_siblings = effective.contains(&"siblings");
    let siblings = needs_siblings
        .then(|| api.siblings(resolved_path))
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
            "folder" => println!("  Folder       : {}", dash_or_empty(view.parent_dir())),
            "siblings" => {
                if let Some(s) = &siblings {
                    println!("  Siblings     : {}", s.len());
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
                );
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

pub fn render_revision_lines(mut revisions: Vec<scat_core::core::db::JsonRow>) -> Vec<String> {
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
            format!("  {revision_type:<7} {os:<7} {user:<12} {timestamp:<15}{age_suffix}")
        })
        .collect()
}

pub fn revisions_to_json(revisions: Vec<scat_core::core::db::JsonRow>) -> serde_json::Value {
    serde_json::Value::Array(
        revisions
            .into_iter()
            .map(serde_json::Value::Object)
            .collect(),
    )
}
