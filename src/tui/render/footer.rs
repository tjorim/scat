use super::super::TuiApp;
use ratatui::{Frame, layout::Rect, widgets::Paragraph};
pub(super) fn draw_footer(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    let hints = app
        .actions()
        .into_iter()
        .filter(|a| a.available)
        .take(6)
        .map(|a| format!("{} {}", a.shortcut, a.label))
        .collect::<Vec<_>>()
        .join("  ");
    frame.render_widget(
        Paragraph::new(
            app.error
                .as_deref()
                .or(app.flash.as_deref())
                .unwrap_or(&hints),
        ),
        area,
    );
}
