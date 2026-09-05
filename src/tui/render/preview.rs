//! The catalog-preview pane: the full indexed content, scrollable.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::{Focus, TuiApp};
use super::common::{clamp_scroll_offset, focus_border, spinner_char};

/// Title for the catalog preview pane: current scroll position and the
/// script's total line count, when known.
fn preview_title(scroll: u16, total_lines: usize) -> String {
    let line = scroll.saturating_add(1);
    if total_lines > 0 {
        format!("Catalog preview (line {line} of {total_lines})")
    } else {
        format!("Catalog preview (line {line})")
    }
}

pub(super) fn draw_preview(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    if app.detail_loading {
        app.preview_scroll = 0;
    } else {
        clamp_scroll_offset(
            &mut app.preview_scroll,
            app.cached_preview_lines.len().max(1),
            area,
        );
    }
    let title = if app.detail_loading {
        "Preview (loading…)".to_string()
    } else {
        preview_title(app.preview_scroll, app.preview_total_lines)
    };
    let text: Text = if app.detail_loading {
        Text::from(format!("{spinner} Loading…"))
    } else if app.cached_preview_lines.is_empty() && app.detail.is_some() {
        Text::from(Span::styled(
            "(empty)",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Text::from(app.cached_preview_lines.clone())
    };
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((
                if app.detail_loading {
                    0
                } else {
                    app.preview_scroll
                },
                0,
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(focus_border(app.focus, Focus::Preview)),
            ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::preview_title;

    #[test]
    fn preview_title_omits_total_when_unknown() {
        // total_lines == 0 covers "no detail loaded yet" / empty content.
        assert_eq!(preview_title(11, 0), "Catalog preview (line 12)");
    }

    #[test]
    fn preview_title_shows_scroll_position_and_total_lines() {
        let title = preview_title(4, 900);
        assert_eq!(title, "Catalog preview (line 5 of 900)");
    }
}
