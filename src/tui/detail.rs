use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use scat_core::core::db::{JsonRow, row_display};
use scat_core::core::resolve::PathResolver;
use scat_core::core::script_view::{ListField, ScriptView};

use super::{PREVIEW_LINES, TuiApp};

pub(super) fn detail_lines(app: &TuiApp) -> Vec<Line<'static>> {
    if app.detail_loading {
        return vec![Line::from("Loading…")];
    }
    let Some(row) = app.detail.as_ref() else {
        return vec![Line::from("No script selected.")];
    };
    let view = ScriptView::new(row);

    let size = view
        .size()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "—".to_string());
    let mut lines = vec![
        section("Script"),
        field_line("Path", view.logical_path().to_string()),
        field_line("Language", display_text(view.language())),
        field_line("Owner", display_text(view.owner())),
        field_line("Purpose", display_text(view.purpose())),
        field_line("Size", format!("{size} bytes")),
        field_line("Indexed", display_text(view.indexed_at())),
        field_line("Checkout", view.checkout_label()),
    ];
    if let Some(native) = native_path_for_row(row, &app.resolver) {
        lines.push(field_line("OS path", native));
    }

    for (label, field) in [
        ("Tags", ListField::Tags),
        ("Entry points", ListField::EntryPoints),
        ("Related metadata", ListField::Related),
    ] {
        let values = view.string_list(field);
        if !values.is_empty() {
            lines.push(field_line(label, values.join(", ")));
        }
    }

    let warnings = view.vc_warning_messages();
    if !warnings.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Warnings"));
        for warning in warnings {
            lines.push(bullet_line(warning));
        }
    }

    let parent_dir = app
        .folder_dir
        .clone()
        .unwrap_or_else(|| view.parent_dir().to_string());
    if !parent_dir.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Folder"));
        lines.push(field_line("Directory", parent_dir.clone()));
        if app.siblings.is_empty() {
            lines.push(Line::from("  (no other scripts in this folder)"));
        } else {
            for (idx, sibling) in app.siblings.iter().enumerate() {
                let sibling_path = ScriptView::new(sibling).logical_path();
                let name = sibling_path
                    .rsplit_once('/')
                    .map_or(sibling_path, |(_, name)| name);
                let marker = if app.folder_focused && idx == app.siblings_selected {
                    "> "
                } else {
                    "  "
                };
                lines.push(Line::from(format!("{marker}- {name}")));
            }
        }
        lines.push(folder_hint_line(app.folder_focused, parent_dir == "/"));
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

    let content = view.content();
    if !content.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Preview"));
        for line in content.lines().take(PREVIEW_LINES) {
            lines.push(Line::from(line.to_string()));
        }
    }

    lines
}

/// Format a `scripts` row field with the [`row_display`] semantics used by the
/// detail/metadata panes (numbers stringified, empty values shown as `—`).
///
/// Retained for row shapes that are not `scripts` rows (for example the
/// `revisions` join rows rendered in the checkouts section).
pub(super) fn display_field(row: &JsonRow, key: &str) -> String {
    row_display(row, key, "—")
}

/// Substitute `—` for an empty string, otherwise return the value owned.
///
/// Matches [`row_display`] for the text `scripts` columns shown in the panes.
pub(super) fn display_text(value: &str) -> String {
    if value.is_empty() {
        "—".to_string()
    } else {
        value.to_string()
    }
}

pub(super) fn native_path_for_row(row: &JsonRow, resolver: &PathResolver) -> Option<String> {
    let path = ScriptView::new(row).logical_path();
    if path.is_empty() {
        return None;
    }
    let native = resolver.to_native(path);
    if native == path { None } else { Some(native) }
}

pub(super) fn warning_summary(row: &JsonRow) -> String {
    ScriptView::new(row).vc_warning_messages().join("; ")
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

/// Hint line shown under the Folder section's sibling list, reflecting
/// whether browse mode is active and whether "go up" has anywhere to go.
fn folder_hint_line(focused: bool, at_root: bool) -> Line<'static> {
    let hint = if !focused {
        "  [Tab] browse folder".to_string()
    } else if at_root {
        "  [Tab] exit browse  [j/k] move  [Enter] open  [Backspace] back  (at root)".to_string()
    } else {
        "  [Tab] exit browse  [j/k] move  [Enter] open  [[] up  [Backspace] back".to_string()
    };
    Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray)))
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use scat_core::core::script_view::{ListField, ScriptView};

    use super::warning_summary;

    #[test]
    fn parses_string_arrays_for_detail_view() {
        let mut row = Map::new();
        row.insert(
            "tags".to_string(),
            Value::String(json!(["one", "two"]).to_string()),
        );
        assert_eq!(
            ScriptView::new(&row).string_list(ListField::Tags),
            vec!["one", "two"]
        );
    }

    #[test]
    fn parses_warning_messages_for_detail_view() {
        let mut row = Map::new();
        row.insert(
            "vc_warnings".to_string(),
            Value::String(json!([{"message": "stale checkout"}]).to_string()),
        );
        assert_eq!(warning_summary(&row), "stale checkout");
    }
}
