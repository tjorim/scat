//! The search input box.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::super::{Focus, TuiApp};
use super::common::focus_border;

pub(super) fn draw_search(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let mut title =
        super::super::search_title(app.error.is_some(), app.search_in_flight).to_string();
    if !app.filter_labels.is_empty() {
        title = format!("{title} [{}]", app.filter_labels.join(" "));
    }
    let text = if app.query.is_empty() {
        Text::from(Line::from(Span::styled(
            "type to search — lang:/owner:/tag: to filter, Enter for results",
            Style::default().fg(Color::DarkGray),
        )))
    } else {
        Text::from(app.query.as_str())
    };
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(focus_border(app.focus, Focus::Search)),
        ),
        area,
    );
}
