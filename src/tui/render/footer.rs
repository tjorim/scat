//! The bottom footer: key hints, or a transient flash status.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::TuiApp;
use super::common::hint_key;

pub(super) fn draw_footer(frame: &mut Frame<'_>, app: &TuiApp, area: Rect) {
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
        hint_key("s"),
        Span::raw(" stats"),
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
