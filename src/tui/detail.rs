use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use scat_core::core::db::{JsonRow, row_display, row_string as str_field};
use scat_core::core::resolve::PathResolver;

use super::{PREVIEW_LINES, TuiApp};

pub(super) fn detail_lines(app: &TuiApp) -> Vec<Line<'static>> {
    if app.detail_loading {
        return vec![Line::from("Loading…")];
    }
    let Some(row) = app.detail.as_ref() else {
        return vec![Line::from("No script selected.")];
    };

    let mut lines = vec![
        section("Script"),
        field_line("Path", str_field(row, "logical_path")),
        field_line("Language", display_field(row, "language")),
        field_line("Owner", display_field(row, "owner")),
        field_line("Purpose", display_field(row, "purpose")),
        field_line("Size", format!("{} bytes", display_field(row, "size"))),
        field_line("Indexed", display_field(row, "indexed_at")),
        field_line("Checkout", checkout_label(row)),
    ];
    if let Some(native) = native_path_for_row(row, &app.resolver) {
        lines.push(field_line("OS path", native));
    }

    for (label, key) in [
        ("Tags", "tags"),
        ("Entry points", "entry_points"),
        ("Related metadata", "related"),
    ] {
        let values = json_string_array(row, key);
        if !values.is_empty() {
            lines.push(field_line(label, values.join(", ")));
        }
    }

    let warnings = warning_messages(row);
    if !warnings.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Warnings"));
        for warning in warnings {
            lines.push(bullet_line(warning));
        }
    }

    if !app.deps.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Dependencies"));
        for item in &app.deps {
            lines.push(bullet_line(format!("{} {}", item.kind, item.logical_path)));
        }
    }

    if !app.checkouts.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Checkouts"));
        for checkout in &app.checkouts {
            let user = display_field(checkout, "user");
            let os = display_field(checkout, "os_flavor");
            let timestamp = display_field(checkout, "timestamp");
            let path = display_field(checkout, "physical_path");
            lines.push(bullet_line(format!("{user} on {os} since {timestamp}")));
            lines.push(Line::from(format!("    {path}")));
        }
    }

    let content = str_field(row, "content");
    if !content.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Preview"));
        for line in content.lines().take(PREVIEW_LINES) {
            lines.push(Line::from(line.to_string()));
        }
    }

    lines
}

pub(super) fn display_field(row: &JsonRow, key: &str) -> String {
    row_display(row, key, "-")
}

pub(super) fn checkout_label(row: &JsonRow) -> String {
    let user = str_field(row, "checkout_user");
    if user.is_empty() {
        return "clean".to_string();
    }
    let timestamp = str_field(row, "checkout_timestamp");
    if timestamp.is_empty() {
        format!("checked out by {user}")
    } else {
        format!("checked out by {user} since {timestamp}")
    }
}

pub(super) fn native_path_for_row(row: &JsonRow, resolver: &PathResolver) -> Option<String> {
    let path = row.get("logical_path")?.as_str()?;
    if path.is_empty() {
        return None;
    }
    let native = resolver.to_native(path);
    if native == path { None } else { Some(native) }
}

pub(super) fn warning_summary(row: &JsonRow) -> String {
    warning_messages(row).join("; ")
}

pub(super) fn warning_messages(row: &JsonRow) -> Vec<String> {
    let raw = str_field(row, "vc_warnings");
    let Ok(Value::Array(warnings)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    warnings
        .iter()
        .filter_map(|warning| warning.get("message").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

pub(super) fn json_string_array(row: &JsonRow, key: &str) -> Vec<String> {
    let raw = str_field(row, key);
    let Ok(Value::Array(values)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

pub(super) fn label_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn field_line(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), label_style()),
        Span::raw(value),
    ])
}

fn bullet_line(value: String) -> Line<'static> {
    Line::from(format!("  - {value}"))
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::{json_string_array, warning_messages};

    #[test]
    fn parses_string_arrays_for_detail_view() {
        let mut row = Map::new();
        row.insert(
            "tags".to_string(),
            Value::String(json!(["one", "two"]).to_string()),
        );
        assert_eq!(json_string_array(&row, "tags"), vec!["one", "two"]);
    }

    #[test]
    fn parses_warning_messages_for_detail_view() {
        let mut row = Map::new();
        row.insert(
            "vc_warnings".to_string(),
            Value::String(json!([{"message": "stale checkout"}]).to_string()),
        );
        assert_eq!(warning_messages(&row), vec!["stale checkout"]);
    }
}
