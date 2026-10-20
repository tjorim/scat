//! TUI frame rendering, split by pane: [`header`], [`search`], [`results`],
//! [`metadata`], [`preview`], [`deps`], [`functions`], [`revisions`],
//! [`footer`], plus the full-screen [`detail_view`]. [`common`] holds the
//! virtualized-list and text-truncation helpers panes share.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use super::{Focus, RegionKind, TuiApp, ViewMode};

mod common;
mod dep_graph;
mod deps;
mod detail_view;
mod footer;
mod functions;
mod header;
mod metadata;
mod popup;
mod preview;
mod results;
mod revisions;
mod search;
mod stats_view;

use dep_graph::draw_dep_graph_view;
use deps::draw_deps;
use detail_view::{draw_detail_diff_view, draw_detail_view};
use footer::draw_footer;
use functions::draw_functions;
use header::draw_header;
use metadata::draw_metadata;
use preview::draw_preview;
use results::draw_results;
use revisions::draw_revisions;
use search::draw_search;
use stats_view::draw_stats_view;

pub(super) fn draw(frame: &mut Frame<'_>, app: &mut TuiApp) {
    if let Some(dialog) = &app.workflow {
        app.click_regions.clear();
        app.pane_tabs.clear();
        super::workflows::draw(frame, dialog);
        return;
    }
    if let Some(page) = &app.inspection {
        app.click_regions.clear();
        app.pane_tabs.clear();
        super::inspection::draw(frame, page);
        popup::draw_popup(frame, app);
        return;
    }
    draw_inner(frame, app);
    if let Some(flash) = &app.flash {
        let area = frame.area();
        let footer =
            ratatui::layout::Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        let color = if flash.starts_with("Copy failed:") {
            ratatui::style::Color::Red
        } else {
            ratatui::style::Color::Green
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(flash.as_str())
                .style(ratatui::style::Style::default().fg(color)),
            footer,
        );
    }
    if app.native_selection {
        let area = frame.area();
        let footer =
            ratatui::layout::Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(
                "Selection mode: drag to select, use terminal Copy; F2/Esc resumes",
            )
            .style(
                ratatui::style::Style::default()
                    .fg(ratatui::style::Color::Black)
                    .bg(ratatui::style::Color::Cyan),
            ),
            footer,
        );
    }
    if !app.native_selection && app.flash.is_none() {
        let area = frame.area();
        let footer =
            ratatui::layout::Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        draw_footer(frame, app, footer);
    }
    popup::draw_popup(frame, app);
}

fn draw_inner(frame: &mut Frame<'_>, app: &mut TuiApp) {
    app.query_area = ratatui::layout::Rect::default();
    // Rebuilt every frame; the mouse handler hit-tests against the panes as
    // they were last drawn.
    app.click_regions.clear();
    app.pane_tabs.clear();
    match app.mode {
        ViewMode::Detail => {
            draw_detail_view(frame, app);
            return;
        }
        ViewMode::DetailDiff => {
            draw_detail_diff_view(frame, app);
            return;
        }
        ViewMode::Stats => {
            draw_stats_view(frame, app);
            return;
        }
        ViewMode::DepGraph => {
            draw_dep_graph_view(frame, app);
            return;
        }
        ViewMode::Browse => {}
    }

    if app.fullscreen {
        draw_browse_fullscreen(frame, app);
        return;
    }

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, app, root[0]);
    draw_search(frame, app, root[1]);
    draw_body(frame, app, root[2]);
    draw_footer(frame, app, root[3]);

    // Header (borderless) and search box are clickable: copy the path / focus
    // the search input respectively.
    app.record_click_area(root[0], RegionKind::Header, 0);
    app.record_click_area(root[1], RegionKind::Search, 0);
}

fn draw_browse_fullscreen(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, app, root[0]);
    app.record_click_area(root[0], RegionKind::Header, 0);
    let pane = root[1];
    draw_focused_pane(frame, app, pane, app.focus);
    draw_footer(frame, app, root[2]);
}

fn draw_body(frame: &mut Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    if area.width < 110 || area.height < 30 {
        draw_compact_body(frame, app, area);
        return;
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(area);
    draw_results(frame, app, columns[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),
            Constraint::Percentage(38),
            Constraint::Percentage(18),
            Constraint::Percentage(20),
            Constraint::Percentage(24),
        ])
        .split(columns[1]);
    draw_metadata(frame, app, right[0]);
    draw_preview(frame, app, right[1]);
    draw_deps(frame, app, right[2]);
    draw_functions(frame, app, right[3]);
    draw_revisions(frame, app, right[4]);

    // Record clickable regions (after drawing, so list scroll offsets are
    // current) for the mouse handler to hit-test against.
    app.record_region_with_row_index(
        columns[0],
        RegionKind::Results,
        app.results_row_index.clone(),
    );
    app.record_region(right[0], RegionKind::Metadata, 0);
    app.record_region(
        right[1],
        RegionKind::Preview,
        usize::from(app.preview_scroll),
    );
    app.record_region(right[2], RegionKind::Deps, app.deps_state.offset());
    app.record_region(
        right[3],
        RegionKind::Functions,
        app.functions_state.offset(),
    );
}

/// Small screens show the active pane; focus remains the source of truth
/// across resize and fullscreen transitions.
fn draw_compact_body(frame: &mut Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
    use ratatui::widgets::Tabs;
    use unicode_width::UnicodeWidthStr;
    const LABELS: [&str; 5] = [
        "Results",
        "Preview",
        "Dependencies",
        "Functions",
        "Revisions",
    ];
    const DIVIDER: &str = " | ";
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    let focuses = [
        Focus::Results,
        Focus::Preview,
        Focus::Deps,
        Focus::Functions,
        Focus::Revisions,
    ];
    let active = focuses.iter().position(|f| *f == app.focus).unwrap_or(0);
    frame.render_widget(
        Tabs::new(LABELS)
            .select(active)
            .padding("", "")
            .divider(DIVIDER),
        rows[0],
    );
    let divider_width = UnicodeWidthStr::width(DIVIDER) as u16;
    let mut x = rows[0].x;
    for (label, focus) in LABELS.into_iter().zip(focuses) {
        let label_width = UnicodeWidthStr::width(label) as u16;
        let width = label_width.min(rows[0].right().saturating_sub(x));
        app.pane_tabs.push((
            ratatui::layout::Rect::new(x, rows[0].y, width, rows[0].height),
            focus,
        ));
        x = x.saturating_add(label_width + divider_width);
    }
    // Reuse fullscreen pane rendering and its mouse regions without changing
    // either focus or fullscreen state.
    draw_focused_pane(frame, app, rows[1], focuses[active]);
}

fn draw_focused_pane(
    frame: &mut Frame<'_>,
    app: &mut TuiApp,
    pane: ratatui::layout::Rect,
    focus: Focus,
) {
    match focus {
        Focus::Search => {
            draw_search(frame, app, pane);
            app.record_click_area(pane, RegionKind::Search, 0);
        }
        Focus::Results => {
            draw_results(frame, app, pane);
            app.record_region_with_row_index(
                pane,
                RegionKind::Results,
                app.results_row_index.clone(),
            );
        }
        Focus::Preview => {
            draw_preview(frame, app, pane);
            app.record_region(pane, RegionKind::Preview, usize::from(app.preview_scroll));
        }
        Focus::Deps => {
            draw_deps(frame, app, pane);
            app.record_region(pane, RegionKind::Deps, app.deps_state.offset());
        }
        Focus::Functions => {
            draw_functions(frame, app, pane);
            app.record_region(pane, RegionKind::Functions, app.functions_state.offset());
        }
        Focus::Revisions => {
            draw_revisions(frame, app, pane);
        }
    }
}
