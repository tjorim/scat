use std::borrow::Cow;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use scat_core::core::script_view::{ScriptView, symlink_target_display};
use scat_core::core::vc::relative_age;

use super::{Focus, RegionKind, TuiApp, ViewMode, detail};

const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn spinner_char(tick: u64) -> char {
    SPINNER_FRAMES[(tick as usize) % SPINNER_FRAMES.len()]
}

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
            app.record_region(pane, RegionKind::Deps, pane_scroll(app.deps_selected, pane));
        }
        Focus::Functions => {
            draw_functions(frame, app, pane);
            app.record_region(
                pane,
                RegionKind::Functions,
                pane_scroll(app.functions_selected, pane),
            );
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

fn draw_header(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let brand = Span::styled(
        " scat ",
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let path_text = app
        .detail
        .as_ref()
        .map(ScriptView::new)
        .and_then(|view| view.logical_path_value())
        .and_then(serde_json::Value::as_str)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let avail = (area.width as usize).saturating_sub(9);
            left_truncate_path(p, avail)
        });

    let mut spans = vec![brand, Span::raw("  ")];
    if let Some(path) = path_text {
        spans.push(Span::styled(path, Style::default().fg(Color::White)));
    }
    if app.fullscreen {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "[FULLSCREEN]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_detail_view(frame: &mut Frame<'_>, app: &mut TuiApp) {
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

fn draw_detail_diff_view(frame: &mut Frame<'_>, app: &mut TuiApp) {
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

fn draw_search(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let mut title = super::search_title(app.error.is_some(), app.search_in_flight).to_string();
    if !app.filter_labels.is_empty() {
        title = format!("{title} [{}]", app.filter_labels.join(" "));
    }
    let text = if app.query.is_empty() {
        Text::from(Line::from(Span::styled(
            "type to search — lang:/owner:/tag: to filter, Enter for results",
            Style::default().fg(Color::DarkGray),
        )))
    } else {
        Text::from(app.query.as_str())
    };
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(focus_border(app.focus, Focus::Search)),
        ),
        area,
    );
}

fn draw_body(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
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
    app.record_region(
        right[2],
        RegionKind::Deps,
        pane_scroll(app.deps_selected, right[2]),
    );
    app.record_region(
        right[3],
        RegionKind::Functions,
        pane_scroll(app.functions_selected, right[3]),
    );
    app.record_region(
        right[4],
        RegionKind::Revisions,
        usize::from(app.revisions_scroll),
    );
}

/// First visible row index for a selection-scrolled pane, matching the
/// `scroll_y` computed in [`draw_deps`]/[`draw_functions`]: keep the selected
/// entry within the pane's inner (bordered) height.
fn pane_scroll(selected: usize, outer: Rect) -> usize {
    let visible_rows = usize::from(outer.height.saturating_sub(2));
    selected.saturating_sub(visible_rows.saturating_sub(1))
}

/// One line of the results pane: the script's path, the target it symlinks to
/// when it is one, then language and checkout markers.
fn result_line(row: &scat_core::core::db::JsonRow, area: Rect) -> String {
    let view = ScriptView::new(row);
    let path = view.logical_path();
    let lang = view.language();
    let checkout = if view.checkout_user().is_empty() {
        ""
    } else {
        " CO"
    };
    // A symlink's own row carries the arrow, the same relationship the CLI
    // table shows as a `↳ <target>` sub-row. Rows here are selectable entries
    // backed by a script, so the target is annotated in place rather than
    // added as a row of its own.
    let arrow = symlink_arrow(path, view.symlink_target());
    // Reserve: 2 (highlight) + 2 (separator) + lang + checkout + arrow
    let max_name = (area.width as usize)
        .saturating_sub(2)
        .saturating_sub(2)
        .saturating_sub(lang.len())
        .saturating_sub(checkout.len())
        .saturating_sub(arrow.chars().count());
    let display = left_truncate_path(path, max_name);
    format!("{display}{arrow}  {lang}{checkout}")
}

fn draw_results(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    let items: Vec<ListItem> = if app.search_in_flight && app.results.is_empty() {
        vec![ListItem::new(format!("{spinner} Searching…"))]
    } else if app.results.is_empty() {
        vec![ListItem::new(Span::styled(
            "No results.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.results
            .iter()
            .map(|row| ListItem::new(result_line(row, area)))
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Results ({})", app.results.len()))
                .border_style(focus_border(app.focus, Focus::Results)),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut app.results_state);
}

fn draw_metadata(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let inactive = Style::default().fg(Color::DarkGray);
    if app.detail_loading {
        let spinner = spinner_char(app.tick);
        frame.render_widget(
            Paragraph::new(format!("{spinner} Loading…")).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Metadata")
                    .border_style(inactive),
            ),
            area,
        );
        return;
    }
    let Some(row) = app.detail.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No script selected.",
                Style::default().fg(Color::DarkGray),
            ))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Metadata")
                    .border_style(inactive),
            ),
            area,
        );
        return;
    };
    let view = ScriptView::new(row);
    let warnings = detail::warning_summary(row);
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Path      ", detail::label_style()),
            Span::raw(view.logical_path().to_string()),
        ]),
        Line::from(vec![
            Span::styled("Language  ", detail::label_style()),
            Span::raw(view.language().to_string()),
        ]),
        Line::from(vec![
            Span::styled("Owner     ", detail::label_style()),
            Span::raw(detail::display_text(view.owner())),
        ]),
        Line::from(vec![
            Span::styled("Contribs  ", detail::label_style()),
            Span::raw(if app.contributors.is_empty() {
                "—".to_string()
            } else {
                app.contributors.join(", ")
            }),
        ]),
        Line::from(vec![
            Span::styled("Purpose   ", detail::label_style()),
            Span::raw(detail::display_text(view.purpose())),
        ]),
        Line::from(vec![
            Span::styled("Checkout  ", detail::label_style()),
            Span::raw(view.checkout_label()),
        ]),
    ];
    if !view.symlink_target().is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Symlink   ", detail::label_style()),
            Span::raw(format!("→ {}", view.symlink_target())),
        ]));
    }
    if !warnings.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Warnings  ", detail::label_style()),
            Span::raw(warnings),
        ]));
    }
    if let Some(native) = detail::native_path_for_row(row, &app.resolver) {
        lines.push(Line::from(vec![
            Span::styled("OS path   ", detail::label_style()),
            Span::raw(native),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Metadata")
                .border_style(inactive),
        ),
        area,
    );
}

fn draw_preview(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
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
        Text::from(Span::styled(
            "(empty)",
            Style::default().fg(Color::DarkGray),
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

fn draw_deps(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    let (text, title) = if app.detail_loading {
        (format!("{spinner} Loading…"), "Deps (loading…)".to_string())
    } else if let Some(function_name) = &app.function_xref {
        if let Some(call_sites) = app.xref_call_sites() {
            let lines = call_sites
                .iter()
                .enumerate()
                .map(|(idx, site)| {
                    let marker = if idx == app.deps_selected { "> " } else { "  " };
                    format!(
                        "{marker}{:<7} {}:{}  {} -> {}",
                        "calls", site.caller_path, site.line, site.caller, site.callee
                    )
                })
                .collect::<Vec<_>>();
            (
                if lines.is_empty() {
                    "No call sites.".to_string()
                } else {
                    lines.join("\n")
                },
                format!("Deps (call sites for {function_name})"),
            )
        } else {
            (
                "No call sites.".to_string(),
                format!("Deps (call sites for {function_name})"),
            )
        }
    } else {
        let lines = app
            .deps
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let marker = if idx == app.deps_selected { "> " } else { "  " };
                format!("{marker}{:<7} {}", item.kind, item.logical_path)
            })
            .collect::<Vec<_>>();
        (
            if lines.is_empty() {
                "No dependencies.".to_string()
            } else {
                lines.join("\n")
            },
            "Deps".to_string(),
        )
    };
    let text_widget: Text =
        if !app.detail_loading && (text == "No dependencies." || text == "No call sites.") {
            Text::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
        } else {
            Text::from(text.as_str())
        };
    // Scroll the pane so the selected item stays within the visible window.
    let visible_rows = area.height.saturating_sub(2) as usize;
    let scroll_y = app
        .deps_selected
        .saturating_sub(visible_rows.saturating_sub(1)) as u16;
    frame.render_widget(
        Paragraph::new(text_widget)
            .wrap(Wrap { trim: true })
            .scroll((scroll_y, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(focus_border(app.focus, Focus::Deps)),
            ),
        area,
    );
}

fn draw_functions(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    let (text, title) = if app.detail_loading {
        (
            format!("{spinner} Loading…"),
            "Functions (loading…)".to_string(),
        )
    } else {
        let lines = app
            .functions
            .iter()
            .enumerate()
            .map(|(idx, function)| {
                let marker = if idx == app.functions_selected {
                    "> "
                } else {
                    "  "
                };
                let doc = if function.docstring.is_empty() {
                    "—".to_string()
                } else {
                    function.docstring.clone()
                };
                format!(
                    "{marker}{:<20} {:<8} {:>4}  {}",
                    function.name, function.kind, function.line, doc
                )
            })
            .collect::<Vec<_>>();
        (
            if lines.is_empty() {
                "No functions indexed.".to_string()
            } else {
                lines.join("\n")
            },
            "Functions".to_string(),
        )
    };
    let text_widget: Text = if !app.detail_loading && text == "No functions indexed." {
        Text::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
    } else {
        Text::from(text.as_str())
    };
    // Scroll the pane so the selected item stays within the visible window.
    let visible_rows = area.height.saturating_sub(2) as usize;
    let scroll_y = app
        .functions_selected
        .saturating_sub(visible_rows.saturating_sub(1)) as u16;
    frame.render_widget(
        Paragraph::new(text_widget)
            .wrap(Wrap { trim: true })
            .scroll((scroll_y, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(focus_border(app.focus, Focus::Functions)),
            ),
        area,
    );
}

fn draw_revisions(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    if app.detail_loading {
        app.revisions_scroll = 0;
        frame.render_widget(
            Paragraph::new(format!("{spinner} Loading…")).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Revisions (loading…)")
                    .border_style(focus_border(app.focus, Focus::Revisions)),
            ),
            area,
        );
        return;
    }

    let lines = if app.checkouts.is_empty() {
        vec![Line::from(Span::styled(
            "No revision data.",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        let active = app
            .detail
            .as_ref()
            .map(ScriptView::new)
            .map(|view| view.symlink_target().to_string())
            .unwrap_or_default();
        revision_lines(&app.checkouts, &active)
    };
    clamp_scroll_offset(&mut app.revisions_scroll, lines.len(), area);
    let title = format!(
        "Revisions (line {})",
        app.revisions_scroll.saturating_add(1)
    );
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: true })
            .scroll((app.revisions_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(focus_border(app.focus, Focus::Revisions)),
            ),
        area,
    );
}

/// Render the revisions pane, grouped by revision type.
///
/// `active_target` is the script's `symlink_target`; the WORKING revision it
/// resolves to is marked as the live one. Which of the retained versions is
/// actually active is not implied by their order — a rollback re-points the
/// symlink at an older version and leaves the newer ones in place, so the
/// group can hold versions both older and newer than the live one.
fn revision_lines(revisions: &[super::JsonRow], active_target: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    append_revision_group(&mut lines, "DEVELOP", revisions, active_target);
    lines.push(Line::raw(""));
    // Between DEVELOP and ARCHIVE: newer than anything archived, not a
    // checkout. Without a group of its own this lands under "OTHER", which is
    // where every working-directory version copy used to be filed.
    append_revision_group(&mut lines, "WORKING", revisions, active_target);
    lines.push(Line::raw(""));
    append_revision_group(&mut lines, "ARCHIVE", revisions, active_target);
    let other_rows = revisions
        .iter()
        .filter(|row| {
            let revision_type = super::str_field(row, "revision_type");
            !matches!(
                revision_type.as_str(),
                "" | "DEVELOP" | "WORKING" | "ARCHIVE"
            )
        })
        .collect::<Vec<_>>();
    if !other_rows.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "OTHER",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        )));
        for row in other_rows {
            lines.push(Line::raw(format_revision_row(row, "")));
        }
    }
    lines
}

fn append_revision_group(
    lines: &mut Vec<Line<'static>>,
    revision_type: &str,
    revisions: &[super::JsonRow],
    active_target: &str,
) {
    let badge_style = match revision_type {
        "DEVELOP" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "WORKING" => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        "ARCHIVE" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    };
    lines.push(Line::from(Span::styled(
        revision_type.to_string(),
        badge_style,
    )));
    let mut found = false;
    for row in revisions {
        let row_revision_type = super::str_field(row, "revision_type");
        if row_revision_type == revision_type
            || (revision_type == "DEVELOP" && row_revision_type.is_empty())
        {
            lines.push(Line::raw(format_revision_row(row, active_target)));
            found = true;
        }
    }
    if !found {
        let label = revision_type.to_ascii_lowercase();
        lines.push(Line::from(Span::styled(
            format!("  (no {label} entries.)"),
            Style::default().fg(Color::DarkGray),
        )));
    }
}

fn format_revision_row(row: &super::JsonRow, active_target: &str) -> String {
    let os = super::str_field(row, "os_flavor");
    let user = super::str_field(row, "user");
    let timestamp = super::str_field(row, "timestamp");
    let age = row
        .get("age_seconds")
        .and_then(serde_json::Value::as_f64)
        .map(relative_age);
    let age_suffix = age.map(|v| format!("   ({v})")).unwrap_or_default();
    let active = if is_active_revision(row, active_target) {
        "  ← active"
    } else {
        ""
    };
    format!("  {os:<7} {user:<12} {timestamp}{age_suffix}{active}")
}

/// Whether this revision is the version the script's symlink resolves to.
///
/// The symlink target is a logical path and a revision carries the on-disk
/// path it was found at, so the two are compared by filename. That is exact
/// for the case it is meant to catch: vc's active-version symlinks point at a
/// sibling in the same working directory, so a matching filename there is the
/// same file. An empty target (the script is not a symlink) matches nothing.
fn is_active_revision(row: &super::JsonRow, active_target: &str) -> bool {
    if active_target.is_empty() {
        return false;
    }
    let file_name = |p: &str| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string();
    let physical = super::str_field(row, "physical_path");
    !physical.is_empty() && file_name(&physical) == file_name(active_target)
}

fn draw_footer(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    // A transient status (e.g. "Copied …") takes over the footer until the
    // next input event.
    if let Some(flash) = &app.flash {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                flash.clone(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ))),
            area,
        );
        return;
    }
    let sep = Span::raw("  ");
    let mut spans = vec![
        hint_key("/"),
        Span::raw(" search"),
        sep.clone(),
        hint_key("Enter"),
        Span::raw(" open/jump"),
        sep.clone(),
        hint_key("Tab"),
        Span::raw(" panes"),
        sep.clone(),
        hint_key("j/k"),
        Span::raw(" move"),
        sep.clone(),
        hint_key("Backspace/["),
        Span::raw(" dep-back"),
        sep.clone(),
        hint_key("g/G"),
        Span::raw(" top/bottom"),
        sep.clone(),
        hint_key("Ctrl+u/d"),
        Span::raw(" scroll"),
        sep.clone(),
        hint_key("v"),
        Span::raw(" view catalog"),
        sep.clone(),
        hint_key("V"),
        Span::raw(" view source"),
        sep.clone(),
    ];
    if app.fullscreen {
        spans.push(hint_key("f/Esc"));
        spans.push(Span::raw(" exit fullscreen"));
    } else {
        spans.push(hint_key("f"));
        spans.push(Span::raw(" fullscreen"));
    }
    spans.push(sep.clone());
    spans.push(hint_key("q/Esc"));
    spans.push(Span::raw(" quit"));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn line_count(text: &str) -> usize {
    text.lines().count().max(1)
}

/// Title for the catalog preview pane. When the indexed script is longer than
/// `PREVIEW_LINES`, the preview is capped, so flag that and point at the
/// full-script viewer keys.
fn preview_title(scroll: u16, total_lines: usize) -> String {
    let line = scroll.saturating_add(1);
    if total_lines > super::PREVIEW_LINES {
        format!(
            "Catalog preview (line {line} — first {} of {total_lines} lines, v/V for full)",
            super::PREVIEW_LINES
        )
    } else {
        format!("Catalog preview (line {line})")
    }
}

fn clamp_scroll_offset(scroll: &mut u16, line_count: usize, area: Rect) {
    let viewport_lines = usize::from(area.height.saturating_sub(2)).max(1);
    let max_scroll = line_count.saturating_sub(viewport_lines);
    *scroll = (*scroll).min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
}

/// Returns a styled span for a key hint label.
fn hint_key(key: &'static str) -> Span<'static> {
    Span::styled(
        key,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

/// Border style for a pane: bold yellow when active, dark gray when inactive.
fn focus_border(current: Focus, target: Focus) -> Style {
    if current == target {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Left-truncate a path to `max_chars`, preferring a `…/parent/file` form.
/// Render a symlink's target as a ` → target` suffix, or an empty string when
/// the script is not a symlink. Counterpart to the CLI table's `↳` sub-row.
fn symlink_arrow(path: &str, target: &str) -> String {
    if target.is_empty() {
        return String::new();
    }
    format!(" → {}", symlink_target_display(path, target))
}

fn left_truncate_path(path: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    if path.chars().count() <= max_chars {
        return path.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }

    // Find slash byte offsets via char_indices so every slice starts on a UTF-8
    // boundary even when directory or file names contain multi-byte characters.
    if let Some(last_slash) = path
        .char_indices()
        .rfind(|&(_, c)| c == '/')
        .map(|(i, _)| i)
    {
        let before_last = &path[..last_slash];
        // Try "…/parent/file".
        if let Some(second_slash) = before_last
            .char_indices()
            .rfind(|&(_, c)| c == '/')
            .map(|(i, _)| i)
        {
            let suffix = &path[second_slash..];
            if suffix.chars().count() < max_chars {
                return format!("…{suffix}");
            }
        }
        // Try "…/file".
        let suffix = &path[last_slash..];
        if suffix.chars().count() < max_chars {
            return format!("…{suffix}");
        }
    }

    // Last resort: character-based truncation from the left.
    let available = max_chars - 1;
    let start_idx = path
        .char_indices()
        .nth_back(available.saturating_sub(1))
        .map(|(idx, _)| idx)
        .unwrap_or(path.len());
    format!("…{}", &path[start_idx..])
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use super::{
        clamp_scroll_offset, left_truncate_path, line_count, preview_title, revision_lines,
        symlink_arrow,
    };
    use ratatui::layout::Rect;

    fn line_text(line: &ratatui::text::Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn preview_title_plain_when_not_truncated() {
        // Total lines within the cap: no truncation hint.
        assert_eq!(
            preview_title(0, super::super::PREVIEW_LINES),
            "Catalog preview (line 1)"
        );
        assert_eq!(preview_title(11, 0), "Catalog preview (line 12)");
    }

    #[test]
    fn preview_title_flags_truncation_and_points_at_viewer() {
        let total = super::super::PREVIEW_LINES + 1;
        let title = preview_title(4, total);
        assert!(title.contains("line 5"), "title: {title}");
        assert!(
            title.contains(&format!(
                "first {} of {total} lines",
                super::super::PREVIEW_LINES
            )),
            "title: {title}"
        );
        assert!(title.contains("v/V for full"), "title: {title}");
    }

    fn revision_row(
        revision_type: &str,
        os: &str,
        user: &str,
        timestamp: &str,
    ) -> Map<String, Value> {
        let mut row = Map::new();
        row.insert(
            "revision_type".to_string(),
            Value::String(revision_type.to_string()),
        );
        row.insert("os_flavor".to_string(), Value::String(os.to_string()));
        row.insert("user".to_string(), Value::String(user.to_string()));
        row.insert(
            "timestamp".to_string(),
            Value::String(timestamp.to_string()),
        );
        row.insert(
            "physical_path".to_string(),
            Value::String(format!("/srv/scripts/tool_{timestamp}")),
        );
        row
    }

    #[test]
    fn revision_lines_group_develop_and_archive_rows() {
        let lines = revision_lines(
            &[
                revision_row("DEVELOP", "LINUX", "alice", "20240102_1200"),
                revision_row("ARCHIVE", "ZOS", "bob", "20231231_0900"),
            ],
            "",
        );

        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t == "DEVELOP"));
        assert!(
            texts
                .iter()
                .any(|t| t == "  LINUX   alice        20240102_1200")
        );
        assert!(texts.iter().any(|t| t == "ARCHIVE"));
        assert!(
            texts
                .iter()
                .any(|t| t == "  ZOS     bob          20231231_0900")
        );
        assert!(!texts.iter().any(|t| t == "  (no archive entries.)"));
    }

    #[test]
    fn revision_lines_give_working_versions_their_own_group() {
        // Working-directory version copies used to land under "OTHER"; they
        // are the common case for a vc-managed script, not an oddity.
        let lines = revision_lines(
            &[revision_row("WORKING", "LINUX", "", "20260701_105550")],
            "",
        );

        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t == "WORKING"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t == "OTHER"),
            "a WORKING row must not fall through to OTHER: {texts:?}"
        );
        let working_at = texts.iter().position(|t| t == "WORKING").unwrap();
        let develop_at = texts.iter().position(|t| t == "DEVELOP").unwrap();
        let archive_at = texts.iter().position(|t| t == "ARCHIVE").unwrap();
        assert!(
            develop_at < working_at && working_at < archive_at,
            "WORKING belongs between DEVELOP and ARCHIVE: {texts:?}"
        );
    }

    #[test]
    fn revision_lines_mark_the_version_the_symlink_points_at() {
        // Order does not imply which version is live: a rollback re-points the
        // symlink at an older version and leaves the newer one in place, so
        // here the *older* of the two is the active one.
        let lines = revision_lines(
            &[
                revision_row("WORKING", "LINUX", "", "20260729_140513"),
                revision_row("WORKING", "LINUX", "", "20260701_105550"),
            ],
            "/catalog/scripts/tool_20260701_105550",
        );

        let texts: Vec<String> = lines.iter().map(line_text).collect();
        let active: Vec<&String> = texts.iter().filter(|t| t.contains("← active")).collect();
        assert_eq!(active.len(), 1, "exactly one row is active: {texts:?}");
        assert!(active[0].contains("20260701_105550"), "{active:?}");
    }

    #[test]
    fn revision_lines_mark_nothing_when_the_script_is_not_a_symlink() {
        let lines = revision_lines(
            &[revision_row("WORKING", "LINUX", "", "20260701_105550")],
            "",
        );
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(!texts.iter().any(|t| t.contains("← active")), "{texts:?}");
    }

    #[test]
    fn results_pane_renders_the_symlink_arrow_on_screen() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut row = Map::new();
        row.insert(
            "logical_path".into(),
            Value::String("/shared/tools/scripts/source/prepare_release".into()),
        );
        row.insert("language".into(), Value::String("shell".into()));
        row.insert(
            "symlink_target".into(),
            Value::String("/shared/tools/scripts/source/prepare_release_20260729_140513".into()),
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let items = vec![ratatui::widgets::ListItem::new(super::result_line(
                    &row, area,
                ))];
                frame.render_widget(ratatui::widgets::List::new(items), area);
            })
            .unwrap();

        let rendered: String =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut acc, cell| {
                    acc.push_str(cell.symbol());
                    acc
                });
        assert!(
            rendered.contains("→ prepare_release_20260729_140513"),
            "results pane must show the symlink target: {rendered:?}"
        );
    }

    #[test]
    fn symlink_arrow_is_empty_for_a_plain_script() {
        assert_eq!(symlink_arrow("/catalog/scripts/tool.py", ""), "");
    }

    #[test]
    fn symlink_arrow_shows_bare_name_for_a_sibling_target() {
        assert_eq!(
            symlink_arrow(
                "/catalog/scripts/prepare_release",
                "/catalog/scripts/prepare_release_20260729_140513"
            ),
            " → prepare_release_20260729_140513"
        );
    }

    #[test]
    fn symlink_arrow_keeps_full_path_for_a_target_elsewhere() {
        assert_eq!(
            symlink_arrow("/catalog/scripts/tool.py", "/catalog/shared/tool_v2.py"),
            " → /catalog/shared/tool_v2.py"
        );
    }

    #[test]
    fn line_count_treats_empty_text_as_one_rendered_line() {
        assert_eq!(line_count(""), 1);
        assert_eq!(line_count("a\nb\nc"), 3);
    }

    #[test]
    fn clamp_scroll_offset_uses_inner_height() {
        let mut scroll = 99;
        clamp_scroll_offset(&mut scroll, 10, Rect::new(0, 0, 20, 5));
        assert_eq!(scroll, 7);

        let mut scroll = 99;
        clamp_scroll_offset(&mut scroll, 2, Rect::new(0, 0, 20, 5));
        assert_eq!(scroll, 0);
    }

    #[test]
    fn left_truncate_path_short_path_unchanged() {
        assert_eq!(left_truncate_path("/a/b.py", 20), "/a/b.py");
    }

    #[test]
    fn left_truncate_path_shows_parent_and_file() {
        let path = "/very/long/catalog/scripts/tools/myscript.py";
        let result = left_truncate_path(path, 25);
        assert!(
            result.starts_with('…'),
            "should start with ellipsis: {result}"
        );
        assert!(
            result.contains("myscript.py"),
            "should contain filename: {result}"
        );
        // use chars().count() since '…' is 1 display column but 3 bytes
        assert!(
            result.chars().count() <= 25,
            "should fit in max_chars: {result}"
        );
    }

    #[test]
    fn left_truncate_path_falls_back_to_filename() {
        // parent/file together too long, but file alone fits
        let path = "/a/very_long_parent_dir/short.py";
        let result = left_truncate_path(path, 12);
        assert!(
            result.starts_with('…'),
            "should start with ellipsis: {result}"
        );
        assert!(
            result.chars().count() <= 12,
            "should fit in max_chars: {result}"
        );
    }

    #[test]
    fn left_truncate_path_no_slash_falls_back_gracefully() {
        // Path with no slashes: should still return a truncated string
        let path = "noslashpath.py";
        let result = left_truncate_path(path, 8);
        assert!(
            result.chars().count() <= 8,
            "should fit in max_chars: {result}"
        );
    }

    #[test]
    fn left_truncate_path_handles_multibyte_characters() {
        let path = "/catalog/scripts/工具/分析🚀.py";
        let result = left_truncate_path(path, 8);
        assert!(
            result.starts_with('…'),
            "should start with ellipsis: {result}"
        );
        assert!(
            result.chars().count() <= 8,
            "should fit in max_chars: {result}"
        );
    }

    #[test]
    fn left_truncate_path_zero_width_returns_empty() {
        assert_eq!(left_truncate_path("/catalog/scripts/分析🚀.py", 0), "");
    }
}
