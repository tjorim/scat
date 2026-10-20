use super::{
    Focus, KeyCode, KeyEvent, KeyModifiers, Result, STATS_PAGE_COUNT, TuiApp, ViewMode,
    apply_scroll_key, move_selection, next_focus, previous_focus,
};

impl TuiApp {
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.workflow.is_some() {
            self.workflow_key(key);
            return Ok(false);
        }
        if !self.native_selection && matches!(key.code, KeyCode::F(11 | 12)) {
            self.open_workflow(key.code == KeyCode::F(11));
            return Ok(false);
        }
        self.query_dragging = false;
        if self.search_settings.is_some() {
            self.handle_settings_key(key);
            return Ok(false);
        }
        if key.code == KeyCode::F(6) {
            self.open_search_settings();
            return Ok(false);
        }
        if self.popup.is_some() {
            return self.handle_popup_key(key);
        }
        if matches!(key.code, KeyCode::F(1 | 4)) {
            self.popup = Some(super::actions::Popup::new(key.code == KeyCode::F(4)));
            return Ok(false);
        }
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            return Ok(true);
        }
        if key.code == KeyCode::F(2) {
            self.native_selection = !self.native_selection;
            self.query_dragging = false;
            return Ok(false);
        }
        if self.native_selection {
            if key.code == KeyCode::Esc {
                self.native_selection = false;
            }
            return Ok(false);
        }
        let kind = match key.code {
            KeyCode::F(7) => Some(super::inspection::Kind::Similar),
            KeyCode::F(8) => Some(super::inspection::Kind::Status),
            KeyCode::F(9) => Some(super::inspection::Kind::Tree),
            KeyCode::F(10) => Some(super::inspection::Kind::Symlinks),
            _ => None,
        };
        if let Some(kind) = kind {
            self.open_inspection(kind)?;
            return Ok(false);
        }
        if self.inspection.is_some() {
            self.inspection_key(key)?;
            return Ok(false);
        }
        if key.code == KeyCode::Esc
            && self.mode == ViewMode::Detail
            && self.inspection_return.is_some()
        {
            self.inspection = self.inspection_return.take();
            self.mode = self.inspection_return_mode;
            return Ok(false);
        }
        if key.code == KeyCode::F(3) {
            self.copy_focused_text();
            return Ok(false);
        }
        if self.mode == ViewMode::Browse
            && self.focus == Focus::Search
            && self.handle_search_key(key)
        {
            return Ok(false);
        }
        if self.mode == ViewMode::Detail {
            return self.handle_detail_key(key);
        }
        if self.mode == ViewMode::DetailDiff {
            return self.handle_detail_diff_key(key);
        }
        if self.mode == ViewMode::Stats {
            return self.handle_stats_key(key);
        }
        if self.mode == ViewMode::DepGraph {
            return self.handle_dep_graph_key(key);
        }

        match key {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if self.fullscreen {
                    self.fullscreen = false;
                } else {
                    return Ok(true);
                }
            }
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return Ok(true),
            KeyEvent {
                code: KeyCode::Char('/'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.focus != Focus::Search => {
                self.focus = Focus::Search;
            }
            KeyEvent {
                code: KeyCode::Char('l'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.focus = Focus::Results;
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                self.focus = next_focus(self.focus);
            }
            KeyEvent {
                code: KeyCode::BackTab,
                ..
            } => {
                self.focus = previous_focus(self.focus);
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } if self.focus == Focus::Search => {
                self.focus = Focus::Results;
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } if self.focus == Focus::Results => {
                self.mode = ViewMode::Detail;
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } if self.focus == Focus::Deps => {
                self.open_selected_dependency()?;
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } if self.focus == Focus::Functions => {
                self.jump_to_selected_function();
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } if self.focus == Focus::Revisions => {
                self.dispatch_diff_against_selected_revision()?;
                self.diff_return_mode = ViewMode::Browse;
                self.mode = ViewMode::DetailDiff;
            }
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                ..
            } if self.focus == Focus::Results => {
                let next = move_selection(self.selected, self.results.len(), -1);
                if next != self.selected {
                    self.selected = next;
                    self.load_selected()?;
                }
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                ..
            } if self.focus == Focus::Results => {
                let next = move_selection(self.selected, self.results.len(), 1);
                if next != self.selected {
                    self.selected = next;
                    self.load_selected()?;
                }
            }
            KeyEvent {
                code: KeyCode::Home,
                ..
            } if self.focus == Focus::Results => {
                self.selected = 0;
                self.load_selected()?;
            }
            KeyEvent {
                code: KeyCode::End, ..
            } if self.focus == Focus::Results && !self.results.is_empty() => {
                self.selected = self.results.len() - 1;
                self.load_selected()?;
            }
            KeyEvent {
                code: KeyCode::Char('g'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.focus == Focus::Results => {
                self.selected = 0;
                self.load_selected()?;
            }
            KeyEvent {
                code: KeyCode::Char('G'),
                ..
            } if self.focus == Focus::Results && !self.results.is_empty() => {
                self.selected = self.results.len() - 1;
                self.load_selected()?;
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } if self.focus == Focus::Deps => {
                if let Some(previous) = self.dep_backstack.pop() {
                    self.navigate_to_path(&previous)?;
                }
            }
            KeyEvent {
                code: KeyCode::Char('['),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.focus == Focus::Deps => {
                if let Some(previous) = self.dep_backstack.pop() {
                    self.navigate_to_path(&previous)?;
                }
            }
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                ..
            } if self.focus == Focus::Deps => {
                self.deps_selected =
                    move_selection(self.deps_selected, self.dependency_target_count(), -1);
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                ..
            } if self.focus == Focus::Deps => {
                self.deps_selected =
                    move_selection(self.deps_selected, self.dependency_target_count(), 1);
            }
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                ..
            } if self.focus == Focus::Functions => {
                self.functions_selected =
                    move_selection(self.functions_selected, self.functions.len(), -1);
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                ..
            } if self.focus == Focus::Functions => {
                self.functions_selected =
                    move_selection(self.functions_selected, self.functions.len(), 1);
            }
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                ..
            } if self.focus == Focus::Revisions => {
                self.move_revision(-1);
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                ..
            } if self.focus == Focus::Revisions => {
                self.move_revision(1);
            }
            KeyEvent {
                code: KeyCode::Char('f'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.focus != Focus::Search => {
                self.fullscreen = !self.fullscreen;
            }
            KeyEvent {
                code: KeyCode::Char('v'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.focus != Focus::Search => {
                self.queue_catalog_view();
            }
            KeyEvent {
                code: KeyCode::Char('V'),
                ..
            } if self.focus != Focus::Search => {
                self.queue_live_source_view();
            }
            KeyEvent {
                code: KeyCode::Char('s'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.focus != Focus::Search => {
                self.dispatch_stats()?;
                self.mode = ViewMode::Stats;
            }
            KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::NONE,
                ..
            } if self.focus != Focus::Search => {
                self.mode = ViewMode::DepGraph;
            }
            _ => {
                self.apply_focused_scroll(key);
            }
        }
        Ok(false)
    }

    pub(super) fn handle_detail_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.folder_focused {
            return self.handle_folder_browse_key(key);
        }

        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return Ok(true),
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                self.mode = ViewMode::Browse;
                self.focus = Focus::Results;
            }
            KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.dispatch_diff()?;
                self.diff_return_mode = ViewMode::Detail;
                self.mode = ViewMode::DetailDiff;
            }
            KeyEvent {
                code: KeyCode::Char('v'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.queue_catalog_view();
            }
            KeyEvent {
                code: KeyCode::Char('V'),
                ..
            } => {
                self.queue_live_source_view();
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                if self.can_focus_folder() {
                    self.folder_focused = true;
                    self.siblings_selected = 0;
                }
            }
            _ => {
                apply_scroll_key(&mut self.detail_scroll, key);
            }
        }
        Ok(false)
    }

    /// Key handling for the Folder section's sibling-browse sub-mode, active
    /// while [`TuiApp::folder_focused`] is set. Up/Down move the highlighted
    /// sibling, Enter jumps to it, `[` browses up one folder level, and
    /// Backspace pops the folder backstack (or exits browse mode if empty).
    pub(super) fn handle_folder_browse_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return Ok(true),
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.folder_focused = false;
            }
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                ..
            } => {
                self.siblings_selected =
                    move_selection(self.siblings_selected, self.folder_entry_count(), -1);
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                ..
            } => {
                self.siblings_selected =
                    move_selection(self.siblings_selected, self.folder_entry_count(), 1);
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                self.open_selected_folder_entry()?;
            }
            KeyEvent {
                code: KeyCode::Char('['),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.folder_go_up()?;
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                if let Some(previous) = self.folder_backstack.pop() {
                    self.navigate_to_path(&previous)?;
                } else {
                    self.folder_focused = false;
                }
            }
            _ => {}
        }
        Ok(false)
    }

    // Matches the `Result<bool>` signature of its sibling `handle_*_key`
    // handlers so `handle_key` can dispatch to any of them uniformly, even
    // though this one never actually errors.
    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn handle_detail_diff_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return Ok(true),
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                self.mode = self.diff_return_mode;
            }
            _ => {
                apply_scroll_key(&mut self.detail_diff_scroll, key);
            }
        }
        Ok(false)
    }

    /// Key handling for the full-screen catalog stats view
    /// (`ViewMode::Stats`, opened with `s`). No scroll state: the bar charts
    /// are capped to a fixed top-N per chart (see
    /// `render::stats_view::MAX_BARS`) so the view always fits one screen.
    ///
    /// Matches the `Result<bool>` signature of its sibling `handle_*_key`
    /// handlers so `handle_key` can dispatch to any of them uniformly, even
    /// though this one never actually errors.
    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn handle_stats_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return Ok(true),
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                self.mode = ViewMode::Browse;
            }
            KeyEvent {
                code: KeyCode::Right,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('l'),
                ..
            } => {
                self.stats_page = (self.stats_page + 1) % STATS_PAGE_COUNT;
            }
            KeyEvent {
                code: KeyCode::Left,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('h'),
                ..
            } => {
                self.stats_page = (self.stats_page + STATS_PAGE_COUNT - 1) % STATS_PAGE_COUNT;
            }
            _ => {}
        }
        Ok(false)
    }

    /// Key handling for the full-screen dependency graph view
    /// (`ViewMode::DepGraph`, opened with `d`). Shares `deps_selected` and
    /// `open_selected_dependency`/`navigate_to_path` with the Deps pane, so
    /// selecting and opening a node here behaves exactly like the pane does.
    pub(super) fn handle_dep_graph_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return Ok(true),
            KeyEvent {
                code: KeyCode::Esc, ..
            }
            | KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                self.mode = ViewMode::Browse;
            }
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                ..
            } => {
                self.deps_selected =
                    move_selection(self.deps_selected, self.dependency_target_count(), -1);
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                ..
            } => {
                self.deps_selected =
                    move_selection(self.deps_selected, self.dependency_target_count(), 1);
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                self.open_selected_dependency()?;
            }
            KeyEvent {
                code: KeyCode::Char('['),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(previous) = self.dep_backstack.pop() {
                    self.navigate_to_path(&previous)?;
                }
            }
            _ => {}
        }
        Ok(false)
    }
}
