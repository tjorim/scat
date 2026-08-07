//! Full-screen catalog stats view (`ViewMode::Stats`, opened with `s`):
//! by-language and by-owner distribution rendered as horizontal bar charts,
//! a direct visual restatement of `scat catalog stats`'s text tables.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, Block, Borders, Paragraph};

use super::super::TuiApp;
use super::common::{hint_key, spinner_char};

/// Cap on bars shown per chart. A catalog can have far more distinct owners
/// than fit legibly in a terminal-height chart; the top N by count is what
/// matters for a "what's dominant" glance, same reasoning as
/// `StatsResult::most_depended_upon`'s cap.
const MAX_BARS: usize = 12;

pub(super) fn draw_stats_view(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let title = match (&app.stats, app.stats_loading) {
        (Some(stats), _) => format!("Catalog Stats — {} scripts", stats.total_scripts),
        (None, true) => format!("{} Loading stats…", spinner_char(app.tick)),
        (None, false) => "Catalog Stats".to_string(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        root[0],
    );

    let body = root[1];
    if let Some(err) = &app.stats_error {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                err.clone(),
                Style::default().fg(Color::Red),
            )))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Catalog Stats"),
            ),
            body,
        );
    } else if let Some(stats) = &app.stats {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(body);
        let by_language: Vec<(&str, i64)> = stats
            .by_language
            .iter()
            .map(|l| (l.language.as_str(), l.count))
            .collect();
        let by_owner: Vec<(&str, i64)> = stats
            .by_owner
            .iter()
            .map(|o| (o.owner.as_str(), o.count))
            .collect();
        draw_bar_chart(frame, columns[0], "By language", &by_language);
        draw_bar_chart(frame, columns[1], "By owner", &by_owner);
    } else {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("Catalog Stats"),
            body,
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            hint_key("Esc/Backspace"),
            Span::raw(" back  "),
            hint_key("q"),
            Span::raw(" quit"),
        ])),
        root[2],
    );
}

/// Draw one horizontal bar chart of `(label, count)` pairs, already sorted
/// count-descending by the caller (matching `StatsResult`'s SQL ordering),
/// truncated to [`MAX_BARS`] with the title noting how many were cut.
fn draw_bar_chart(frame: &mut Frame<'_>, area: Rect, title: &str, data: &[(&str, i64)]) {
    if data.is_empty() {
        frame.render_widget(
            Paragraph::new("No data.").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title.to_string()),
            ),
            area,
        );
        return;
    }
    let shown = data.len().min(MAX_BARS);
    let bars: Vec<Bar<'_>> = data[..shown]
        .iter()
        .map(|(label, count)| Bar::with_label(*label, u64::try_from(*count).unwrap_or(0)))
        .collect();
    let full_title = if data.len() > shown {
        format!("{title} (top {shown} of {})", data.len())
    } else {
        title.to_string()
    };
    let chart = BarChart::horizontal(bars)
        .bar_width(1)
        .bar_gap(1)
        .bar_style(Style::default().fg(Color::Cyan))
        .value_style(Style::default().fg(Color::Black).bg(Color::Cyan))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(full_title)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    frame.render_widget(chart, area);
}
