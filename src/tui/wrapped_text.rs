//! Long, read-only text for the scrollable panes (catalog preview, detail
//! view tail, script diff), with its wrapped layout cached per width.
//!
//! A `Paragraph` with `.scroll()` still wraps every line above the viewport
//! on each render, and its `line_count` wraps the whole text again. For a
//! 20k-line preview that was hundreds of milliseconds per frame (#139).
//! Each source line wraps independently of its neighbours, so the row
//! offset of every line is computed once per width. A frame then clones and
//! wraps only the lines that intersect the viewport.

use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use rayon::prelude::*;

/// Below this many lines a sequential layout is already cheap.
const PARALLEL_LAYOUT_LINES: usize = 4096;

#[derive(Debug, Default)]
pub(super) struct WrappedText {
    lines: Vec<Line<'static>>,
    /// Wrap width `row_starts` was computed for.
    layout_width: Option<u16>,
    /// `row_starts[i]` is the first wrapped row of line `i`; the extra last
    /// entry is the total row count.
    row_starts: Vec<usize>,
}

/// Wrapped row count of one line at `width`, matching `Paragraph`'s
/// `Wrap { trim: false }` layout.
pub(super) fn line_rows(line: &Line<'static>, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    // Most source lines fit the pane; skip the word wrapper for those.
    if line.width() <= usize::from(width) {
        return 1;
    }
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width)
}

impl WrappedText {
    pub(super) fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            layout_width: None,
            row_starts: Vec::new(),
        }
    }

    /// Plain text split into unstyled lines, as `Text::from(&str)` would.
    pub(super) fn from_plain(text: &str) -> Self {
        Self::new(
            text.lines()
                .map(|line| Line::from(line.to_owned()))
                .collect(),
        )
    }

    pub(super) fn lines(&self) -> &[Line<'static>] {
        &self.lines
    }

    pub(super) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn ensure_layout(&mut self, width: u16) {
        if self.layout_width == Some(width) {
            return;
        }
        // Rows per line are independent, so a large text (a width change on
        // a multi-megabyte script) is laid out in parallel.
        let rows: Vec<usize> = if self.lines.len() >= PARALLEL_LAYOUT_LINES {
            self.lines
                .par_iter()
                .map(|line| line_rows(line, width))
                .collect()
        } else {
            self.lines
                .iter()
                .map(|line| line_rows(line, width))
                .collect()
        };
        self.row_starts.clear();
        self.row_starts.reserve(rows.len() + 1);
        let mut row = 0;
        for rows in rows {
            self.row_starts.push(row);
            row += rows;
        }
        self.row_starts.push(row);
        self.layout_width = Some(width);
    }

    /// Total wrapped rows at `width`.
    pub(super) fn row_count(&mut self, width: u16) -> usize {
        self.ensure_layout(width);
        self.row_starts.last().copied().unwrap_or(0)
    }

    /// The lines covering wrapped rows `[first_row, first_row + height)` at
    /// `width`, plus how many rows of the first returned line lie above
    /// `first_row` (the `Paragraph` scroll to apply to the window).
    pub(super) fn window(
        &mut self,
        width: u16,
        first_row: usize,
        height: usize,
    ) -> (&[Line<'static>], usize) {
        self.ensure_layout(width);
        let Some((&total, starts)) = self.row_starts.split_last() else {
            return (&[], 0);
        };
        if first_row >= total || height == 0 {
            return (&[], 0);
        }
        // Last line starting at or before `first_row`.
        let start = starts.partition_point(|&row| row <= first_row) - 1;
        // Every line starting before the window's last row.
        let end = starts.partition_point(|&row| row < first_row.saturating_add(height));
        let skip = first_row - starts[start];
        (&self.lines[start..end], skip)
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Paragraph, Wrap};
    use ratatui::{Terminal, backend::TestBackend};

    use super::WrappedText;

    fn sample() -> Vec<Line<'static>> {
        vec![
            Line::from("short"),
            Line::from(""),
            Line::from("a fairly long line that will certainly wrap at narrow widths"),
            Line::from(vec![
                Span::styled("styled ", Style::default().fg(Color::Red)),
                Span::raw("工具 分析 🚀 wide graphemes ".repeat(4)),
            ]),
            Line::from("    "),
            Line::from("unbroken_token_".repeat(9)),
            Line::from("tail"),
        ]
    }

    fn render(paragraph: Paragraph<'_>, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| frame.render_widget(paragraph, frame.area()))
            .unwrap();
        format!("{:?}", terminal.backend().buffer())
    }

    #[test]
    fn row_count_matches_whole_paragraph_line_count() {
        for width in [1, 3, 7, 12, 25, 80, 200] {
            let mut text = WrappedText::new(sample());
            let whole = Paragraph::new(sample())
                .wrap(Wrap { trim: false })
                .line_count(width);
            assert_eq!(text.row_count(width), whole, "width {width}");
        }
    }

    #[test]
    fn windowed_render_matches_whole_paragraph_scroll() {
        for width in [4, 9, 17, 40] {
            let height = 5;
            let mut text = WrappedText::new(sample());
            let total = text.row_count(width);
            for scroll in 0..total {
                let whole = Paragraph::new(sample())
                    .wrap(Wrap { trim: false })
                    .scroll((u16::try_from(scroll).unwrap(), 0));
                let (lines, skip) = text.window(width, scroll, usize::from(height));
                let windowed = Paragraph::new(lines.to_vec())
                    .wrap(Wrap { trim: false })
                    .scroll((u16::try_from(skip).unwrap(), 0));
                assert_eq!(
                    render(whole, width, height),
                    render(windowed, width, height),
                    "width {width} scroll {scroll}"
                );
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn line_rows_matches_paragraph_line_count(
            text in "[a-z \t工🚀\u{301}\u{200d}\u{fe0f}é-]{0,60}",
            width in 1u16..30,
        ) {
            let line = Line::from(text);
            let whole = Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width);
            proptest::prop_assert_eq!(super::line_rows(&line, width), whole);
        }
    }

    #[test]
    fn layout_is_recomputed_when_width_changes() {
        let mut text = WrappedText::new(vec![Line::from("abcdefghij")]);
        assert_eq!(text.row_count(10), 1);
        assert_eq!(text.row_count(5), 2);
        assert_eq!(text.row_count(10), 1);
    }

    #[test]
    fn window_past_the_end_or_zero_width_is_empty() {
        let mut text = WrappedText::new(sample());
        let total = text.row_count(10);
        assert!(text.window(10, total, 5).0.is_empty());
        assert_eq!(text.row_count(0), 0);
        assert!(text.window(0, 0, 5).0.is_empty());
        assert!(WrappedText::default().window(10, 0, 5).0.is_empty());
    }
}
