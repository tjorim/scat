//! Full-screen views: the script's catalog detail, and its diff.

use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::super::{RegionKind, TuiApp, detail};
use super::common::{clamp_scroll_offset, hint_key, line_count, spinner_char};

pub(super) fn draw_detail_view(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
        .split(frame.area());

    let lines = detail::detail_lines(app);
    // While folder-browse mode is active, keep the selected Folder entry
    // scrolled into view so j/k selection never moves invisibly off-screen.
    if app.folder_focused
        && let Some(selected_line) = detail::folder_selected_line(app, &lines)
    {
        let viewport = root[0].height.saturating_sub(2).max(1);
        if selected_line < app.detail_scroll {
            app.detail_scroll = selected_line;
        } else if selected_line >= app.detail_scroll.saturating_add(viewport) {
            app.detail_scroll = selected_line.saturating_sub(viewport - 1);
        }
    }
    clamp_scroll_offset(&mut app.detail_scroll, lines.len(), root[0]);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(
                        "Script Detail (line {})",
                        app.detail_scroll.saturating_add(1)
                    ))
                    .border_style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
            ),
        root[0],
    );
    app.record_region(
        root[0],
        RegionKind::DetailBody,
        usize::from(app.detail_scroll),
    );
    let hint = if let Some(flash) = &app.flash {
        Line::from(Span::styled(
            flash.clone(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(vec![
            hint_key("d"),
            Span::raw(" diff  "),
            hint_key("v"),
            Span::raw(" view catalog  "),
            hint_key("V"),
            Span::raw(" view source  "),
            hint_key("Tab"),
            Span::raw(" browse folder  "),
            hint_key("click Path"),
            Span::raw(" copy  "),
            hint_key("Esc"),
            Span::raw(" back  "),
            hint_key("j/k"),
            Span::raw(" scroll  "),
            hint_key("q"),
            Span::raw(" quit"),
        ])
    };
    frame.render_widget(Paragraph::new(hint), root[1]);
}

pub(super) fn draw_detail_diff_view(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
        .split(frame.area());

    let spinner = spinner_char(app.tick);
    if app.detail_diff_loading {
        app.detail_diff_scroll = 0;
    } else {
        clamp_scroll_offset(
            &mut app.detail_diff_scroll,
            line_count(app.detail_diff_output.as_str()),
            root[0],
        );
    }
    let (content, title) = if app.detail_diff_loading {
        (
            Cow::Owned(format!("{spinner} Loading diff…")),
            "Script Diff (loading…)".to_string(),
        )
    } else {
        (
            Cow::Borrowed(app.detail_diff_output.as_str()),
            format!(
                "Script Diff (line {})",
                app.detail_diff_scroll.saturating_add(1)
            ),
        )
    };
    frame.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .scroll((
                if app.detail_diff_loading {
                    0
                } else {
                    app.detail_diff_scroll
                },
                0,
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
            ),
        root[0],
    );
    app.record_region(
        root[0],
        RegionKind::DetailDiffBody,
        usize::from(app.detail_diff_scroll),
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            hint_key("Esc/Backspace"),
            Span::raw(" back  "),
            hint_key("j/k"),
            Span::raw(" scroll  "),
            hint_key("Ctrl+u/d"),
            Span::raw(" half-page  "),
            hint_key("Ctrl+b/f"),
            Span::raw(" page  "),
            hint_key("g"),
            Span::raw(" top  "),
            hint_key("q"),
            Span::raw(" quit"),
        ])),
        root[1],
    );
}
