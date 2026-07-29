use super::{
    FULL_PAGE_SCROLL_LINES, Focus, HALF_PAGE_SCROLL_LINES, PAGE_SCROLL_LINES, ScrollCommand,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use scat_core::core::db::JsonRow;
use scat_core::core::vc::compare_revision_rows;

pub(super) fn sort_checkouts(checkouts: &mut [JsonRow]) {
    checkouts.sort_by(compare_revision_rows);
}

pub(super) fn search_title(has_error: bool, is_searching: bool) -> &'static str {
    if has_error {
        "Search (invalid query)"
    } else if is_searching {
        "Search (searching…)"
    } else {
        "Search"
    }
}

pub(super) fn next_focus(focus: Focus) -> Focus {
    match focus {
        Focus::Search => Focus::Results,
        Focus::Results => Focus::Preview,
        Focus::Preview => Focus::Deps,
        Focus::Deps => Focus::Functions,
        Focus::Functions => Focus::Revisions,
        Focus::Revisions => Focus::Search,
    }
}

pub(super) fn previous_focus(focus: Focus) -> Focus {
    match focus {
        Focus::Search => Focus::Revisions,
        Focus::Results => Focus::Search,
        Focus::Preview => Focus::Results,
        Focus::Deps => Focus::Preview,
        Focus::Functions => Focus::Deps,
        Focus::Revisions => Focus::Functions,
    }
}

pub(super) fn move_selection(selected: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len - 1;
    selected.saturating_add_signed(delta).min(max)
}

pub(super) fn scroll_by(current: u16, delta: i16) -> u16 {
    current.saturating_add_signed(delta)
}

fn scroll_command(key: KeyEvent) -> Option<ScrollCommand> {
    match key {
        KeyEvent {
            code: KeyCode::Up, ..
        }
        | KeyEvent {
            code: KeyCode::Char('k'),
            ..
        } => Some(ScrollCommand::Delta(-1)),
        KeyEvent {
            code: KeyCode::Down,
            ..
        }
        | KeyEvent {
            code: KeyCode::Char('j'),
            ..
        } => Some(ScrollCommand::Delta(1)),
        KeyEvent {
            code: KeyCode::PageUp,
            ..
        } => Some(ScrollCommand::Delta(-PAGE_SCROLL_LINES)),
        KeyEvent {
            code: KeyCode::PageDown,
            ..
        } => Some(ScrollCommand::Delta(PAGE_SCROLL_LINES)),
        KeyEvent {
            code: KeyCode::Char('u'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(ScrollCommand::Delta(-HALF_PAGE_SCROLL_LINES)),
        KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(ScrollCommand::Delta(HALF_PAGE_SCROLL_LINES)),
        KeyEvent {
            code: KeyCode::Char('b'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(ScrollCommand::Delta(-FULL_PAGE_SCROLL_LINES)),
        KeyEvent {
            code: KeyCode::Char('f'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(ScrollCommand::Delta(FULL_PAGE_SCROLL_LINES)),
        KeyEvent {
            code: KeyCode::Char('g'),
            modifiers: KeyModifiers::NONE,
            ..
        }
        | KeyEvent {
            code: KeyCode::Home,
            ..
        } => Some(ScrollCommand::Top),
        _ => None,
    }
}

fn apply_scroll_command(scroll: &mut u16, command: ScrollCommand) {
    match command {
        ScrollCommand::Delta(delta) => *scroll = scroll_by(*scroll, delta),
        ScrollCommand::Top => *scroll = 0,
    }
}

pub(super) fn apply_scroll_key(scroll: &mut u16, key: KeyEvent) -> bool {
    let Some(command) = scroll_command(key) else {
        return false;
    };
    apply_scroll_command(scroll, command);
    true
}
