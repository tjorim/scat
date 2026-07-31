//! The Functions pane: indexed functions/methods for the selected script.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, List, ListItem};

use super::super::{Focus, TuiApp};
use super::common::{focus_border, render_list_pane, spinner_char};

pub(super) fn draw_functions(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    let border_style = focus_border(app.focus, Focus::Functions);

    if app.detail_loading {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Functions (loading…)")
            .border_style(border_style);
        let items = vec![ListItem::new(format!("{spinner} Loading…"))];
        frame.render_widget(List::new(items).block(block), area);
        return;
    }

    let labels: Vec<String> = app
        .functions
        .iter()
        .map(|function| {
            let doc = if function.docstring.is_empty() {
                "—"
            } else {
                function.docstring.as_str()
            };
            format!(
                "{:<20} {:<8} {:>4}  {}",
                function.name, function.kind, function.line, doc
            )
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Functions")
        .border_style(border_style);

    if labels.is_empty() {
        let items = vec![ListItem::new(ratatui::text::Span::styled(
            "No functions indexed.",
            Style::default().fg(Color::DarkGray),
        ))];
        frame.render_widget(List::new(items).block(block), area);
        return;
    }

    render_list_pane(
        frame,
        area,
        block,
        &labels,
        app.functions_selected,
        &mut app.functions_state,
    );
}
