use super::{
    ClickRegion, DOUBLE_CLICK_MS, Focus, Instant, KeyEvent, MouseButton, MouseEvent,
    MouseEventKind, Rect, RegionKind, Result, TuiApp, ViewMode, apply_scroll_key, clipboard,
    detail, hit_test, inner_rect, move_selection, scroll_by,
};

impl TuiApp {
    pub(super) fn scroll_target(&mut self) -> Option<&mut u16> {
        match self.focus {
            Focus::Preview => Some(&mut self.preview_scroll),
            // Revisions moved to selection-based navigation (Up/Down move
            // `revisions_selected`, matching Deps/Functions) — its own key
            // handlers in `handlers.rs` intercept before this fallback runs,
            // and `revisions_scroll` auto-follows the selection rather than
            // being driven directly by scroll keys.
            _ => None,
        }
    }

    pub(super) fn apply_focused_scroll(&mut self, key: KeyEvent) -> bool {
        let Some(scroll) = self.scroll_target() else {
            return false;
        };
        apply_scroll_key(scroll, key)
    }

    /// Whether a spinner is currently on screen and needs its frame advanced.
    /// Only these states animate; when none hold, the screen is static and the
    /// run loop can leave it untouched (preserving any mouse selection).
    pub(super) fn is_animating(&self) -> bool {
        (self.search_in_flight && self.results.is_empty())
            || self.detail_loading
            || self.detail_diff_loading
            || self.stats_loading
    }

    /// Record a clickable pane for this frame. `outer` is the bordered rect;
    /// the stored region is its inner content area. Called from `render`.
    pub(super) fn record_region(&mut self, outer: Rect, kind: RegionKind, scroll: usize) {
        self.record_click_area(inner_rect(outer), kind, scroll);
    }

    /// Record a clickable region using `area` verbatim (no border inset), for
    /// borderless surfaces like the header line.
    pub(super) fn record_click_area(&mut self, area: Rect, kind: RegionKind, scroll: usize) {
        self.click_regions.push(ClickRegion {
            area,
            kind,
            scroll,
            row_item_index: None,
        });
    }

    /// Like `record_region`, but for a pane whose items can span more than
    /// one screen row — see `ClickRegion::row_item_index`. `row_item_index`
    /// is the render's own row→item map (e.g. `app.results_row_index`),
    /// covering only the rows actually drawn this frame.
    pub(super) fn record_region_with_row_index(
        &mut self,
        outer: Rect,
        kind: RegionKind,
        row_item_index: Vec<usize>,
    ) {
        self.click_regions.push(ClickRegion {
            area: inner_rect(outer),
            kind,
            scroll: 0,
            row_item_index: Some(row_item_index),
        });
    }

    /// Handle a mouse event, returning whether it changed anything (and thus
    /// needs a repaint). Drags select search text; bare movement is ignored.
    pub(super) fn handle_mouse(&mut self, event: MouseEvent) -> Result<bool> {
        if self.workflow.is_some()
            || self.popup.is_some()
            || self.inspection.is_some()
            || self.search_settings.is_some()
        {
            return Ok(false);
        }
        if self.native_selection {
            return Ok(false);
        }
        match event.kind {
            MouseEventKind::Drag(MouseButton::Left) if self.query_dragging => {
                let cursor = self.query_cursor_at_column(event.column);
                self.move_query_cursor(cursor, true);
                Ok(true)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.query_dragging = false;
                Ok(false)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.flash = None;
                self.query_dragging = false;
                if self.query_area.contains((event.column, event.row).into())
                    && event.modifiers.contains(super::KeyModifiers::SHIFT)
                    && hit_test(&self.click_regions, event.column, event.row)
                        .is_some_and(|(kind, _)| kind == RegionKind::Search)
                {
                    let cursor = self.query_cursor_at_column(event.column);
                    self.move_query_cursor(cursor, true);
                    self.query_dragging = true;
                    self.focus = Focus::Search;
                    return Ok(true);
                }
                self.handle_left_click(event.column, event.row)
            }
            MouseEventKind::ScrollDown => self.handle_scroll(event.column, event.row, 1),
            MouseEventKind::ScrollUp => self.handle_scroll(event.column, event.row, -1),
            _ => Ok(false),
        }
    }

    /// Record a click for double-click detection; return whether it completes
    /// a double-click (two clicks on the same cell within [`DOUBLE_CLICK_MS`]).
    pub(super) fn register_click(&mut self, col: u16, row: u16) -> bool {
        let now = Instant::now();
        let is_double = self.last_click.is_some_and(|(c, r, at)| {
            c == col && r == row && now.duration_since(at).as_millis() <= DOUBLE_CLICK_MS
        });
        // Reset after a double so a third click starts a fresh pair.
        self.last_click = if is_double {
            None
        } else {
            Some((col, row, now))
        };
        is_double
    }

    pub(super) fn handle_left_click(&mut self, col: u16, row: u16) -> Result<bool> {
        if let Some((_, focus)) = self
            .pane_tabs
            .iter()
            .find(|(area, _)| area.contains((col, row).into()))
        {
            self.focus = *focus;
            return Ok(true);
        }
        let double = self.register_click(col, row);
        let Some((kind, index)) = hit_test(&self.click_regions, col, row) else {
            return Ok(false);
        };
        match kind {
            RegionKind::Header => {
                // The header displays the selected script's path; clicking it
                // copies the full path (handy in fullscreen, where the
                // Metadata pane isn't shown).
                self.copy_selected_path();
                Ok(true)
            }
            RegionKind::Search => {
                self.focus = Focus::Search;
                if self.query_area.contains((col, row).into()) {
                    let cursor = self.query_cursor_at_column(col);
                    self.move_query_cursor(cursor, false);
                    self.query_anchor = Some(cursor);
                    self.query_dragging = true;
                }
                Ok(true)
            }
            RegionKind::Results => {
                if index >= self.results.len() {
                    return Ok(false);
                }
                self.focus = Focus::Results;
                if index != self.selected {
                    self.selected = index;
                    self.load_selected()?;
                }
                // Double-click opens the full detail view, mirroring Enter.
                if double {
                    self.mode = ViewMode::Detail;
                }
                Ok(true)
            }
            RegionKind::Metadata => {
                self.copy_selected_path();
                Ok(true)
            }
            RegionKind::Deps => {
                self.focus = Focus::Deps;
                if index < self.dependency_target_count() {
                    // Click again (or double-click) on the highlighted dep to
                    // navigate; first click just selects it.
                    if double || index == self.deps_selected {
                        self.deps_selected = index;
                        self.open_selected_dependency()?;
                    } else {
                        self.deps_selected = index;
                    }
                }
                Ok(true)
            }
            RegionKind::Functions => {
                self.focus = Focus::Functions;
                if index < self.functions.len() {
                    if double || index == self.functions_selected {
                        self.functions_selected = index;
                        self.jump_to_selected_function();
                    } else {
                        self.functions_selected = index;
                    }
                }
                Ok(true)
            }
            RegionKind::Preview => {
                self.focus = Focus::Preview;
                Ok(true)
            }
            RegionKind::Revisions => {
                self.focus = Focus::Revisions;
                if index < self.checkouts.len() {
                    self.revisions_selected = index;
                    if double {
                        self.dispatch_diff_against_selected_revision()?;
                        self.diff_return_mode = ViewMode::Browse;
                        self.mode = ViewMode::DetailDiff;
                    }
                }
                Ok(true)
            }
            RegionKind::DetailBody => self.handle_detail_body_click(index),
            // The header/search are handled above; the diff body has no
            // click action (scroll only).
            RegionKind::DetailDiffBody => Ok(false),
        }
    }

    /// Handle a click on line `line` of the detail-view body: copy the path
    /// field, or select/open a Folder entry.
    pub(super) fn handle_detail_body_click(&mut self, line: usize) -> Result<bool> {
        match detail::detail_click_at(self, line) {
            detail::DetailClick::CopyPath => {
                self.copy_selected_path();
                Ok(true)
            }
            detail::DetailClick::FolderEntry(index) => {
                self.folder_focused = true;
                if index == self.siblings_selected {
                    self.open_selected_folder_entry()?;
                } else {
                    self.siblings_selected = index;
                }
                Ok(true)
            }
            detail::DetailClick::None => Ok(false),
        }
    }

    pub(super) fn handle_scroll(&mut self, col: u16, row: u16, delta: i16) -> Result<bool> {
        let Some((kind, _)) = hit_test(&self.click_regions, col, row) else {
            return Ok(false);
        };
        match kind {
            RegionKind::Results => {
                let next = move_selection(self.selected, self.results.len(), delta as isize);
                if next != self.selected {
                    self.selected = next;
                    self.load_selected()?;
                    return Ok(true);
                }
                Ok(false)
            }
            RegionKind::Preview => {
                self.preview_scroll = scroll_by(self.preview_scroll, delta);
                Ok(true)
            }
            RegionKind::Revisions => {
                let previous = self.revisions_selected;
                self.move_revision(delta as isize);
                if previous != self.revisions_selected {
                    return Ok(true);
                }
                Ok(false)
            }
            RegionKind::DetailBody => {
                self.detail_scroll = scroll_by(self.detail_scroll, delta);
                Ok(true)
            }
            RegionKind::DetailDiffBody => {
                self.detail_diff_scroll = scroll_by(self.detail_diff_scroll, delta);
                Ok(true)
            }
            RegionKind::Deps => {
                self.deps_selected = move_selection(
                    self.deps_selected,
                    self.dependency_target_count(),
                    delta as isize,
                );
                Ok(true)
            }
            RegionKind::Functions => {
                self.functions_selected = move_selection(
                    self.functions_selected,
                    self.functions.len(),
                    delta as isize,
                );
                Ok(true)
            }
            // No scroll behaviour for these single-purpose regions.
            RegionKind::Header | RegionKind::Search | RegionKind::Metadata => Ok(false),
        }
    }

    /// Copy the selected script's full logical path to the clipboard.
    pub(super) fn copy_selected_path(&mut self) {
        let Some(path) = self.selected_logical_path() else {
            return;
        };
        self.copy_text(&path, "path");
    }

    /// What F3 would copy in the current view, decided without building the
    /// text: the footer asks this every frame, and the detail view's full
    /// text can run to the whole indexed content.
    pub(super) fn copy_kind(&self) -> Option<CopyKind> {
        Some(match (self.mode, self.focus) {
            (ViewMode::Browse, Focus::Search) => CopyKind::Search,
            (ViewMode::Browse, Focus::Preview) if !self.detail_loading => CopyKind::Preview,
            (ViewMode::Browse, Focus::Preview) => return None,
            (ViewMode::DetailDiff, _) if !self.detail_diff_loading => CopyKind::Diff,
            (ViewMode::Detail, _) if !self.detail_loading => CopyKind::Detail,
            (ViewMode::Browse, _) if self.selected_logical_path().is_some() => CopyKind::Path,
            _ => return None,
        })
    }

    pub(super) fn copy_target(&self) -> Option<(String, &'static str)> {
        Some(match self.copy_kind()? {
            CopyKind::Search => {
                let text = self
                    .query_selection()
                    .map_or(self.query.as_str(), |range| &self.query[range]);
                (text.to_string(), "search")
            }
            CopyKind::Preview => (self.cached_preview.clone(), "catalog content"),
            CopyKind::Diff => (self.detail_diff_output.clone(), "diff"),
            CopyKind::Detail => (detail::detail_text(self), "script details"),
            CopyKind::Path => (self.selected_logical_path()?, "path"),
        })
    }

    pub(super) fn copy_focused_text(&mut self) {
        if let Some((text, label)) = self.copy_target() {
            self.copy_text(&text, label);
        } else {
            self.flash = Some("Nothing available to copy in this view yet".into());
        }
    }

    fn copy_text(&mut self, text: &str, label: &str) {
        if text.is_empty() {
            self.flash = Some("Nothing to copy".into());
            return;
        }
        // OSC 52 is a request; the terminal may reject it without replying.
        self.force_full_redraw = true;
        self.flash = Some(match clipboard::copy_to_clipboard(text) {
            Ok(()) => format!("Copy requested: {label} (terminal clipboard permission required)"),
            Err(err) => format!("Copy failed: {err}"),
        });
    }
}

/// Source of the text F3 copies; see [`TuiApp::copy_kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CopyKind {
    Search,
    Preview,
    Diff,
    Detail,
    Path,
}
