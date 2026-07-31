//! The Metadata pane: path, language, owner, contributors, purpose, etc.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use scat_core::core::script_view::ScriptView;

use super::super::{TuiApp, detail};
use super::common::spinner_char;

pub(super) fn draw_metadata(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let inactive = Style::default().fg(Color::DarkGray);
    if app.detail_loading {
        let spinner = spinner_char(app.tick);
        frame.render_widget(
            Paragraph::new(format!("{spinner} Loading…")).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Metadata")
                    .border_style(inactive),
            ),
            area,
        );
        return;
    }
    let Some(row) = app.detail.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No script selected.",
                Style::default().fg(Color::DarkGray),
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Metadata")
                    .border_style(inactive),
            ),
            area,
        );
        return;
    };
    let view = ScriptView::new(row);
    let warnings = detail::warning_summary(row);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Path      ", detail::label_style()),
            Span::raw(view.logical_path().to_string()),
        ]),
        Line::from(vec![
            Span::styled("Language  ", detail::label_style()),
            Span::raw(view.language().to_string()),
        ]),
        Line::from(vec![
            Span::styled("Owner     ", detail::label_style()),
            Span::raw(detail::display_text(view.owner())),
        ]),
        Line::from(vec![
            Span::styled("Contribs  ", detail::label_style()),
            Span::raw(if app.contributors.is_empty() {
                "—".to_string()
            } else {
                app.contributors.join(", ")
            }),
        ]),
        Line::from(vec![
            Span::styled("Purpose   ", detail::label_style()),
            Span::raw(detail::display_text(view.purpose())),
        ]),
        Line::from(vec![
            Span::styled("Checkout  ", detail::label_style()),
            Span::raw(view.checkout_label()),
        ]),
    ];
    if !view.symlink_target().is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Symlink   ", detail::label_style()),
            Span::raw(format!("→ {}", view.symlink_target())),
        ]));
    }
    if !warnings.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Warnings  ", detail::label_style()),
            Span::raw(warnings),
        ]));
    }
    if let Some(native) = detail::native_path_for_row(row, &app.resolver) {
        lines.push(Line::from(vec![
            Span::styled("OS path   ", detail::label_style()),
            Span::raw(native),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Metadata")
                .border_style(inactive),
        ),
        area,
    );
}
