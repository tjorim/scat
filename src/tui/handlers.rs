use super::*;

impl TuiApp {
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.mode == ViewMode::Detail {
            return self.handle_detail_key(key);
        }
        if self.mode == ViewMode::DetailDiff {
            return self.handle_detail_diff_key(key);
        }

        match key {
            KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                ..
            } => return Ok(true),
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
                code: KeyCode::Backspace,
                ..
            } if self.focus == Focus::Search => {
                self.query.pop();
                self.schedule_query();
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
                code: KeyCode::Char(ch),
                modifiers,
                ..
            } if self.focus == Focus::Search
                && (modifiers.is_empty() || modifiers == KeyModifiers::SHIFT) =>
            {
                self.query.push(ch);
                self.schedule_query();
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
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                ..
            } => return Ok(true),
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
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                ..
            } => return Ok(true),
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

    pub(super) fn handle_detail_diff_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key {
            KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                ..
            } => return Ok(true),
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
                self.mode = ViewMode::Detail;
            }
            _ => {
                apply_scroll_key(&mut self.detail_diff_scroll, key);
            }
        }
        Ok(false)
    }
}
