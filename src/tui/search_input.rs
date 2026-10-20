//! Single-line search editing and terminal paste handling.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{Focus, KeyCode, KeyEvent, KeyModifiers, TuiApp, ViewMode};

impl TuiApp {
    pub(super) fn query_selection(&self) -> Option<Range<usize>> {
        self.query_anchor
            .filter(|&anchor| anchor != self.query_cursor)
            .map(|anchor| anchor.min(self.query_cursor)..anchor.max(self.query_cursor))
    }

    fn previous_query_boundary(&self) -> usize {
        self.query[..self.query_cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_query_boundary(&self) -> usize {
        self.query[self.query_cursor..]
            .graphemes(true)
            .next()
            .map_or(self.query.len(), |text| self.query_cursor + text.len())
    }

    fn previous_query_word(&self) -> usize {
        self.query[..self.query_cursor]
            .unicode_word_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_query_word(&self) -> usize {
        self.query[self.query_cursor..]
            .unicode_word_indices()
            .next()
            .map_or(self.query.len(), |(index, word)| {
                self.query_cursor + index + word.len()
            })
    }

    pub(super) fn move_query_cursor(&mut self, cursor: usize, selecting: bool) {
        if selecting {
            self.query_anchor.get_or_insert(self.query_cursor);
        } else {
            self.query_anchor = None;
        }
        self.query_cursor = cursor;
    }

    fn replace_query_range(&mut self, range: Range<usize>, text: &str) {
        if range.is_empty() && text.is_empty() {
            return;
        }
        let cursor = range.start + text.len();
        self.query.replace_range(range, text);
        // Inserting a combining mark or joiner can change grapheme boundaries.
        self.query_cursor = self
            .query
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain(std::iter::once(self.query.len()))
            .find(|&index| index >= cursor)
            .unwrap_or(self.query.len());
        self.query_anchor = None;
        self.schedule_query();
    }

    fn insert_query_text(&mut self, text: &str) {
        let range = self
            .query_selection()
            .unwrap_or(self.query_cursor..self.query_cursor);
        self.replace_query_range(range, text);
    }

    pub(super) fn handle_paste(&mut self, text: &str) -> bool {
        if self.native_selection {
            return false;
        }
        if self.mode != ViewMode::Browse || self.focus != Focus::Search {
            self.flash = Some("Focus search with / before pasting".into());
            return true;
        }
        let text: String = text
            .replace("\r\n", "\n")
            .chars()
            .filter_map(|ch| match ch {
                '\r' | '\n' | '\t' => Some(' '),
                ch if ch.is_control() => None,
                ch => Some(ch),
            })
            .collect();
        if text.is_empty() {
            return false;
        }
        self.flash = None;
        self.insert_query_text(&text);
        true
    }

    pub(super) fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.modifiers.intersects(
            KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
        ) {
            return false;
        }
        match key.code {
            KeyCode::Char('a') if control => {
                self.query_anchor = Some(0);
                self.query_cursor = self.query.len();
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                let selection = self.query_selection();
                let cursor = match key.code {
                    KeyCode::Left if !shift && selection.is_some() => selection.unwrap().start,
                    KeyCode::Right if !shift && selection.is_some() => selection.unwrap().end,
                    KeyCode::Left if control => self.previous_query_word(),
                    KeyCode::Right if control => self.next_query_word(),
                    KeyCode::Left => self.previous_query_boundary(),
                    KeyCode::Right => self.next_query_boundary(),
                    KeyCode::Home => 0,
                    _ => self.query.len(),
                };
                self.move_query_cursor(cursor, shift);
            }
            KeyCode::Backspace | KeyCode::Delete => {
                let range = self.query_selection().unwrap_or_else(|| {
                    if key.code == KeyCode::Backspace {
                        let start = if control {
                            self.previous_query_word()
                        } else {
                            self.previous_query_boundary()
                        };
                        start..self.query_cursor
                    } else {
                        let end = if control {
                            self.next_query_word()
                        } else {
                            self.next_query_boundary()
                        };
                        self.query_cursor..end
                    }
                });
                self.replace_query_range(range, "");
            }
            KeyCode::Char('w') if control => {
                let range = self
                    .query_selection()
                    .unwrap_or(self.previous_query_word()..self.query_cursor);
                self.replace_query_range(range, "");
            }
            KeyCode::Char('u') if control => self.replace_query_range(0..self.query_cursor, ""),
            KeyCode::Char('k') if control => {
                self.replace_query_range(self.query_cursor..self.query.len(), "");
            }
            KeyCode::Esc if self.query_selection().is_some() => self.query_anchor = None,
            KeyCode::Char(ch) if !control && !ch.is_control() => {
                self.insert_query_text(&ch.to_string());
            }
            _ => return false,
        }
        true
    }

    /// Translate a mouse cell into a byte offset without splitting a grapheme.
    pub(super) fn query_cursor_at_column(&self, column: u16) -> usize {
        let target = usize::from(
            column
                .saturating_sub(self.query_area.x)
                .min(self.query_area.width.saturating_sub(1)),
        );
        let mut width = 0;
        for (index, text) in self.query[self.query_view_start..].grapheme_indices(true) {
            let next = width + text.width();
            if target < next {
                return self.query_view_start
                    + index
                    + if target - width >= text.width().div_ceil(2) {
                        text.len()
                    } else {
                        0
                    };
            }
            width = next;
        }
        self.query.len()
    }
}
