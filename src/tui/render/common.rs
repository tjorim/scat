//! Rendering helpers shared across panes: spinner animation, list-window
//! scrolling/virtualization, text truncation, and styling.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, List, ListItem, ListState};

use super::super::Focus;

const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub(super) fn spinner_char(tick: u64) -> char {
    SPINNER_FRAMES[(tick as usize) % SPINNER_FRAMES.len()]
}

/// Scroll offset for a pane of uniform single-line items, keeping the
/// selected item within the visible window. `prev_offset` is preserved
/// as-is when the selection is already visible (matching ratatui's `List`
/// widget behavior for uniform-height items) so a pane doesn't jump to put
/// the selection at an edge every frame — only when it would otherwise fall
/// off-screen. Shared by every list-shaped pane (Results, Deps, Functions)
/// so their scrolling can't silently drift apart.
pub(super) fn scroll_window(
    prev_offset: usize,
    selected: usize,
    len: usize,
    inner_height: usize,
) -> usize {
    if len == 0 || inner_height == 0 {
        return 0;
    }
    let selected = selected.min(len - 1);
    let mut offset = prev_offset.min(len - 1);
    if selected < offset {
        offset = selected;
    } else if selected >= offset + inner_height {
        offset = selected + 1 - inner_height;
    }
    offset.min(len.saturating_sub(inner_height))
}

/// Truncate `text` to at most `max_chars` display columns, marking
/// truncation with a trailing ellipsis. Used for free-form labels (a
/// dependency's path, a function's docstring) that must render as exactly
/// one row — unlike wrapping, this guarantees the row a click resolves to is
/// always the row it visually points at.
pub(super) fn truncate_line(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let keep: String = text.chars().take(max_chars - 1).collect();
    format!("{keep}…")
}

/// Render the already-sliced visible window of a list pane. `selected_in_window`
/// is the selected index relative to the start of `items` (not the full
/// source list), since only the window — not the whole list — is ever handed
/// to the widget.
pub(super) fn render_window(
    frame: &mut Frame<'_>,
    area: Rect,
    block: Block<'_>,
    items: Vec<ListItem>,
    selected_in_window: usize,
) {
    let mut window_state = ListState::default();
    window_state.select(Some(selected_in_window));
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::REVERSED)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut window_state);
}

/// Render a bordered pane as a virtualized, single-selection list of
/// pre-formatted `labels`: only the rows inside the visible window become
/// `ListItem`s (truncated, never wrapped), and the real full-list offset is
/// written back into `state` for mouse hit-testing. Shared by the Deps and
/// Functions panes — Results uses the same `scroll_window` math but builds
/// its window lazily from `JsonRow`s since its item count can run into the
/// thousands, where Deps/Functions labels are cheap to format in full.
pub(super) fn render_list_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    block: Block<'_>,
    labels: &[String],
    selected: usize,
    state: &mut ListState,
) {
    let inner_height = area.height.saturating_sub(2) as usize;
    let len = labels.len();
    let selected = selected.min(len - 1);
    let offset = scroll_window(state.offset(), selected, len, inner_height);
    *state.offset_mut() = offset;

    let end = (offset + inner_height.max(1)).min(len);
    let max_chars = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = labels[offset..end]
        .iter()
        .map(|line| ListItem::new(truncate_line(line, max_chars)))
        .collect();

    render_window(frame, area, block, items, selected - offset);
}

pub(super) fn line_count(text: &str) -> usize {
    text.lines().count().max(1)
}

pub(super) fn clamp_scroll_offset(scroll: &mut u16, line_count: usize, area: Rect) {
    let viewport_lines = usize::from(area.height.saturating_sub(2)).max(1);
    let max_scroll = line_count.saturating_sub(viewport_lines);
    *scroll = (*scroll).min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
}

/// Returns a styled span for a key hint label.
pub(super) fn hint_key(key: &'static str) -> ratatui::text::Span<'static> {
    ratatui::text::Span::styled(
        key,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

/// Border style for a pane: bold yellow when active, dark gray when inactive.
pub(super) fn focus_border(current: Focus, target: Focus) -> Style {
    if current == target {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Left-truncate a path to `max_chars`, preferring a `…/parent/file` form.
pub(super) fn left_truncate_path(path: &str, max_chars: usize) -> String {
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
        .map_or(path.len(), |(idx, _)| idx);
    format!("…{}", &path[start_idx..])
}

#[cfg(test)]
mod tests {
    use super::*;

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
