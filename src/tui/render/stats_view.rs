//! Full-screen catalog stats view (`ViewMode::Stats`, opened with `s`): four
//! horizontal bar charts (by language, by owner, top tags, most functions
//! per script) — a direct visual restatement of `scat catalog stats`'s text
//! tables, laid out as a 2x2 grid.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, Block, Borders, Paragraph};

use super::super::TuiApp;
use super::common::{hint_key, left_truncate_path, spinner_char};

/// Cap on bars shown per chart. A catalog can have far more distinct owners
/// (or scripts) than fit legibly in a quarter-screen chart; the top N by
/// count is what matters for a "what's dominant" glance, same reasoning as
/// `StatsResult`'s own ranking caps.
const MAX_BARS: usize = 8;

/// Cap on a bar's label width, for the two charts (`most_functions`) whose
/// labels are full logical paths rather than short language/owner/tag
/// strings.
const MAX_LABEL_CHARS: usize = 28;

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
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(body);
        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[0]);
        let bottom = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(rows[1]);

        let by_language: Vec<(String, i64)> = stats
            .by_language
            .iter()
            .map(|l| (l.language.clone(), l.count))
            .collect();
        let by_owner: Vec<(String, i64)> = stats
            .by_owner
            .iter()
            .map(|o| (o.owner.clone(), o.count))
            .collect();
        let top_tags: Vec<(String, i64)> = stats
            .top_tags
            .iter()
            .map(|t| (t.tag.clone(), t.count))
            .collect();
        let most_functions: Vec<(String, i64)> = stats
            .most_functions
            .iter()
            .map(|f| {
                (
                    left_truncate_path(&f.logical_path, MAX_LABEL_CHARS),
                    f.count,
                )
            })
            .collect();

        draw_bar_chart(frame, top[0], "By language", &by_language);
        draw_bar_chart(frame, top[1], "By owner", &by_owner);
        draw_bar_chart(frame, bottom[0], "Top tags", &top_tags);
        draw_bar_chart(frame, bottom[1], "Most functions", &most_functions);
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
fn draw_bar_chart(frame: &mut Frame<'_>, area: Rect, title: &str, data: &[(String, i64)]) {
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
        .map(|(label, count)| Bar::with_label(label.clone(), u64::try_from(*count).unwrap_or(0)))
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
