//! The top header line: brand, selected script path, fullscreen indicator.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use scat_core::core::script_view::ScriptView;

use super::super::TuiApp;
use super::common::left_truncate_path;

pub(super) fn draw_header(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let brand = Span::styled(
        " scat ",
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let path_text = app
        .detail
        .as_ref()
        .map(ScriptView::new)
        .and_then(|view| view.logical_path_value())
        .and_then(serde_json::Value::as_str)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let avail = (area.width as usize).saturating_sub(9);
            left_truncate_path(p, avail)
        });

    let mut spans = vec![brand, Span::raw("  ")];
    if let Some(path) = path_text {
        spans.push(Span::styled(path, Style::default().fg(Color::White)));
    }
    if app.fullscreen {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[FULLSCREEN]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
