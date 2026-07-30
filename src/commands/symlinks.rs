use anyhow::Result;
use scat_core::core::script_view::ScriptView;

use crate::cli::OutputFormat;
use crate::output::{canonicalize_row_keys, print_json, print_script_table};

pub fn cmd_symlinks(
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
