//! Render-performance regression harness (#46, #139).
//!
//! Drives the real `render::draw` path against a ratatui `TestBackend` with
//! large synthetic state: 1k/10k results (mixed with two-row symlink
//! entries), a 20k-line wrapped preview in the pane, the detail view and the
//! diff view, a large Functions list, and a deep/wide dependency tree.
//!
//! Each scenario times a full draw, a scroll step and a resize. The budget
//! assertions are deliberately coarse (they must hold in an unoptimized
//! debug build on a loaded CI runner) and only catch "whole-content work per
//! frame" regressions, which cost orders of magnitude more. A width change
//! legitimately re-wraps long text once (see `wrapped_text`), so those
//! scenarios check resize against the looser `RELAYOUT_BUDGET`. For
//! indicative numbers, run the report in release mode:
//!
//! ```text
//! cargo test --release --bin scat render_perf -- --nocapture
//! ```

use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::text::Line;
use serde_json::{Map, Value};

use super::detail_worker::FunctionItem;
use super::render::draw;
use super::tests::{make_app, settle};
use super::{Focus, TuiApp, ViewMode, inspection};

/// Per-frame budget for the coarse CI check. A frame that has to walk or
/// clone whole-content state at these sizes costs well over this in a debug
/// build; one that only touches the visible window costs a small fraction.
const FRAME_BUDGET: Duration = Duration::from_millis(40);
/// Budget for a frame that follows a width change on a 20k-line text, which
/// re-wraps every line that doesn't fit the new width.
const RELAYOUT_BUDGET: Duration = Duration::from_secs(2);
const SAMPLES: usize = 15;
const WIDTH: u16 = 160;
const HEIGHT: u16 = 48;

struct Timings {
    draw: Duration,
    scroll: Duration,
    resize: Duration,
}

/// Median of `SAMPLES` runs of `step` followed by a draw.
fn median_frame(
    terminal: &mut Terminal<TestBackend>,
    app: &mut TuiApp,
    mut step: impl FnMut(&mut Terminal<TestBackend>, &mut TuiApp, usize),
) -> Duration {
    let mut samples: Vec<Duration> = (0..SAMPLES)
        .map(|i| {
            let start = Instant::now();
            step(terminal, app, i);
            terminal.draw(|frame| draw(frame, app)).unwrap();
            start.elapsed()
        })
        .collect();
    samples.sort();
    samples[SAMPLES / 2]
}

fn measure(app: &mut TuiApp, scroll: impl Fn(&mut TuiApp, usize)) -> Timings {
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    // Warm-up frame: first-draw caches (wrap layout etc.) are built here.
    terminal.draw(|frame| draw(frame, app)).unwrap();
    let draw = median_frame(&mut terminal, app, |_, _, _| {});
    let scroll = median_frame(&mut terminal, app, |_, app, i| scroll(app, i));
    let resize = median_frame(&mut terminal, app, |terminal, _, i| {
        let width = if i % 2 == 0 { WIDTH - 37 } else { WIDTH };
        // `draw` autoresizes to the backend, so resize the backend itself.
        terminal.backend_mut().resize(width, HEIGHT);
    });
    Timings {
        draw,
        scroll,
        resize,
    }
}

fn settled_app(db: &tempfile::NamedTempFile) -> TuiApp {
    let mut app = make_app(db.path());
    settle(&mut app);
    app
}

fn result_rows(count: usize) -> Vec<Map<String, Value>> {
    (0..count)
        .map(|i| {
            let mut row = Map::new();
            let path = format!("/catalog/scripts/team_{}/tool_{i}.py", i % 37);
            if i % 5 == 0 {
                row.insert(
                    "symlink_target".into(),
                    Value::String(format!("{path}_20260729_140513")),
                );
            }
            row.insert("logical_path".into(), Value::String(path));
            row.insert("language".into(), Value::String("python".into()));
            row
        })
        .collect()
}

/// `count` lines, every third one long enough to wrap in the preview pane.
fn long_lines(count: usize) -> Vec<Line<'static>> {
    (0..count)
        .map(|i| {
            if i % 3 == 0 {
                Line::from(format!("{i:>6} {}", "wrapped words ".repeat(20)))
            } else {
                Line::from(format!("{i:>6} print('line')"))
            }
        })
        .collect()
}

fn load_preview(app: &mut TuiApp, count: usize) {
    let lines = long_lines(count);
    app.set_preview_content(
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        lines,
        count,
    );
}

fn report(name: &str, t: &Timings) {
    eprintln!(
        "render_perf {name:<28} draw {:>9.3?}  scroll {:>9.3?}  resize {:>9.3?}",
        t.draw, t.scroll, t.resize
    );
}

fn assert_budget(name: &str, t: &Timings, resize_budget: Duration) {
    report(name, t);
    for (what, d, budget) in [
        ("draw", t.draw, FRAME_BUDGET),
        ("scroll", t.scroll, FRAME_BUDGET),
        ("resize", t.resize, resize_budget),
    ] {
        assert!(
            d < budget,
            "{name}: {what} frame took {d:?}, budget {budget:?}"
        );
    }
}

fn results_scenario(count: usize) -> Timings {
    let db = super::make_test_db();
    let mut app = settled_app(&db);
    app.results = result_rows(count);
    app.focus = Focus::Results;
    app.selected = count / 2;
    measure(&mut app, |app, i| {
        app.selected = if i % 2 == 0 {
            app.selected + 1
        } else {
            app.selected - 1
        };
    })
}

#[test]
fn render_perf_results_1k() {
    assert_budget("results 1k", &results_scenario(1_000), FRAME_BUDGET);
}

#[test]
fn render_perf_results_10k() {
    assert_budget("results 10k", &results_scenario(10_000), FRAME_BUDGET);
}

#[test]
fn render_perf_long_preview_pane() {
    let db = super::make_test_db();
    let mut app = settled_app(&db);
    load_preview(&mut app, 20_000);
    app.focus = Focus::Preview;
    app.preview_scroll = 15_000;
    let t = measure(&mut app, |app, i| {
        app.preview_scroll = 15_000 + u16::try_from(i).unwrap();
    });
    assert_budget("preview pane 20k lines", &t, RELAYOUT_BUDGET);
}

#[test]
fn render_perf_long_detail_view() {
    let db = super::make_test_db();
    let mut app = settled_app(&db);
    load_preview(&mut app, 20_000);
    app.mode = ViewMode::Detail;
    app.detail_scroll = 15_000;
    let t = measure(&mut app, |app, i| {
        app.detail_scroll = 15_000 + u16::try_from(i).unwrap();
    });
    assert_budget("detail view 20k lines", &t, RELAYOUT_BUDGET);
}

#[test]
fn render_perf_long_diff_view() {
    let db = super::make_test_db();
    let mut app = settled_app(&db);
    let diff: Vec<String> = long_lines(20_000)
        .iter()
        .map(|line| format!("+{line}"))
        .collect();
    app.set_diff_output(diff.join("\n"));
    app.mode = ViewMode::DetailDiff;
    app.detail_diff_scroll = 15_000;
    let t = measure(&mut app, |app, i| {
        app.detail_diff_scroll = 15_000 + u16::try_from(i).unwrap();
    });
    assert_budget("diff view 20k lines", &t, RELAYOUT_BUDGET);
}

#[test]
fn render_perf_large_functions_list() {
    let db = super::make_test_db();
    let mut app = settled_app(&db);
    app.functions = (0..10_000)
        .map(|i| FunctionItem {
            name: format!("function_{i}"),
            kind: "function".into(),
            line: u16::try_from(i % 60_000).unwrap(),
            docstring: "Does a thing with some words in the docstring.".into(),
        })
        .collect();
    app.focus = Focus::Functions;
    app.functions_selected = 5_000;
    let t = measure(&mut app, |app, i| {
        app.functions_selected = 5_000 + i;
    });
    assert_budget("functions 10k", &t, FRAME_BUDGET);
}

#[test]
fn render_perf_deep_and_wide_dependency_tree() {
    let db = super::make_test_db();
    let mut app = settled_app(&db);
    // 20 levels deep x 500 wide: what a large `deps --tree` page flattens to.
    let rows = (0..10_000)
        .map(|i| {
            let level = i % 20;
            (
                format!(
                    "{}import /catalog/lib/module_{i}.py [repeated]",
                    "  ".repeat(level)
                ),
                Some(format!("/catalog/lib/module_{i}.py")),
            )
        })
        .collect();
    app.inspection = Some(inspection::Page::for_test(rows, 5_000));
    let t = measure(&mut app, |app, i| {
        app.inspection.as_mut().unwrap().selected = 5_000 + i;
    });
    assert_budget("deps tree 10k rows", &t, FRAME_BUDGET);
}
