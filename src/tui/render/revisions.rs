//! The Revisions pane: DEVELOP/WORKING/ARCHIVE checkout history, grouped and
//! marked with the version the script's symlink currently resolves to.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use scat_core::core::db::{JsonRow, row_string as str_field};
use scat_core::core::script_view::ScriptView;
use scat_core::core::vc::relative_age;

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
fn revision_lines(revisions: &[JsonRow], active_target: &str) -> Vec<Line<'static>> {
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
            let revision_type = str_field(row, "revision_type");
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
    revisions: &[JsonRow],
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
        let row_revision_type = str_field(row, "revision_type");
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

fn format_revision_row(row: &JsonRow, active_target: &str) -> String {
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
    format!("  {os:<7} {user:<12} {timestamp}{age_suffix}{active}")
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

    use super::revision_lines;

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
}
