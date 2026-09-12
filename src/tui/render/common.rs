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
/// off-screen. Shared by every uniform-height list-shaped pane (Deps,
/// Functions, Revisions) so their scrolling can't silently drift apart; see
/// [`scroll_window_variable`] for the Results pane, whose items don't all
/// render as exactly one row.
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

/// Like [`scroll_window`], but for a pane whose items can render as more
/// than one terminal row (Results, when a row's symlink target gets its own
/// wrapped line): `height_of(i)` returns how many rows item `i` occupies.
/// Returns `(offset, end)` — the half-open range of item indices whose
/// combined rendered height fits within `inner_height` rows, chosen with the
/// same "keep `prev_offset` when the selection is already visible, else
/// scroll the minimal amount" rule as `scroll_window`, and never including a
/// trailing item that would be cut off partway through.
///
/// The extra bookkeeping here — summing `height_of` over a range rather than
/// flat index arithmetic — costs O(the number of items actually scrolled
/// past this frame), not O(`len`): ordinary up/down navigation moves
/// `selected` one item at a time, so `offset` is already close to where it
/// needs to be; only a big jump (Home/End, a fresh search) pays for a
/// larger one-off sum.
pub(super) fn scroll_window_variable(
    prev_offset: usize,
    selected: usize,
    len: usize,
    inner_height: usize,
    height_of: impl Fn(usize) -> usize,
) -> (usize, usize) {
    if len == 0 || inner_height == 0 {
        return (0, 0);
    }
    let selected = selected.min(len - 1);
    let mut offset = prev_offset.min(len - 1);

    if selected < offset {
        offset = selected;
    } else {
        // Advance offset while [offset, selected] doesn't fit within
        // inner_height rows. The sum is sought once, then maintained
        // incrementally (subtracting the row(s) that fall out of the front
        // of the range) rather than re-summed every step.
        let mut used: usize = (offset..=selected).map(&height_of).sum();
        while used > inner_height && offset < selected {
            used -= height_of(offset);
            offset += 1;
        }
    }

    // Don't leave blank trailing rows: cap offset at the smallest value
    // whose suffix still fills the window exactly (or the list doesn't fill
    // the window at all, in which case this settles at 0). Bounded by how
    // many items it takes to fill inner_height rows from the end — every
    // item is at least 1 row, so at most inner_height of them — not by
    // `len`.
    let mut max_offset = 0usize;
    let mut used = 0usize;
    for i in (0..len).rev() {
        used += height_of(i);
        if used > inner_height {
            max_offset = i + 1;
            break;
        }
    }
    offset = offset.min(max_offset);

    // How many items starting at offset fit within inner_height rows, always
    // including at least one even if it alone overflows (matching
    // `render_window`'s expectation that the window is never empty when
    // `len > 0`).
    let mut end = offset;
    let mut used = 0usize;
    while end < len {
        let h = height_of(end);
        if used > 0 && used + h > inner_height {
            break;
        }
        used += h;
        end += 1;
        if used >= inner_height {
            break;
        }
    }

    (offset, end)
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

    // -----------------------------------------------------------------------
    // scroll_window_variable
    // -----------------------------------------------------------------------

    #[test]
    fn scroll_window_variable_matches_scroll_window_when_uniform() {
        // Every item 1 row: must degenerate to exactly what `scroll_window`
        // computes for offset, and end must always be offset + inner_height.
        for (prev_offset, selected, len, inner_height) in [
            (0, 5, 100, 10),
            (80, 95, 100, 10),
            (80, 85, 100, 10),
            (0, 0, 1, 10),
            (0, 99, 100, 10),
        ] {
            let expected_offset = scroll_window(prev_offset, selected, len, inner_height);
            let (offset, end) =
                scroll_window_variable(prev_offset, selected, len, inner_height, |_| 1);
            assert_eq!(offset, expected_offset, "case {prev_offset:?}");
            assert_eq!(end, (offset + inner_height).min(len));
        }
    }

    #[test]
    fn scroll_window_variable_keeps_prev_offset_when_selection_visible() {
        // heights: item 2 is 2 rows tall, everything else 1 row.
        let height_of = |i: usize| if i == 2 { 2 } else { 1 };
        let (offset, _) = scroll_window_variable(0, 3, 10, 5, height_of);
        // [0,1,2,3] sums to 1+1+2+1=5, fits exactly within inner_height=5.
        assert_eq!(offset, 0);
    }

    #[test]
    fn scroll_window_variable_scrolls_up_when_selection_above_window() {
        let (offset, _) = scroll_window_variable(5, 1, 10, 3, |_| 1);
        assert_eq!(offset, 1);
    }

    #[test]
    fn scroll_window_variable_advances_offset_to_fit_a_tall_selected_item() {
        // Item 4 is 2 rows; window is 3 rows. Starting at offset 0, [0..=4]
        // is 1+1+1+1+2=6 rows — too many — so offset must advance until the
        // selected item's rows fit.
        let height_of = |i: usize| if i == 4 { 2 } else { 1 };
        let (offset, end) = scroll_window_variable(0, 4, 10, 3, height_of);
        // [offset..=4] must sum to <= 3: offset=3 -> [3,4] = 1+2 = 3. Fits.
        assert_eq!(offset, 3);
        assert_eq!(end, 5);
    }

    #[test]
    fn scroll_window_variable_never_includes_a_partially_cut_item() {
        // heights: [2, 2, 2, 2], inner_height=3 — only one 2-row item fits
        // per window without cutting the next one in half.
        let (offset, end) = scroll_window_variable(0, 0, 4, 3, |_| 2);
        assert_eq!(offset, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn scroll_window_variable_always_includes_at_least_one_item() {
        // A single item taller than the window must still be shown (not
        // produce an empty window).
        let (offset, end) = scroll_window_variable(0, 0, 3, 1, |_| 5);
        assert_eq!(offset, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn scroll_window_variable_does_not_leave_trailing_blank_space() {
        // 5 items of 1 row each, inner_height=3, selected near the start —
        // but since only 5 items exist total, offset must not exceed 2
        // (so the last window [2,3,4] fills all 3 rows).
        let (offset, end) = scroll_window_variable(10, 0, 5, 3, |_| 1);
        assert_eq!(offset, 0);
        assert_eq!(end, 3);
    }

    #[test]
    fn scroll_window_variable_empty_list_returns_empty_window() {
        assert_eq!(scroll_window_variable(0, 0, 0, 10, |_| 1), (0, 0));
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
