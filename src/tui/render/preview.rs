//! The catalog-preview pane: the first `PREVIEW_LINES` of indexed content.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::{Focus, TuiApp};
use super::common::{clamp_scroll_offset, focus_border, line_count, spinner_char};

/// Title for the catalog preview pane. When the indexed script is longer than
/// `PREVIEW_LINES`, the preview is capped, so flag that and point at the
/// full-script viewer keys.
fn preview_title(scroll: u16, total_lines: usize) -> String {
    let line = scroll.saturating_add(1);
    if total_lines > super::super::PREVIEW_LINES {
        format!(
            "Catalog preview (line {line} — first {} of {total_lines} lines, v/V for full)",
            super::super::PREVIEW_LINES
        )
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
            line_count(app.cached_preview.as_str()),
            area,
        );
    }
    let (content, title) = if app.detail_loading {
        (
            Cow::Owned(format!("{spinner} Loading…")),
            "Preview (loading…)".to_string(),
        )
    } else if app.cached_preview.is_empty() && app.detail.is_some() {
        (
            Cow::Borrowed(""),
            preview_title(app.preview_scroll, app.preview_total_lines),
        )
    } else {
        (
            Cow::Borrowed(app.cached_preview.as_str()),
            preview_title(app.preview_scroll, app.preview_total_lines),
        )
    };
    let text: Text = if !app.detail_loading && content.is_empty() {
        Text::from(ratatui::text::Span::styled(
            "(empty)",
            ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
        ))
    } else {
        Text::from(content)
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
    fn preview_title_plain_when_not_truncated() {
        // Total lines within the cap: no truncation hint.
        assert_eq!(
            preview_title(0, super::super::super::PREVIEW_LINES),
            "Catalog preview (line 1)"
        );
        assert_eq!(preview_title(11, 0), "Catalog preview (line 12)");
    }

    #[test]
    fn preview_title_flags_truncation_and_points_at_viewer() {
        let total = super::super::super::PREVIEW_LINES + 1;
        let title = preview_title(4, total);
        assert!(title.contains("line 5"), "title: {title}");
        assert!(
            title.contains(&format!(
                "first {} of {total} lines",
                super::super::super::PREVIEW_LINES
            )),
            "title: {title}"
        );
        assert!(title.contains("v/V for full"), "title: {title}");
    }
}
