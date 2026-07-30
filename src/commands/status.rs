use anyhow::Result;
use scat_core::core::script_view::ScriptView;

use crate::cli::OutputFormat;
use crate::output::{
    canonicalize_row_keys, dash_or_empty, print_json, render_table, warning_kinds,
};

pub fn cmd_status(
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
