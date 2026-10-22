//! The search input box.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::super::{Focus, TuiApp};
use super::common::focus_border;

pub(super) fn draw_search(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let mut title =
        super::super::search_title(app.error.is_some(), app.search.is_loading()).to_string();
    if !app.filter_labels.is_empty() {
        title = format!("{title} [{}]", app.filter_labels.join(" "));
    }
    title.push_str(&format!(
        " | {} limit={} F6 settings",
        app.search_options.mode.label(),
        app.result_limit
    ));
    for (key, value) in [
        ("role", &app.search_options.role),
        ("lang", &app.search_options.lang),
        ("owner", &app.search_options.owner),
        ("tag", &app.search_options.tag),
    ] {
        if let Some(value) = value {
            title.push_str(&format!(" {key}={value}"));
        }
    }
    let inner = area.inner(ratatui::layout::Margin::new(1, 1));
    // Preserve the viewport while clicking/dragging; scroll only when needed.
    let limit = app.query_view_start.min(app.query_cursor);
    let mut start = app
        .query
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|&index| index <= limit)
        .last()
        .unwrap_or(0);
    if app.query[start..app.query_cursor].width() >= usize::from(inner.width) {
        start = app.query_cursor;
        for (index, _) in app.query[..app.query_cursor].grapheme_indices(true).rev() {
            if app.query[index..app.query_cursor].width() >= usize::from(inner.width) {
                break;
            }
            start = index;
        }
    }
    app.query_view_start = start;
    app.query_area = inner;
    let text = if app.query.is_empty() {
        Text::from(Line::from(Span::styled(
            "type to search — lang:/owner:/tag: to filter, Enter for results",
            Style::default().fg(Color::DarkGray),
        )))
    } else {
        let selection = app.query_selection();
        Text::from(Line::from(
            app.query[start..]
                .grapheme_indices(true)
                .map(|(index, text)| {
                    let selected = selection
                        .as_ref()
                        .is_some_and(|range| range.contains(&(start + index)));
                    if selected {
                        Span::styled(text, Style::default().fg(Color::Black).bg(Color::Cyan))
                    } else {
                        Span::raw(text)
                    }
                })
                .collect::<Vec<_>>(),
        ))
    };
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(app.expanded_query.clone().unwrap_or_default())
                .border_style(focus_border(app.focus, Focus::Search)),
        ),
        area,
    );
    if app.focus == Focus::Search && inner.width > 0 && inner.height > 0 {
        let offset = app.query[start..app.query_cursor].width() as u16;
        frame.set_cursor_position((inner.x + offset, inner.y));
    }
}
