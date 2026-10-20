use super::super::TuiApp;
use ratatui::{Frame, layout::Rect, widgets::Paragraph};
pub(super) fn draw_footer(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
    // F-key shortcuts are global; keep help and the palette discoverable and
    // spend the remaining slots on actions specific to the current context.
    let is_global = |shortcut: &str| {
        shortcut
            .strip_prefix('F')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let actions = app.actions();
    let hints = actions
        .iter()
        .filter(|a| a.available && !is_global(a.shortcut))
        .take(4)
        .chain(actions.iter().filter(|a| matches!(a.shortcut, "F1" | "F4")))
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
