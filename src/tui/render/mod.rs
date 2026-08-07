//! TUI frame rendering, split by pane: [`header`], [`search`], [`results`],
//! [`metadata`], [`preview`], [`deps`], [`functions`], [`revisions`],
//! [`footer`], plus the full-screen [`detail_view`]. [`common`] holds the
//! virtualized-list and text-truncation helpers panes share.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use super::{Focus, RegionKind, TuiApp, ViewMode};

mod common;
mod deps;
mod detail_view;
mod footer;
mod functions;
mod header;
mod metadata;
mod preview;
mod results;
mod revisions;
mod search;
mod stats_view;

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
    // Rebuilt every frame; the mouse handler hit-tests against the panes as
    // they were last drawn.
    app.click_regions.clear();
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
            Constraint::Min(10),
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
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, app, root[0]);
    app.record_click_area(root[0], RegionKind::Header, 0);
    let pane = root[1];
    match app.focus {
        Focus::Search => {
            draw_search(frame, app, pane);
            app.record_click_area(pane, RegionKind::Search, 0);
        }
        Focus::Results => {
            draw_results(frame, app, pane);
            app.record_region(pane, RegionKind::Results, app.results_state.offset());
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
            app.record_region(
                pane,
                RegionKind::Revisions,
                usize::from(app.revisions_scroll),
            );
        }
    }
    draw_footer(frame, app, root[2]);
}

fn draw_body(frame: &mut Frame<'_>, app: &mut TuiApp, area: ratatui::layout::Rect) {
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
    app.record_region(columns[0], RegionKind::Results, app.results_state.offset());
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
    app.record_region(
        right[4],
        RegionKind::Revisions,
        usize::from(app.revisions_scroll),
    );
}
