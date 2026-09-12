//! The Results pane: the virtualized list of matching scripts.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem};
use scat_core::core::script_view::{ScriptView, symlink_target_display};

use super::super::{Focus, TuiApp};
use super::common::{focus_border, render_window, scroll_window_variable, spinner_char};

/// Terminal rows a result renders as: 2 for a symlink (path, then an indented
/// `↳ target` line mirroring the CLI table's sub-row), 1 otherwise. Kept
/// separate from [`result_item`] so the windowing math in [`draw_results`]
/// can be answered without formatting every candidate row's text.
fn result_height(row: &scat_core::core::db::JsonRow) -> usize {
    if ScriptView::new(row).symlink_target().is_empty() {
        1
    } else {
        2
    }
}

/// Build one result's `ListItem` — one line normally, or two for a symlink
/// (its path, then an indented `↳ target` line, the same relationship the
/// CLI table shows as a `↳ <target>` sub-row). `area` is the pane's outer
/// (bordered) rect; the List widget lays rows out inside `block.inner(area)`.
fn result_item(row: &scat_core::core::db::JsonRow, area: Rect) -> ListItem<'static> {
    let view = ScriptView::new(row);
    let path = view.logical_path();
    let lang = view.language();
    let checkout = if view.checkout_user().is_empty() {
        ""
    } else {
        " CO"
    };
    let target = view.symlink_target();

    // Reserve: 2 (border) + 2 (highlight) + 2 (separator) + lang + checkout —
    // missing the border here previously let the last couple of characters
    // of every row (usually into `lang`/`checkout`) get silently clipped by
    // the widget.
    let max_name = (area.width as usize)
        .saturating_sub(2)
        .saturating_sub(2)
        .saturating_sub(2)
        .saturating_sub(lang.len())
        .saturating_sub(checkout.len());
    let display = super::common::left_truncate_path(path, max_name);
    let first_line = format!("{display}  {lang}{checkout}");

    if target.is_empty() {
        return ListItem::new(first_line);
    }

    let shown = symlink_target_display(path, target);
    // "  ↳ " is 4 display columns.
    let sub_max = (area.width as usize)
        .saturating_sub(2)
        .saturating_sub(2)
        .saturating_sub(4);
    let sub_line = format!("  ↳ {}", super::common::truncate_line(shown, sub_max));
    ListItem::new(Text::from(vec![
        Line::raw(first_line),
        Line::styled(sub_line, Style::default().fg(Color::DarkGray)),
    ]))
}

pub(super) fn draw_results(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    let title = format!("Results ({})", app.results.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(focus_border(app.focus, Focus::Results));

    if app.results.is_empty() {
        app.results_row_index.clear();
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

    // A symlink result renders as 2 rows (its `↳ target` sub-line), so the
    // window can't be sliced by flat item count the way a uniform pane's
    // can — `scroll_window_variable` walks `result_height` instead. Only the
    // window is turned into `ListItem`s and reformatted, instead of all of
    // `app.results` on every frame — the difference that lets the list stay
    // cheap to draw with thousands of results, not just the handful visible.
    let inner_height = area.height.saturating_sub(2) as usize;
    let len = app.results.len();
    let selected = app.selected.min(len - 1);
    let (offset, end) = scroll_window_variable(
        app.results_state.offset(),
        selected,
        len,
        inner_height,
        |i| result_height(&app.results[i]),
    );
    // Recorded here (rather than left to the widget) since only the window is
    // rendered below; `record_region_with_row_index` (after `draw_results`)
    // reads this back to map a mouse click to a full-list index.
    *app.results_state.offset_mut() = offset;

    let mut items: Vec<ListItem> = Vec::with_capacity(end - offset);
    app.results_row_index.clear();
    for (i, row) in app.results[offset..end].iter().enumerate() {
        let index = offset + i;
        for _ in 0..result_height(row) {
            app.results_row_index.push(index);
        }
        items.push(result_item(row, area));
    }

    render_window(frame, area, block, items, selected - offset);
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use super::{result_height, result_item};

    fn plain_row(path: &str, lang: &str) -> scat_core::core::db::JsonRow {
        let mut row = Map::new();
        row.insert("logical_path".into(), Value::String(path.to_string()));
        row.insert("language".into(), Value::String(lang.to_string()));
        row
    }

    fn symlink_row(path: &str, lang: &str, target: &str) -> scat_core::core::db::JsonRow {
        let mut row = plain_row(path, lang);
        row.insert("symlink_target".into(), Value::String(target.to_string()));
        row
    }

    fn render_lines(item: ratatui::widgets::ListItem<'static>, width: u16) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend};

        let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::List::new(vec![item.clone()]),
                    frame.area(),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..2)
            .map(|row| {
                (0..width)
                    .map(|col| buf[(col, row)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn result_height_is_one_for_a_plain_script() {
        assert_eq!(
            result_height(&plain_row("/catalog/scripts/tool.py", "python")),
            1
        );
    }

    #[test]
    fn result_height_is_two_for_a_symlink() {
        let row = symlink_row(
            "/catalog/scripts/prepare_release",
            "shell",
            "/catalog/scripts/prepare_release_20260729_140513",
        );
        assert_eq!(result_height(&row), 2);
    }

    #[test]
    fn results_pane_renders_the_symlink_target_on_its_own_line() {
        let row = symlink_row(
            "/shared/tools/scripts/source/prepare_release",
            "shell",
            "/shared/tools/scripts/source/prepare_release_20260729_140513",
        );
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        let item = result_item(&row, area);
        let lines = render_lines(item, 80);

        assert!(
            lines[0].contains("prepare_release") && !lines[0].contains('↳'),
            "first line should be the path, no sub-row marker: {lines:?}"
        );
        assert!(
            lines[1].contains("↳") && lines[1].contains("prepare_release_20260729_140513"),
            "second line should be the ↳ target sub-row: {lines:?}"
        );
    }

    #[test]
    fn plain_script_renders_no_second_line() {
        let row = plain_row("/catalog/scripts/tool.py", "python");
        let area = ratatui::layout::Rect::new(0, 0, 80, 10);
        let item = result_item(&row, area);
        let lines = render_lines(item, 80);
        assert!(lines[0].contains("tool.py"));
        assert!(
            lines[1].trim().is_empty(),
            "a plain script must not render a second line: {lines:?}"
        );
    }

    #[test]
    fn result_item_lines_fit_inside_the_bordered_pane_width() {
        // `result_item` is handed the pane's *outer* (bordered) rect, but
        // it's always drawn inside a `Borders::ALL` block plus a 2-column
        // highlight-symbol reservation. Its width budget must account for
        // both, or the last couple of characters (often into `lang`/
        // `checkout`, or the sub-row's target) get silently clipped by the
        // widget.
        let row = symlink_row(
            "/very/long/catalog/of/scripts/tools/prepare_release_for_deployment.py",
            "python",
            "/very/long/catalog/of/scripts/tools/prepare_release_for_deployment.py_20260729_140513",
        );
        let area = ratatui::layout::Rect::new(0, 0, 40, 10);
        let item = result_item(&row, area);
        let inner_width = area.width as usize - 2 /* border */ - 2 /* highlight symbol */;
        assert!(
            item.width() <= inner_width,
            "widest line ({} chars) overflows the {inner_width}-column inner width",
            item.width()
        );
    }
}
