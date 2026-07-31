//! The Deps pane: outbound/inbound dependencies, or call sites when a
//! function cross-reference is active.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, List, ListItem};

use super::super::{Focus, TuiApp};
use super::common::{focus_border, render_list_pane, spinner_char};

pub(super) fn draw_deps(frame: &mut Frame<'_>, app: &mut TuiApp, area: Rect) {
    let spinner = spinner_char(app.tick);
    let border_style = focus_border(app.focus, Focus::Deps);

    if app.detail_loading {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Deps (loading…)")
            .border_style(border_style);
        let items = vec![ListItem::new(format!("{spinner} Loading…"))];
        frame.render_widget(List::new(items).block(block), area);
        return;
    }

    let (labels, title, empty_message) = if let Some(function_name) = &app.function_xref {
        let labels = app
            .xref_call_sites()
            .map(|sites| {
                sites
                    .iter()
                    .map(|site| {
                        format!(
                            "{:<7} {}:{}  {} -> {}",
                            "calls", site.caller_path, site.line, site.caller, site.callee
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        (
            labels,
            format!("Deps (call sites for {function_name})"),
            "No call sites.",
        )
    } else {
        let labels = app
            .deps
            .iter()
            .map(|item| format!("{:<7} {}", item.kind, item.logical_path))
            .collect::<Vec<_>>();
        (labels, "Deps".to_string(), "No dependencies.")
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    if labels.is_empty() {
        let items = vec![ListItem::new(ratatui::text::Span::styled(
            empty_message,
            Style::default().fg(Color::DarkGray),
        ))];
        frame.render_widget(List::new(items).block(block), area);
        return;
    }

    render_list_pane(
        frame,
        area,
        block,
        &labels,
        app.deps_selected,
        &mut app.deps_state,
    );
}
