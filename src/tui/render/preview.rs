//! The catalog-preview pane: the full indexed content, scrollable.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};

use super::super::wrapped_text::WrappedText;
use super::super::{Focus, TuiApp};
use super::common::{draw_wrapped, focus_border, spinner_char};

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
    }
    let mut placeholder = WrappedText::default();
    let (head, tail) = if app.detail_loading {
        (
            vec![Line::from(format!("{spinner} Loading…"))],
            &mut placeholder,
        )
    } else if app.preview_text.is_empty() && app.detail.is_some() {
        (
            vec![Line::from(Span::styled(
                "(empty)",
                Style::default().fg(Color::DarkGray),
            ))],
            &mut placeholder,
        )
    } else {
        (Vec::new(), &mut app.preview_text)
    };
    let loading = app.detail_loading;
    let total_lines = app.preview_total_lines;
    let border_style = focus_border(app.focus, Focus::Preview);
    draw_wrapped(frame, area, &mut app.preview_scroll, head, tail, |scroll| {
        let title = if loading {
            "Preview (loading…)".to_string()
        } else {
            preview_title(scroll, total_lines)
        };
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border_style)
    });
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
