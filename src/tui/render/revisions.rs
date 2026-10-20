//! The Revisions pane: DEVELOP/WORKING/ARCHIVE checkout history, grouped and
//! marked with the version the script's symlink currently resolves to.
//!
//! Columns collapse with width; selection always uses the original checkout index.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use scat_core::core::db::{JsonRow, row_string as str_field};
use scat_core::core::script_view::ScriptView;

use super::super::{Focus, TuiApp};
use super::common::{focus_border, spinner_char};

pub(super) fn draw_revisions(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    use ratatui::layout::Constraint;
    use ratatui::widgets::{Cell, Row, Table};
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Revisions")
        .border_style(focus_border(app.focus, Focus::Revisions));
    if app.detail_loading || app.checkouts.is_empty() {
        app.revisions_scroll = 0;
        let text = if app.detail_loading {
            format!("{} Loading…", spinner_char(app.tick))
        } else {
            "No revision data.".into()
        };
        frame.render_widget(Paragraph::new(text).block(block), area);
        return;
    }
    let active = app
        .detail
        .as_ref()
        .map(ScriptView::new)
        .map(|v| v.symlink_target().to_string())
        .unwrap_or_default();
    // Group visually while retaining indices into the original checkout list.
    let indices = app.revision_order();
    let selected = indices
        .iter()
        .position(|i| *i == app.revisions_selected)
        .unwrap_or(0);
    let viewport = usize::from(area.height.saturating_sub(3));
    let offset = super::common::scroll_window(
        usize::from(app.revisions_scroll),
        selected,
        indices.len(),
        viewport,
    );
    app.revisions_scroll = u16::try_from(offset).unwrap_or(u16::MAX);
    // Owner collapses first (<72), timestamp next (<48); type and path remain.
    let owner = area.width >= 72;
    let timestamp = area.width >= 48;
    let mut widths = vec![Constraint::Length(10)];
    let mut headings = vec!["Type"];
    if timestamp {
        widths.push(Constraint::Length(20));
        headings.push("Timestamp");
    }
    if owner {
        widths.push(Constraint::Length(12));
        headings.push("Owner");
    }
    widths.push(Constraint::Min(1));
    headings.push("Checkout path");
    let visible = &indices[offset..(offset + viewport).min(indices.len())];
    let rows: Vec<_> = visible
        .iter()
        .map(|i| {
            let row = &app.checkouts[*i];
            let kind = str_field(row, "revision_type");
            let kind = if kind.is_empty() {
                "DEVELOP".into()
            } else {
                kind
            };
            let mut cells = vec![Cell::from(format!(
                "{}{}",
                if is_active_revision(row, &active) {
                    "*"
                } else {
                    ""
                },
                kind
            ))];
            if timestamp {
                cells.push(Cell::from(str_field(row, "timestamp")));
            }
            if owner {
                cells.push(Cell::from(str_field(row, "user")));
            }
            cells.push(Cell::from(str_field(row, "physical_path")));
            Row::new(cells).style(if *i == app.revisions_selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            })
        })
        .collect();
    frame.render_widget(
        Table::new(rows, widths)
            .header(Row::new(headings).style(Style::default().fg(Color::Cyan)))
            .block(block),
        area,
    );
    // Exclude the column header from the scrollbar track and mouse row mapping.
    let body = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    super::common::draw_scrollbar(frame, body, indices.len(), offset, viewport);
    app.record_region_with_row_index(body, super::super::RegionKind::Revisions, visible.to_vec());
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
