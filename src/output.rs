use comfy_table::{
    Attribute, Cell, Color, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED,
};
use tracing::warn;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MIN_COLUMN_WIDTH: usize = 4;
const TABLE_OVERHEAD_PER_COLUMN: usize = 3;
const TABLE_BORDER_END_WIDTH: usize = 1;
const DEFAULT_TERMINAL_WIDTH: usize = 120;
const DEFAULT_SEARCH_FIELDS: &[&str] = &["path", "language", "owner", "purpose"];

pub(crate) fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => warn!(error = %e, "failed to serialize JSON"),
    }
}

pub(crate) fn print_script_table(scripts: &[scat_core::core::db::JsonRow], no_color: bool) {
    print_script_table_with_fields(scripts, DEFAULT_SEARCH_FIELDS, no_color);
}

pub(crate) fn print_script_table_with_fields(
    scripts: &[scat_core::core::db::JsonRow],
    fields: &[&str],
    no_color: bool,
) {
    let (headers, rows, truncate_left) = script_table_rows(scripts, fields);
    let header_refs = headers.iter().map(String::as_str).collect::<Vec<_>>();
    let width = terminal_width();
    println!(
        "{}",
        render_table_with_width(&header_refs, &rows, no_color, width, &truncate_left,)
    );
}

fn script_table_rows(
    scripts: &[scat_core::core::db::JsonRow],
    fields: &[&str],
) -> (Vec<String>, Vec<Vec<String>>, Vec<bool>) {
    let headers = fields
        .iter()
        .map(|field| script_field_header(field).to_string())
        .collect::<Vec<_>>();
    let rows = scripts
        .iter()
        .map(|row| {
            fields
                .iter()
                .map(|field| display_script_field(row, field))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let truncate_left = fields
        .iter()
        .map(|field| *field == "path")
        .collect::<Vec<_>>();
    (headers, rows, truncate_left)
}

pub(crate) fn render_script_csv(
    scripts: &[scat_core::core::db::JsonRow],
    fields: &[String],
) -> String {
    let selected_fields = selected_script_fields(fields);
    let mut output = String::new();
    output.push_str(&render_csv_row(&selected_fields));
    output.push('\n');
    for row in scripts {
        let values = selected_fields
            .iter()
            .map(|field| display_script_field(row, field))
            .collect::<Vec<_>>();
        output.push_str(&render_csv_row(&values));
        output.push('\n');
    }
    output
}

fn render_csv_row<T: AsRef<str>>(values: &[T]) -> String {
    values
        .iter()
        .map(|value| csv_escape(value.as_ref()))
        .collect::<Vec<_>>()
        .join(",")
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub(crate) fn script_rows_to_json(
    scripts: &[scat_core::core::db::JsonRow],
    fields: &[String],
) -> Vec<scat_core::core::db::JsonRow> {
    let selected_fields = selected_script_fields(fields);
    scripts
        .iter()
        .map(|row| {
            let mut out = scat_core::core::db::JsonRow::new();
            for field in &selected_fields {
                out.insert((*field).to_string(), json_script_field(row, field));
            }
            out
        })
        .collect()
}

pub(crate) fn selected_script_fields(fields: &[String]) -> Vec<&'static str> {
    let requested = if fields.is_empty() {
        DEFAULT_SEARCH_FIELDS
            .iter()
            .map(|field| (*field).to_string())
            .collect::<Vec<_>>()
    } else {
        fields.to_vec()
    };

    let mut selected = Vec::new();
    for field in requested {
        match canonical_script_field(&field) {
            Some(canonical) if !selected.contains(&canonical) => selected.push(canonical),
            Some(_) => {}
            None => warn!("unknown field '{field}'"),
        }
    }
    selected
}

fn canonical_script_field(field: &str) -> Option<&'static str> {
    match field {
        "path" | "logical_path" => Some("path"),
        "language" => Some("language"),
        "owner" => Some("owner"),
        "purpose" => Some("purpose"),
        "checkout" => Some("checkout"),
        "size" => Some("size"),
        "indexed" | "indexed_at" => Some("indexed"),
        "symlink" | "symlink_target" => Some("symlink"),
        "mtime" => Some("mtime"),
        "tags" => Some("tags"),
        "entry_points" => Some("entry_points"),
        "related" => Some("related"),
        _ => None,
    }
}

fn script_field_header(field: &str) -> &'static str {
    match field {
        "path" => "Path",
        "language" => "Language",
        "owner" => "Owner",
        "purpose" => "Purpose",
        "checkout" => "Checkout",
        "size" => "Size",
        "indexed" => "Indexed",
        "symlink" => "Symlink",
        "mtime" => "Modified",
        "tags" => "Tags",
        "entry_points" => "Entry Points",
        "related" => "Related",
        _ => "Field",
    }
}

fn display_script_field(row: &scat_core::core::db::JsonRow, field: &str) -> String {
    match field {
        "path" => str_field(row, "logical_path"),
        "language" => str_field(row, "language"),
        "owner" => str_field(row, "owner"),
        "purpose" => str_field(row, "purpose"),
        "checkout" => checkout_label(row),
        "size" => size_field(row),
        "indexed" => str_field(row, "indexed_at"),
        "symlink" => str_field(row, "symlink_target"),
        "mtime" => mtime_field(row),
        "tags" => json_array_field(row, "tags"),
        "entry_points" => json_array_field(row, "entry_points"),
        "related" => json_array_field(row, "related"),
        _ => "—".to_string(),
    }
}

pub(crate) fn json_script_field(
    row: &scat_core::core::db::JsonRow,
    field: &str,
) -> serde_json::Value {
    match field {
        "path" => row
            .get("logical_path")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "language" => row
            .get("language")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "owner" => row.get("owner").cloned().unwrap_or(serde_json::Value::Null),
        "purpose" => row
            .get("purpose")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "checkout" => serde_json::Value::String(checkout_label(row)),
        "size" => row.get("size").cloned().unwrap_or(serde_json::Value::Null),
        "indexed" => row
            .get("indexed_at")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "symlink" => row
            .get("symlink_target")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "mtime" => row.get("mtime").cloned().unwrap_or(serde_json::Value::Null),
        "tags" => json_value_field(row, "tags"),
        "entry_points" => json_value_field(row, "entry_points"),
        "related" => json_value_field(row, "related"),
        _ => serde_json::Value::Null,
    }
}

fn json_value_field(row: &scat_core::core::db::JsonRow, key: &str) -> serde_json::Value {
    row.get(key)
        .and_then(|value| value.as_str())
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// Return the unique, sorted list of contributors for a script row.
///
/// Contributors are the union of the `owner` field and all `author` values
/// found in `history_entries` inside `metadata_json`.
pub(crate) fn contributors_from_script(row: &scat_core::core::db::JsonRow) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<String> = BTreeSet::new();

    // Include the primary owner.
    if let Some(serde_json::Value::String(owner)) = row.get("owner") {
        let trimmed = owner.trim();
        if !trimmed.is_empty() {
            set.insert(trimmed.to_string());
        }
    }

    // Include authors from history_entries inside metadata_json.
    if let Some(serde_json::Value::String(meta)) = row.get("metadata_json")
        && let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(meta)
        && let Some(serde_json::Value::Array(entries)) = map.get("history_entries")
    {
        for entry in entries {
            if let Some(serde_json::Value::String(author)) = entry.get("author") {
                let trimmed = author.trim();
                if !trimmed.is_empty() {
                    set.insert(trimmed.to_string());
                }
            }
        }
    }

    set.into_iter().collect()
}

pub(crate) fn render_table(headers: &[&str], rows: &[Vec<String>], no_color: bool) -> String {
    let width = terminal_width();
    render_table_with_width(headers, rows, no_color, width, &[])
}

fn render_table_with_width(
    headers: &[&str],
    rows: &[Vec<String>],
    no_color: bool,
    width: usize,
    truncate_left: &[bool],
) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let widths = fit_column_widths(headers, rows, width);
    let header_cells = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            styled_header(
                truncate_with_ellipsis(header, *widths.get(i).unwrap_or(&MIN_COLUMN_WIDTH)),
                no_color,
            )
        })
        .collect::<Vec<_>>();
    table.set_header(header_cells);

    for row in rows {
        let cells = row
            .iter()
            .enumerate()
            .map(|(i, value)| {
                let col_width = *widths.get(i).unwrap_or(&MIN_COLUMN_WIDTH);
                let truncated = if truncate_left.get(i).copied().unwrap_or(false) {
                    truncate_left_with_ellipsis(value, col_width)
                } else {
                    truncate_with_ellipsis(value, col_width)
                };
                Cell::new(truncated)
            })
            .collect::<Vec<_>>();
        table.add_row(cells);
    }
    table.to_string()
}

fn styled_header(value: String, no_color: bool) -> Cell {
    let mut cell = Cell::new(value);
    if !no_color {
        cell = cell.fg(Color::Cyan).add_attribute(Attribute::Bold);
    }
    cell
}

fn fit_column_widths(headers: &[&str], rows: &[Vec<String>], total_width: usize) -> Vec<usize> {
    let mut widths = headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            let max_row = rows
                .iter()
                .filter_map(|row| row.get(index))
                .map(|value| value.width())
                .max()
                .unwrap_or(0);
            header.width().max(max_row).max(MIN_COLUMN_WIDTH)
        })
        .collect::<Vec<_>>();

    if widths.is_empty() {
        return widths;
    }

    let overhead = (TABLE_OVERHEAD_PER_COLUMN * widths.len()) + TABLE_BORDER_END_WIDTH;
    let budget = total_width
        .saturating_sub(overhead)
        .max(widths.len() * MIN_COLUMN_WIDTH);

    while widths.iter().sum::<usize>() > budget {
        if let Some((idx, _)) = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, width)| *width)
            .filter(|(_, width)| **width > MIN_COLUMN_WIDTH)
        {
            widths[idx] -= 1;
        } else {
            break;
        }
    }

    widths
}

fn truncate_with_ellipsis(value: &str, max_width: usize) -> String {
    let width = value.width();
    if width <= max_width {
        return value.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let ellipsis = "…";
    let ellipsis_width = ellipsis.width();
    let available = max_width.saturating_sub(ellipsis_width);

    let mut result = String::new();
    let mut current_width = 0;
    for ch in value.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > available {
            break;
        }
        result.push(ch);
        current_width += ch_width;
    }
    result.push_str(ellipsis);
    result
}

fn truncate_left_with_ellipsis(value: &str, max_width: usize) -> String {
    let width = value.width();
    if width <= max_width {
        return value.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let ellipsis = "…";
    let ellipsis_width = ellipsis.width();
    let available = max_width.saturating_sub(ellipsis_width);

    let mut result = String::new();
    let mut current_width = 0;
    for ch in value.chars().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > available {
            break;
        }
        result.push(ch);
        current_width += ch_width;
    }
    let mut out = ellipsis.to_string();
    out.push_str(&result.chars().rev().collect::<String>());
    out
}

fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(width, _)| usize::from(width))
        .unwrap_or(DEFAULT_TERMINAL_WIDTH)
}

pub(crate) fn warning_kinds(row: &scat_core::core::db::JsonRow) -> String {
    let raw = str_field(row, "vc_warnings");
    let Ok(serde_json::Value::Array(warnings)) = serde_json::from_str::<serde_json::Value>(&raw)
    else {
        return "—".to_string();
    };

    let kinds = warnings
        .iter()
        .filter_map(|warning| warning.get("kind").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    if kinds.is_empty() {
        "—".to_string()
    } else {
        kinds.join("; ")
    }
}

pub(crate) fn str_field(row: &scat_core::core::db::JsonRow, key: &str) -> String {
    row.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("—")
        .to_string()
}

/// Rename raw DB column names to the canonical keys used in JSON output:
/// `logical_path` → `path`, `indexed_at` → `indexed`, `symlink_target` → `symlink`.
/// All other keys are preserved as-is.
pub(crate) fn canonicalize_row_keys(
    row: &scat_core::core::db::JsonRow,
) -> scat_core::core::db::JsonRow {
    row.iter()
        .map(|(key, val)| {
            let canonical = match key.as_str() {
                "logical_path" => "path",
                "indexed_at" => "indexed",
                "symlink_target" => "symlink",
                other => other,
            };
            (canonical.to_string(), val.clone())
        })
        .collect()
}

/// Serialize a [`DependencyEntry`] as a JSON object using the canonical `"path"` key.
pub(crate) fn dep_entry_to_json(e: &scat_core::core::search::DependencyEntry) -> serde_json::Value {
    serde_json::json!({
        "path": e.logical_path,
        "depends_on_path": e.depends_on_path,
        "language": e.language,
        "owner": e.owner,
        "purpose": e.purpose,
        "indexed": e.indexed,
    })
}

/// Serialize a reverse-dependency row as a compact JSON object using canonical field names.
pub(crate) fn used_by_row_to_json(row: &scat_core::core::db::JsonRow) -> serde_json::Value {
    serde_json::json!({
        "path": row.get("logical_path"),
        "language": row.get("language"),
        "owner": row.get("owner"),
        "purpose": row.get("purpose"),
    })
}

pub(crate) fn size_field(row: &scat_core::core::db::JsonRow) -> String {
    match row.get("size").and_then(|v| v.as_i64()) {
        Some(n) if n >= 0 => format_size(n as u64),
        _ => "—".to_string(),
    }
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn json_array_field(row: &scat_core::core::db::JsonRow, key: &str) -> String {
    let s = match row.get(key).and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return "—".to_string(),
    };
    let arr: Vec<String> = serde_json::from_str(s).unwrap_or_default();
    if arr.is_empty() {
        "—".to_string()
    } else {
        arr.join(", ")
    }
}

pub(crate) fn mtime_field(row: &scat_core::core::db::JsonRow) -> String {
    let secs = match row.get("mtime").and_then(|v| v.as_f64()) {
        Some(s) => s,
        None => return "—".to_string(),
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "—".to_string())
}

pub(crate) fn checkout_label(row: &scat_core::core::db::JsonRow) -> String {
    let user = row
        .get("checkout_user")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if user.is_empty() {
        return "clean".to_string();
    }
    let ts = row
        .get("checkout_timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if ts.is_empty() {
        format!("checked out by {user}")
    } else {
        format!("checked out by {user} since {ts}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_row() -> scat_core::core::db::JsonRow {
        let mut row = scat_core::core::db::JsonRow::new();
        row.insert("logical_path".into(), json!("/catalog/scripts/checkmc.py"));
        row.insert("language".into(), json!("python"));
        row.insert("owner".into(), json!("Alice, Inc."));
        // Includes quotes and a newline so CSV escaping is exercised explicitly.
        row.insert("purpose".into(), json!("Checks \"mc\"\nquickly"));
        row.insert("size".into(), json!(1536));
        row.insert("mtime".into(), json!(1_715_000_000.0));
        row.insert("tags".into(), json!(r#"["ops","nightly"]"#));
        row.insert("entry_points".into(), json!(r#"["main"]"#));
        row.insert("related".into(), json!(r#"["/catalog/scripts/helper.py"]"#));
        row
    }

    #[test]
    fn csv_export_escapes_quotes_commas_and_newlines() {
        let csv = render_script_csv(
            &[sample_row()],
            &["path".into(), "owner".into(), "purpose".into()],
        );
        assert_eq!(
            csv,
            "path,owner,purpose\n/catalog/scripts/checkmc.py,\"Alice, Inc.\",\"Checks \"\"mc\"\"\nquickly\"\n"
        );
    }

    #[test]
    fn json_export_uses_canonical_keys_when_no_fields_specified() {
        let rows = script_rows_to_json(&[sample_row()], &[]);
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].contains_key("path"),
            "should use canonical 'path' key"
        );
        assert!(
            !rows[0].contains_key("logical_path"),
            "should not expose raw 'logical_path' key"
        );
    }

    #[test]
    fn json_export_filters_and_converts_selected_fields() {
        let rows = script_rows_to_json(
            &[sample_row()],
            &["path".into(), "tags".into(), "size".into()],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("path"),
            Some(&json!("/catalog/scripts/checkmc.py"))
        );
        assert_eq!(rows[0].get("size"), Some(&json!(1536)));
        assert_eq!(rows[0].get("tags"), Some(&json!(["ops", "nightly"])));
    }

    #[test]
    fn script_table_rows_use_selected_headers() {
        let (headers, rows, truncate_left) = script_table_rows(&[sample_row()], &["path", "size"]);
        assert_eq!(headers, vec!["Path".to_string(), "Size".to_string()]);
        assert_eq!(
            rows,
            vec![vec![
                "/catalog/scripts/checkmc.py".to_string(),
                "1.5 KB".to_string()
            ]]
        );
        assert_eq!(truncate_left, vec![true, false]);
    }

    #[test]
    fn table_rendering_truncates_long_values() {
        let table = render_table_with_width(
            &["Path", "Owner"],
            &[vec![
                "/really/long/path/that/should/truncate.py".to_string(),
                "A".to_string(),
            ]],
            true,
            32,
            &[],
        );

        assert!(table.contains('…'));
        assert!(table.contains("┌"));
    }

    #[test]
    fn table_rendering_no_color_has_no_ansi_escape_codes() {
        let table = render_table_with_width(
            &["Path", "Owner"],
            &[vec![
                "/catalog/scripts/foo.py".to_string(),
                "Alice".to_string(),
            ]],
            true,
            80,
            &[],
        );
        assert!(!table.contains("\u{1b}["));
    }

    #[test]
    fn canonicalize_row_keys_renames_db_columns() {
        let mut row = scat_core::core::db::JsonRow::new();
        row.insert("logical_path".into(), json!("/foo/bar.py"));
        row.insert("indexed_at".into(), json!("2025-01-01T00:00:00"));
        row.insert("symlink_target".into(), json!("/foo/actual.py"));
        row.insert("language".into(), json!("python"));

        let canonical = canonicalize_row_keys(&row);
        assert_eq!(canonical.get("path"), Some(&json!("/foo/bar.py")));
        assert_eq!(
            canonical.get("indexed"),
            Some(&json!("2025-01-01T00:00:00"))
        );
        assert_eq!(canonical.get("symlink"), Some(&json!("/foo/actual.py")));
        assert_eq!(canonical.get("language"), Some(&json!("python")));
        assert!(!canonical.contains_key("logical_path"));
        assert!(!canonical.contains_key("indexed_at"));
        assert!(!canonical.contains_key("symlink_target"));
    }

    #[test]
    fn canonicalize_row_keys_preserves_non_renamed_fields() {
        let mut row = scat_core::core::db::JsonRow::new();
        row.insert("checkout_user".into(), json!("alice"));
        row.insert("checkout_os".into(), json!("linux"));
        row.insert("logical_path".into(), json!("/foo/bar.py"));

        let canonical = canonicalize_row_keys(&row);
        assert_eq!(canonical.get("path"), Some(&json!("/foo/bar.py")));
        assert_eq!(canonical.get("checkout_user"), Some(&json!("alice")));
        assert_eq!(canonical.get("checkout_os"), Some(&json!("linux")));
    }
}
