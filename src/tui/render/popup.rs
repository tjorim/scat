use super::super::TuiApp;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
};

pub(super) fn draw_popup(frame: &mut Frame<'_>, app: &TuiApp) {
    super::super::search_settings::draw(frame, app);
    let Some(popup) = &app.popup else {
        return;
    };
    let screen = frame.area();
    let width = screen.width.min(76);
    let height = screen.height.min(24);
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let title = if popup.palette {
        format!("Commands: {} | Esc closes", popup.query)
    } else {
        "Help | Up/Down scroll | Esc closes".into()
    };
    let items: Vec<_> = app
        .filtered_actions()
        .into_iter()
        .map(|a| {
            ListItem::new(format!(
                "{:10} {}{}",
                a.shortcut,
                a.label,
                if a.available { "" } else { " (unavailable)" }
            ))
            .style(Style::default().fg(if a.available {
                Color::White
            } else {
                Color::DarkGray
            }))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(popup.selected));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().bg(Color::Blue)),
        area,
        &mut state,
    );
}
