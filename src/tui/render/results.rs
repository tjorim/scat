//! The Results pane: the virtualized list of matching scripts.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, List, ListItem};
use scat_core::core::script_view::{ScriptView, symlink_target_display};

use super::super::{Focus, TuiApp};
use super::common::{focus_border, render_window, scroll_window, spinner_char};

/// One line of the results pane: the script's path, the target it symlinks to
/// when it is one, then language and checkout markers.
fn result_line(row: &scat_core::core::db::JsonRow, area: Rect) -> String {
    let view = ScriptView::new(row);
    let path = view.logical_path();
    let lang = view.language();
    let checkout = if view.checkout_user().is_empty() {
        ""
    } else {
        " CO"
    };
    // A symlink's own row carries the arrow, the same relationship the CLI
    // table shows as a `↳ <target>` sub-row. Rows here are selectable entries
    // backed by a script, so the target is annotated in place rather than
    // added as a row of its own.
    let arrow = symlink_arrow(path, view.symlink_target());
    // `area` is the pane's outer (bordered) rect, but the List widget lays
    // rows out inside `block.inner(area)`. Reserve: 2 (border) + 2
    // (highlight) + 2 (separator) + lang + checkout + arrow — missing the
    // border here previously let the last couple of characters of every row
    // (usually into `lang`/`checkout`) get silently clipped by the widget.
    let max_name = (area.width as usize)
        .saturating_sub(2)
        .saturating_sub(2)
        .saturating_sub(2)
        .saturating_sub(lang.len())
        .saturating_sub(checkout.len())
        .saturating_sub(arrow.chars().count());
    let display = super::common::left_truncate_path(path, max_name);
    format!("{display}{arrow}  {lang}{checkout}")
}

/// Render a symlink's target as a ` → target` suffix, or an empty string when
/// the script is not a symlink. Counterpart to the CLI table's `↳` sub-row.
fn symlink_arrow(path: &str, target: &str) -> String {
    if target.is_empty() {
        return String::new();
    }
    format!(" → {}", symlink_target_display(path, target))
}

pub(super) fn draw_results(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    let title = format!("Results ({})", app.results.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(focus_border(app.focus, Focus::Results));

    if app.results.is_empty() {
        let items = if app.search_in_flight {
            vec![ListItem::new(format!("{spinner} Searching…"))]
        } else {
            vec![ListItem::new(ratatui::text::Span::styled(
                "No results.",
                Style::default().fg(Color::DarkGray),
            ))]
        };
        frame.render_widget(List::new(items).block(block), area);
        return;
    }

    // Every row renders as exactly one line, so the visible window can be
    // computed directly (no variable item heights to walk). Only that window
    // is turned into `ListItem`s and reformatted, instead of all of
    // `app.results` on every frame — the difference that lets the list stay
    // cheap to draw with thousands of results, not just the handful visible.
    let inner_height = area.height.saturating_sub(2) as usize;
    let len = app.results.len();
    let selected = app.selected.min(len - 1);
    let offset = scroll_window(app.results_state.offset(), selected, len, inner_height);
    // Recorded here (rather than left to the widget) since only the window is
    // rendered below; `record_region` calls after `draw_results` read this to
    // map a mouse click back to a full-list index.
    *app.results_state.offset_mut() = offset;

    let end = (offset + inner_height.max(1)).min(len);
    let items: Vec<ListItem> = app.results[offset..end]
        .iter()
        .map(|row| ListItem::new(result_line(row, area)))
        .collect();

    render_window(frame, area, block, items, selected - offset);
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use super::{result_line, symlink_arrow};

    #[test]
    fn results_pane_renders_the_symlink_arrow_on_screen() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut row = Map::new();
        row.insert(
            "logical_path".into(),
            Value::String("/shared/tools/scripts/source/prepare_release".into()),
        );
        row.insert("language".into(), Value::String("shell".into()));
        row.insert(
            "symlink_target".into(),
            Value::String("/shared/tools/scripts/source/prepare_release_20260729_140513".into()),
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let items = vec![ratatui::widgets::ListItem::new(result_line(&row, area))];
                frame.render_widget(ratatui::widgets::List::new(items), area);
            })
            .unwrap();

        let rendered: String =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut acc, cell| {
                    acc.push_str(cell.symbol());
                    acc
                });
        assert!(
            rendered.contains("→ prepare_release_20260729_140513"),
            "results pane must show the symlink target: {rendered:?}"
        );
    }

    #[test]
    fn result_line_fits_inside_the_bordered_pane_width() {
        // `result_line` is handed the pane's *outer* (bordered) rect, but it's
        // always drawn inside a `Borders::ALL` block plus a 2-column
        // highlight-symbol reservation. Its width budget must account for
        // both, or the last couple of characters (often into `lang`/
        // `checkout`) get silently clipped by the widget.
        let mut row = Map::new();
        row.insert(
            "logical_path".into(),
            Value::String(
                "/very/long/catalog/of/scripts/tools/prepare_release_for_deployment.py".into(),
            ),
        );
        row.insert("language".into(), Value::String("python".into()));
        row.insert("checkout_user".into(), Value::String("alice".into()));

        let area = ratatui::layout::Rect::new(0, 0, 40, 10);
        let line = result_line(&row, area);
        let inner_width = area.width as usize - 2 /* border */ - 2 /* highlight symbol */;
        assert!(
            line.chars().count() <= inner_width,
            "line {line:?} ({} chars) overflows the {inner_width}-column inner width",
            line.chars().count()
        );
    }

    #[test]
    fn symlink_arrow_is_empty_for_a_plain_script() {
        assert_eq!(symlink_arrow("/catalog/scripts/tool.py", ""), "");
    }

    #[test]
    fn symlink_arrow_shows_bare_name_for_a_sibling_target() {
        assert_eq!(
            symlink_arrow(
                "/catalog/scripts/prepare_release",
                "/catalog/scripts/prepare_release_20260729_140513"
            ),
            " → prepare_release_20260729_140513"
        );
    }

    #[test]
    fn symlink_arrow_keeps_full_path_for_a_target_elsewhere() {
        assert_eq!(
            symlink_arrow("/catalog/scripts/tool.py", "/catalog/shared/tool_v2.py"),
            " → /catalog/shared/tool_v2.py"
        );
    }
}
