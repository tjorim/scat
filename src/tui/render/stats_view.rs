//! Full-screen catalog stats view (`ViewMode::Stats`, opened with `s`): a
//! direct visual restatement of `scat catalog stats`'s text tables, spread
//! across [`super::super::STATS_PAGE_COUNT`] pages (Left/Right or h/l to
//! cycle):
//!
//! 1. Overview — by-language, by-owner, top-tags, most-functions bar charts.
//! 2. Checkouts & Size — checkout OS-flavor, most-active checkout users,
//!    checkout-staleness and script-size histograms.
//! 3. Activity & Audit — a GitHub-contributions-style checkout activity
//!    calendar, plus an audit-findings-by-check bar chart.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::calendar::{CalendarEventStore, Monthly};
use ratatui::widgets::{Bar, BarChart, Block, Borders, Paragraph};

use scat_core::core::search::{DailyCount, StatsResult};

use super::super::{STATS_PAGE_COUNT, TuiApp};
use super::common::{hint_key, left_truncate_path, spinner_char};

/// Cap on bars shown per chart. A catalog can have far more distinct owners
/// (or scripts) than fit legibly in a quarter-screen chart; the top N by
/// count is what matters for a "what's dominant" glance, same reasoning as
/// `StatsResult`'s own ranking caps.
const MAX_BARS: usize = 8;

/// Cap on a bar's label width, for the charts (`most_functions`) whose
/// labels are full logical paths rather than short language/owner/tag/etc
/// strings.
const MAX_LABEL_CHARS: usize = 28;

/// Number of months shown side by side in the activity calendar.
const CALENDAR_MONTHS: i32 = 3;

const PAGE_TITLES: [&str; STATS_PAGE_COUNT] = ["Overview", "Checkouts & Size", "Activity & Audit"];

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
        (Some(stats), _) => format!(
            "Catalog Stats — {} scripts ({}/{} {})",
            stats.total_scripts,
            app.stats_page + 1,
            STATS_PAGE_COUNT,
            PAGE_TITLES[app.stats_page]
        ),
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
        match app.stats_page {
            0 => draw_overview_page(frame, body, stats),
            1 => draw_checkouts_page(frame, body, stats),
            _ => draw_activity_page(frame, body, stats),
        }
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
            hint_key("Left/Right"),
            Span::raw(" page  "),
            hint_key("Esc/Backspace"),
            Span::raw(" back  "),
            hint_key("q"),
            Span::raw(" quit"),
        ])),
        root[2],
    );
}

/// Split `area` into a 2x2 grid.
fn grid_2x2(area: Rect) -> [Rect; 4] {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);
    [top[0], top[1], bottom[0], bottom[1]]
}

fn draw_overview_page(frame: &mut Frame<'_>, area: Rect, stats: &StatsResult) {
    let [top_left, top_right, bottom_left, bottom_right] = grid_2x2(area);

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

    draw_bar_chart(frame, top_left, "By language", &by_language, MAX_BARS);
    draw_bar_chart(frame, top_right, "By owner", &by_owner, MAX_BARS);
    draw_bar_chart(frame, bottom_left, "Top tags", &top_tags, MAX_BARS);
    draw_bar_chart(
        frame,
        bottom_right,
        "Most functions",
        &most_functions,
        MAX_BARS,
    );
}

fn draw_checkouts_page(frame: &mut Frame<'_>, area: Rect, stats: &StatsResult) {
    let [top_left, top_right, bottom_left, bottom_right] = grid_2x2(area);

    let checkout_by_os: Vec<(String, i64)> = stats
        .checkout_by_os
        .iter()
        .map(|o| (o.os_flavor.clone(), o.count))
        .collect();
    let most_active_checkout_users: Vec<(String, i64)> = stats
        .most_active_checkout_users
        .iter()
        .map(|u| (u.user.clone(), u.count))
        .collect();
    // Histograms keep their fixed bucket order (ascending range, not by
    // count) — see `core::search::stats::bucket_counts` — so unlike the
    // ranking charts above, these are never sorted by `draw_bar_chart`'s
    // caller-owns-the-order contract; they already arrive in the right order.
    let size_histogram: Vec<(String, i64)> = stats
        .size_histogram
        .iter()
        .map(|b| (b.label.clone(), b.count))
        .collect();
    let checkout_staleness_histogram: Vec<(String, i64)> = stats
        .checkout_staleness_histogram
        .iter()
        .map(|b| (b.label.clone(), b.count))
        .collect();

    draw_bar_chart(
        frame,
        top_left,
        "Checkouts by OS",
        &checkout_by_os,
        MAX_BARS,
    );
    draw_bar_chart(
        frame,
        top_right,
        "Most active checkout users",
        &most_active_checkout_users,
        MAX_BARS,
    );
    // Histograms are never truncated, regardless of MAX_BARS: every bucket
    // is part of the shape, and SIZE_BUCKETS/STALENESS_BUCKETS_DAYS (see
    // core::search::stats) are already fixed-size and small.
    draw_bar_chart(
        frame,
        bottom_left,
        "Checkout staleness",
        &checkout_staleness_histogram,
        checkout_staleness_histogram.len(),
    );
    draw_bar_chart(
        frame,
        bottom_right,
        "Script size",
        &size_histogram,
        size_histogram.len(),
    );
}

fn draw_activity_page(frame: &mut Frame<'_>, area: Rect, stats: &StatsResult) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    draw_activity_calendar(frame, columns[0], &stats.checkout_activity_by_day);

    let findings_by_check: Vec<(String, i64)> = stats
        .findings_by_check
        .iter()
        .map(|c| (c.check.clone(), c.count))
        .collect();
    draw_bar_chart(
        frame,
        columns[1],
        "Audit findings by check",
        &findings_by_check,
        MAX_BARS,
    );
}

/// Draw one horizontal bar chart of `(label, count)` pairs, in the order the
/// caller supplies (rankings pass count-descending; histograms pass their
/// fixed ascending bucket order), truncated to `max_bars` with the title
/// noting how many were cut. Rankings pass [`MAX_BARS`]; histograms pass
/// their own full length so a fixed bucket is never silently dropped.
fn draw_bar_chart(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    data: &[(String, i64)],
    max_bars: usize,
) {
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
    let shown = data.len().min(max_bars);
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

/// Draw a GitHub-contributions-style heatmap: [`CALENDAR_MONTHS`] months of
/// active (DEVELOP) checkout activity, side by side, ending at the most
/// recent recorded date (not "today" — the data can be older than the
/// session, e.g. a catalog nobody has touched recently).
fn draw_activity_calendar(frame: &mut Frame<'_>, area: Rect, daily: &[DailyCount]) {
    let parsed: Vec<(time::Date, i64)> = daily
        .iter()
        .filter_map(|d| parse_yyyymmdd(&d.date).map(|date| (date, d.count)))
        .collect();
    let Some(latest) = parsed.iter().map(|(date, _)| *date).max() else {
        frame.render_widget(
            Paragraph::new("No checkout activity recorded.").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Checkout activity"),
            ),
            area,
        );
        return;
    };
    let max_count = parsed
        .iter()
        .map(|(_, count)| *count)
        .max()
        .unwrap_or(1)
        .max(1) as f64;

    let mut store = CalendarEventStore::default();
    for (date, count) in &parsed {
        let intensity = *count as f64 / max_count;
        let style = if intensity > 0.67 {
            Style::default().bg(Color::Rgb(0, 220, 0)).fg(Color::Black)
        } else if intensity > 0.34 {
            Style::default().bg(Color::Rgb(0, 150, 0)).fg(Color::Black)
        } else {
            Style::default().bg(Color::Rgb(0, 90, 0))
        };
        store.add(*date, style);
    }

    let months = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![
            Constraint::Ratio(1, CALENDAR_MONTHS as u32);
            CALENDAR_MONTHS as usize
        ])
        .split(area);

    for (i, area) in months.iter().enumerate() {
        let offset = CALENDAR_MONTHS - 1 - i as i32;
        let display_date = shift_months(latest, -offset);
        let calendar = Monthly::new(display_date, &store)
            .show_month_header(Style::default().add_modifier(Modifier::BOLD))
            .show_weekdays_header(Style::default().fg(Color::DarkGray))
            .default_style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(calendar, *area);
    }
}

/// Parse a `YYYYMMDD` date string (as stored by `StatsResult::checkout_activity_by_day`).
fn parse_yyyymmdd(s: &str) -> Option<time::Date> {
    if s.len() != 8 {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u8 = s.get(4..6)?.parse().ok()?;
    let day: u8 = s.get(6..8)?.parse().ok()?;
    let month = time::Month::try_from(month).ok()?;
    time::Date::from_calendar_date(year, month, day).ok()
}

/// Shift `date` by `delta` whole calendar months (may be negative), always
/// landing on the 1st — callers only need the target month, not a specific
/// day, which sidesteps day-of-month overflow (e.g. Jan 31 minus a month).
fn shift_months(date: time::Date, delta: i32) -> time::Date {
    let month_index = date.year() * 12 + i32::from(u8::from(date.month())) - 1 + delta;
    let year = month_index.div_euclid(12);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let month = time::Month::try_from((month_index.rem_euclid(12) + 1) as u8)
        .expect("rem_euclid(12) + 1 is always in 1..=12");
    time::Date::from_calendar_date(year, month, 1).expect("day 1 is always valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_yyyymmdd_accepts_valid_dates() {
        let date = parse_yyyymmdd("20260315").unwrap();
        assert_eq!(date.year(), 2026);
        assert_eq!(u8::from(date.month()), 3);
        assert_eq!(date.day(), 15);
    }

    #[test]
    fn parse_yyyymmdd_rejects_malformed_input() {
        assert!(parse_yyyymmdd("2026031").is_none());
        assert!(parse_yyyymmdd("2026-03-15").is_none());
        assert!(parse_yyyymmdd("20261301").is_none());
        assert!(parse_yyyymmdd("").is_none());
    }

    #[test]
    fn shift_months_stays_within_the_year() {
        let date = time::Date::from_calendar_date(2026, time::Month::August, 15).unwrap();
        let shifted = shift_months(date, -1);
        assert_eq!(shifted.year(), 2026);
        assert_eq!(u8::from(shifted.month()), 7);
        assert_eq!(shifted.day(), 1);
    }

    #[test]
    fn shift_months_crosses_a_year_boundary_backward() {
        let date = time::Date::from_calendar_date(2026, time::Month::January, 15).unwrap();
        let shifted = shift_months(date, -2);
        assert_eq!(shifted.year(), 2025);
        assert_eq!(u8::from(shifted.month()), 11);
    }

    #[test]
    fn shift_months_crosses_a_year_boundary_forward() {
        let date = time::Date::from_calendar_date(2025, time::Month::December, 15).unwrap();
        let shifted = shift_months(date, 2);
        assert_eq!(shifted.year(), 2026);
        assert_eq!(u8::from(shifted.month()), 2);
    }

    #[test]
    fn shift_months_zero_is_a_no_op_on_the_month() {
        let date = time::Date::from_calendar_date(2026, time::Month::June, 15).unwrap();
        let shifted = shift_months(date, 0);
        assert_eq!(shifted.year(), 2026);
        assert_eq!(u8::from(shifted.month()), 6);
        assert_eq!(shifted.day(), 1);
    }
}
