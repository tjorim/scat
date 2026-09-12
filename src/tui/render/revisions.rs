//! The Revisions pane: DEVELOP/WORKING/ARCHIVE checkout history, grouped and
//! marked with the version the script's symlink currently resolves to.
//!
//! Unlike the other list panes (Deps, Functions), this one is rendered as a
//! wrapped `Paragraph` rather than a `List` — headers and blank-line group
//! separators are baked into the same text block as the entries. Selection
//! (`app.revisions_selected`, an index into `app.checkouts`) is layered on
//! top: [`revision_lines`] reports which rendered line the selected entry
//! landed on, and `draw_revisions` scrolls that line into view.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use scat_core::core::db::{JsonRow, row_string as str_field};
use scat_core::core::script_view::ScriptView;
use scat_core::core::vc::relative_age;
use unicode_width::UnicodeWidthStr;

use super::super::{Focus, TuiApp};
use super::common::{clamp_scroll_offset, focus_border, spinner_char};

pub(super) fn draw_revisions(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
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

    let (lines, selected_line) = if app.checkouts.is_empty() {
        (
            vec![Line::from(Span::styled(
                "No revision data.",
                Style::default().fg(Color::DarkGray),
            ))],
            None,
        )
    } else {
        app.revisions_selected = app.revisions_selected.min(app.checkouts.len() - 1);
        let active = app
            .detail
            .as_ref()
            .map(ScriptView::new)
            .map(|view| view.symlink_target().to_string())
            .unwrap_or_default();
        let selected_physical_path =
            str_field(&app.checkouts[app.revisions_selected], "physical_path");
        revision_lines(&app.checkouts, &active, &selected_physical_path)
    };
    if let Some(selected_line) = selected_line {
        let inner_width = usize::from(area.width.saturating_sub(2)).max(1);
        let selected_row = wrapped_row_offset(&lines, selected_line, inner_width);
        ensure_line_visible(&mut app.revisions_scroll, selected_row, area);
    }
    let inner_width = usize::from(area.width.saturating_sub(2)).max(1);
    let rendered_rows = wrapped_row_offset(&lines, lines.len(), inner_width);
    clamp_scroll_offset(&mut app.revisions_scroll, rendered_rows, area);
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

/// Count terminal rows occupied by preceding logical lines under the same
/// word wrapping and leading-whitespace trimming used by `Paragraph`.
fn wrapped_row_offset(lines: &[Line<'_>], before_line: usize, width: usize) -> usize {
    lines
        .iter()
        .take(before_line)
        .map(|line| wrapped_line_rows(&line.to_string(), width))
        .sum()
}

/// Count the rows a single line occupies with `Wrap { trim: true }`.
/// Revision rows contain ordinary whitespace-separated fields, so this keeps
/// the calculation in sync with ratatui's word wrapping without changing the
/// rendered text itself.
fn wrapped_line_rows(line: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut rows = 1;
    let mut used = 0;

    let mut pending_whitespace = 0;
    let mut word_start = None;
    for (index, ch) in line.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = word_start.take() {
                let word_width = UnicodeWidthStr::width(&line[start..index]);
                (rows, used) =
                    place_wrapped_word(rows, used, pending_whitespace, word_width, width);
                pending_whitespace = 0;
            }
            pending_whitespace += UnicodeWidthStr::width(&line[index..index + ch.len_utf8()]);
        } else if word_start.is_none() {
            word_start = Some(index);
        }
    }
    if let Some(start) = word_start {
        let word_width = UnicodeWidthStr::width(&line[start..]);
        (rows, _) = place_wrapped_word(rows, used, pending_whitespace, word_width, width);
    }
    rows
}

/// Add one non-whitespace run after the whitespace that preceded it. With
/// `trim: true`, whitespace is discarded when it would begin a wrapped row.
fn place_wrapped_word(
    mut rows: usize,
    mut used: usize,
    whitespace_width: usize,
    word_width: usize,
    width: usize,
) -> (usize, usize) {
    if used != 0 && used + whitespace_width + word_width > width {
        rows += 1;
        used = 0;
    }
    if used != 0 {
        used += whitespace_width;
    }

    if word_width > width {
        let chunks = word_width.div_ceil(width);
        rows += chunks - 1;
        used = word_width % width;
        if used == 0 {
            used = width;
        }
    } else {
        used += word_width;
    }
    (rows, used)
}

/// Scroll the minimal amount to bring rendered line `line_index` into the
/// pane's visible window — same "keep visible, don't otherwise move" rule
/// `scroll_window` applies to the item-based panes, adapted to raw line
/// scrolling since this pane wraps text rather than listing discrete items.
fn ensure_line_visible(scroll: &mut u16, line_index: usize, area: Rect) {
    let inner_height = usize::from(area.height.saturating_sub(2)).max(1);
    let current = usize::from(*scroll);
    if line_index < current {
        *scroll = u16::try_from(line_index).unwrap_or(u16::MAX);
    } else if line_index >= current + inner_height {
        let target = line_index + 1 - inner_height;
        *scroll = u16::try_from(target).unwrap_or(u16::MAX);
    }
}

/// Render the revisions pane, grouped by revision type.
///
/// `active_target` is the script's `symlink_target`; the WORKING revision it
/// resolves to is marked as the live one. Which of the retained versions is
/// actually active is not implied by their order — a rollback re-points the
/// symlink at an older version and leaves the newer ones in place, so the
/// group can hold versions both older and newer than the live one.
///
/// Returns the lines to render, plus the index of the line matching
/// `selected_physical_path` (for `draw_revisions` to scroll into view), if
/// any row matched.
fn revision_lines(
    revisions: &[JsonRow],
    active_target: &str,
    selected_physical_path: &str,
) -> (Vec<Line<'static>>, Option<usize>) {
    let mut lines = Vec::new();
    let mut selected_line = None;
    append_revision_group(
        &mut lines,
        "DEVELOP",
        revisions,
        active_target,
        selected_physical_path,
        &mut selected_line,
    );
    // A rollback moved this version into DEVELOP as a re-editable candidate
    // rather than someone checking it out to edit — shown separately so it
    // doesn't read as an in-progress checkout. Unlike DEVELOP/WORKING/ARCHIVE
    // this group is rare, so it's omitted entirely rather than always shown
    // with a "(no rollback entries.)" placeholder.
    let rollback_rows = revisions
        .iter()
        .filter(|row| str_field(row, "revision_type") == "ROLLBACK")
        .collect::<Vec<_>>();
    if !rollback_rows.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "ROLLBACK",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        for row in rollback_rows {
            push_revision_row(
                &mut lines,
                row,
                active_target,
                selected_physical_path,
                &mut selected_line,
            );
        }
    }
    lines.push(Line::raw(""));
    // Between DEVELOP and ARCHIVE: newer than anything archived, not a
    // checkout. Without a group of its own this lands under "OTHER", which is
    // where every working-directory version copy used to be filed.
    append_revision_group(
        &mut lines,
        "WORKING",
        revisions,
        active_target,
        selected_physical_path,
        &mut selected_line,
    );
    lines.push(Line::raw(""));
    append_revision_group(
        &mut lines,
        "ARCHIVE",
        revisions,
        active_target,
        selected_physical_path,
        &mut selected_line,
    );
    let other_rows = revisions
        .iter()
        .filter(|row| {
            let revision_type = str_field(row, "revision_type");
            !matches!(
                revision_type.as_str(),
                "" | "DEVELOP" | "ROLLBACK" | "WORKING" | "ARCHIVE"
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
            push_revision_row(
                &mut lines,
                row,
                "",
                selected_physical_path,
                &mut selected_line,
            );
        }
    }
    (lines, selected_line)
}

#[allow(clippy::too_many_arguments)]
fn append_revision_group(
    lines: &mut Vec<Line<'static>>,
    revision_type: &str,
    revisions: &[JsonRow],
    active_target: &str,
    selected_physical_path: &str,
    selected_line: &mut Option<usize>,
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
        let row_revision_type = str_field(row, "revision_type");
        if row_revision_type == revision_type
            || (revision_type == "DEVELOP" && row_revision_type.is_empty())
        {
            push_revision_row(
                lines,
                row,
                active_target,
                selected_physical_path,
                selected_line,
            );
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

/// Push one revision's rendered line, recording its index in `selected_line`
/// when it's the currently selected entry.
fn push_revision_row(
    lines: &mut Vec<Line<'static>>,
    row: &JsonRow,
    active_target: &str,
    selected_physical_path: &str,
    selected_line: &mut Option<usize>,
) {
    let is_selected = !selected_physical_path.is_empty()
        && str_field(row, "physical_path") == selected_physical_path;
    if is_selected {
        *selected_line = Some(lines.len());
    }
    lines.push(format_revision_row(row, active_target, is_selected));
}

fn format_revision_row(row: &JsonRow, active_target: &str, selected: bool) -> Line<'static> {
    let os = str_field(row, "os_flavor");
    let user = str_field(row, "user");
    let timestamp = str_field(row, "timestamp");
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
    let text = format!("  {os:<7} {user:<12} {timestamp}{age_suffix}{active}");
    if selected {
        Line::from(Span::styled(
            text,
            Style::default().add_modifier(Modifier::REVERSED),
        ))
    } else {
        Line::raw(text)
    }
}

/// Whether this revision is the version the script's symlink resolves to.
///
/// The symlink target is a logical path and a revision carries the on-disk
/// path it was found at, so the two are compared by filename. That is exact
/// for the case it is meant to catch: vc's active-version symlinks point at a
/// sibling in the same working directory, so a matching filename there is the
/// same file. An empty target (the script is not a symlink) matches nothing.
fn is_active_revision(row: &JsonRow, active_target: &str) -> bool {
    if active_target.is_empty() {
        return false;
    }
    let file_name = |p: &str| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string();
    let physical = str_field(row, "physical_path");
    !physical.is_empty() && file_name(&physical) == file_name(active_target)
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use super::{revision_lines, wrapped_line_rows, wrapped_row_offset};

    fn line_text(line: &ratatui::text::Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
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
        let (lines, _) = revision_lines(
            &[
                revision_row("DEVELOP", "LINUX", "alice", "20240102_1200"),
                revision_row("ARCHIVE", "ZOS", "bob", "20231231_0900"),
            ],
            "",
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
        let (lines, _) = revision_lines(
            &[revision_row("WORKING", "LINUX", "", "20260701_105550")],
            "",
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
    fn wrapped_row_offset_counts_rows_before_the_selected_logical_line() {
        let lines = vec![
            ratatui::text::Line::raw("DEVELOP"),
            ratatui::text::Line::raw("  linux alice 20260910_091500"),
            ratatui::text::Line::raw("  linux bob 20260910_091501"),
        ];

        assert_eq!(wrapped_line_rows(&lines[1].to_string(), 12), 3);
        assert_eq!(wrapped_row_offset(&lines, 2, 12), 4);
    }

    #[test]
    fn revision_lines_give_rollback_entries_their_own_group() {
        // A rollback-displaced version must not read as an in-progress
        // checkout, so it gets its own labeled group rather than sitting
        // among ordinary DEVELOP rows or falling through to OTHER.
        let (lines, _) = revision_lines(
            &[revision_row("ROLLBACK", "LINUX", "abcd", "20260910_120000")],
            "",
            "",
        );

        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t == "ROLLBACK"), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t == "OTHER"),
            "a ROLLBACK row must not fall through to OTHER: {texts:?}"
        );
        let rollback_at = texts.iter().position(|t| t == "ROLLBACK").unwrap();
        let develop_at = texts.iter().position(|t| t == "DEVELOP").unwrap();
        assert!(
            develop_at < rollback_at,
            "ROLLBACK belongs after DEVELOP: {texts:?}"
        );
    }

    #[test]
    fn revision_lines_omit_rollback_group_when_no_rollback_entries() {
        // Unlike DEVELOP/WORKING/ARCHIVE, ROLLBACK is rare — no placeholder
        // line when there's nothing to show.
        let (lines, _) = revision_lines(
            &[revision_row("DEVELOP", "LINUX", "alice", "20240102_1200")],
            "",
            "",
        );

        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(!texts.iter().any(|t| t == "ROLLBACK"), "{texts:?}");
    }

    #[test]
    fn revision_lines_mark_the_version_the_symlink_points_at() {
        // Order does not imply which version is live: a rollback re-points the
        // symlink at an older version and leaves the newer one in place, so
        // here the *older* of the two is the active one.
        let (lines, _) = revision_lines(
            &[
                revision_row("WORKING", "LINUX", "", "20260729_140513"),
                revision_row("WORKING", "LINUX", "", "20260701_105550"),
            ],
            "/catalog/scripts/tool_20260701_105550",
            "",
        );

        let texts: Vec<String> = lines.iter().map(line_text).collect();
        let active: Vec<&String> = texts.iter().filter(|t| t.contains("← active")).collect();
        assert_eq!(active.len(), 1, "exactly one row is active: {texts:?}");
        assert!(active[0].contains("20260701_105550"), "{active:?}");
    }

    #[test]
    fn revision_lines_mark_nothing_when_the_script_is_not_a_symlink() {
        let (lines, _) = revision_lines(
            &[revision_row("WORKING", "LINUX", "", "20260701_105550")],
            "",
            "",
        );
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(!texts.iter().any(|t| t.contains("← active")), "{texts:?}");
    }

    #[test]
    fn revision_lines_reports_the_selected_rows_line_index() {
        let (lines, selected_line) = revision_lines(
            &[
                revision_row("DEVELOP", "LINUX", "alice", "20240102_1200"),
                revision_row("ARCHIVE", "ZOS", "bob", "20231231_0900"),
            ],
            "",
            "/srv/scripts/tool_20231231_0900",
        );

        let selected_line = selected_line.expect("a row matched the selected physical_path");
        assert!(
            line_text(&lines[selected_line]).contains("20231231_0900"),
            "line {selected_line} should be the ARCHIVE/bob row: {:?}",
            line_text(&lines[selected_line])
        );
    }

    #[test]
    fn revision_lines_selected_line_is_none_when_nothing_matches() {
        let (_, selected_line) = revision_lines(
            &[revision_row("DEVELOP", "LINUX", "alice", "20240102_1200")],
            "",
            "/no/such/path",
        );
        assert_eq!(selected_line, None);
    }
}
