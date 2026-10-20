//! Contextual actions shared by help, the palette and footer hints.
use super::{Focus, KeyCode, KeyEvent, KeyModifiers, Result, TuiApp, ViewMode};

pub(super) struct Popup {
    pub palette: bool,
    pub query: String,
    pub selected: usize,
}
impl Popup {
    pub fn new(palette: bool) -> Self {
        Self {
            palette,
            query: String::new(),
            selected: 0,
        }
    }
}
pub(super) struct Action {
    pub label: &'static str,
    pub shortcut: &'static str,
    pub key: KeyEvent,
    pub available: bool,
}
impl TuiApp {
    pub(super) fn actions(&self) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut add = |label, shortcut, code, available| {
            actions.push(Action {
                label,
                shortcut,
                key: KeyEvent::new(code, KeyModifiers::NONE),
                available,
            });
        };
        add(
            "Search mode, filters and result limit",
            "F6",
            KeyCode::F(6),
            true,
        );
        add(
            "Compare files, revisions or catalog snapshots",
            "F11",
            KeyCode::F(11),
            true,
        );
        add("Export JSON / search CSV", "F12", KeyCode::F(12), true);
        add("Similar scripts", "F7", KeyCode::F(7), true);
        add("Catalog checkout status", "F8", KeyCode::F(8), true);
        add("Transitive dependency trees", "F9", KeyCode::F(9), true);
        add("Symlink inspection", "F10", KeyCode::F(10), true);
        add("Contextual help", "F1", KeyCode::F(1), true);
        add("Command palette", "F4", KeyCode::F(4), true);
        add("Native text selection", "F2", KeyCode::F(2), true);
        add(
            "Copy focused content",
            "F3",
            KeyCode::F(3),
            self.copy_kind().is_some(),
        );
        if self.inspection.is_some() {
            add("Open inspected script", "Enter", KeyCode::Enter, true);
            add("Back to browsing", "Esc", KeyCode::Esc, true);
            add("Move up", "Up", KeyCode::Up, true);
            add("Move down", "Down", KeyCode::Down, true);
            add("Increase limit / depth", "+", KeyCode::Char('+'), true);
            add("Decrease limit / depth", "-", KeyCode::Char('-'), true);
            add("Reload inspection", "r", KeyCode::Char('r'), true);
            return actions;
        }
        let browse = self.mode == ViewMode::Browse;
        let input = browse && self.focus == Focus::Search;
        if browse {
            add("Search", "/", KeyCode::Char('/'), !input);
            add("Next pane", "Tab", KeyCode::Tab, true);
            add("Previous pane", "Shift+Tab", KeyCode::BackTab, true);
            add("Toggle fullscreen", "f", KeyCode::Char('f'), !input);
            add("Catalog statistics", "s", KeyCode::Char('s'), !input);
            add("Dependency graph", "d", KeyCode::Char('d'), !input);
            add(
                "Open selection",
                "Enter",
                KeyCode::Enter,
                match self.focus {
                    Focus::Search => true,
                    Focus::Results => !self.results.is_empty(),
                    Focus::Deps => self.dependency_target_count() > 0,
                    Focus::Functions => !self.functions.is_empty(),
                    Focus::Revisions => !self.checkouts.is_empty(),
                    Focus::Preview => false,
                },
            );
        }
        if browse || self.mode == ViewMode::Detail {
            add(
                "View catalog content",
                "v",
                KeyCode::Char('v'),
                !input && self.detail.is_some(),
            );
            add(
                "View live source",
                "V",
                KeyCode::Char('V'),
                !input && self.detail.is_some(),
            );
        }
        if self.mode == ViewMode::Detail {
            if self.folder_focused {
                add(
                    "Open folder entry",
                    "Enter",
                    KeyCode::Enter,
                    self.folder_entry_count() > 0,
                );
                add("Parent folder", "[", KeyCode::Char('['), true);
            } else {
                add(
                    "Diff against source",
                    "d",
                    KeyCode::Char('d'),
                    self.detail.is_some(),
                );
                add(
                    "Browse folder",
                    "Tab",
                    KeyCode::Tab,
                    self.can_focus_folder(),
                );
            }
        }
        if self.mode == ViewMode::DepGraph {
            add(
                "Open dependency",
                "Enter",
                KeyCode::Enter,
                self.dependency_target_count() > 0,
            );
        }
        if self.mode == ViewMode::Stats {
            add("Next statistics page", "Right", KeyCode::Right, true);
            add("Previous statistics page", "Left", KeyCode::Left, true);
        } else if !input {
            add("Move / scroll up", "Up", KeyCode::Up, true);
            add("Move / scroll down", "Down", KeyCode::Down, true);
            add(
                "Top",
                "Home",
                KeyCode::Home,
                !matches!(
                    self.focus,
                    Focus::Deps | Focus::Functions | Focus::Revisions
                ) && self.mode != ViewMode::DepGraph,
            );
            add(
                "Bottom",
                "End",
                KeyCode::End,
                !matches!(
                    self.focus,
                    Focus::Deps | Focus::Functions | Focus::Revisions
                ) && self.mode != ViewMode::DepGraph,
            );
        }
        if (browse && self.focus == Focus::Deps) || self.mode == ViewMode::DepGraph {
            add(
                "Previous dependency",
                "Backspace / [",
                KeyCode::Backspace,
                !self.dep_backstack.is_empty(),
            );
        }
        if self.mode == ViewMode::DetailDiff
            || (self.mode == ViewMode::Detail && !self.folder_focused)
            || (browse && self.focus == Focus::Preview)
        {
            add("Scroll page up", "PageUp / Ctrl+B", KeyCode::PageUp, true);
            add(
                "Scroll page down",
                "PageDown / Ctrl+F",
                KeyCode::PageDown,
                true,
            );
        }
        add("Back / exit", "Esc", KeyCode::Esc, true);
        for (label, shortcut, c) in [
            ("Catalog information", "Alt+i", 'i'),
            ("Host diagnostics (scat doctor)", "Alt+d", 'd'),
            ("Catalog audit findings and thresholds", "Alt+a", 'a'),
            ("Administration: build / rebuild catalog", "Alt+b", 'b'),
            ("Administration: vc passthrough", "Alt+v", 'v'),
        ] {
            actions.push(Action {
                label,
                shortcut,
                key: KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT),
                available: true,
            });
        }
        actions.push(Action {
            label: "Quit",
            shortcut: "Ctrl+C",
            key: KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            available: true,
        });
        actions
    }

    pub(super) fn filtered_actions(&self) -> Vec<Action> {
        let query = self
            .popup
            .as_ref()
            .map_or("", |p| p.query.as_str())
            .to_lowercase();
        self.actions()
            .into_iter()
            .filter(|a| {
                format!("{} {}", a.label, a.shortcut)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }

    pub(super) fn handle_popup_key(&mut self, key: KeyEvent) -> Result<bool> {
        let actions = self.filtered_actions();
        let popup = self.popup.as_mut().unwrap();
        match key.code {
            KeyCode::Esc => self.popup = None,
            KeyCode::Up => popup.selected = popup.selected.saturating_sub(1),
            KeyCode::Down => {
                popup.selected = (popup.selected + 1).min(actions.len().saturating_sub(1));
            }
            KeyCode::Enter if popup.palette => {
                if let Some(action) = actions.get(popup.selected).filter(|a| a.available) {
                    let key = action.key;
                    self.popup = None;
                    return self.handle_key(key);
                }
            }
            KeyCode::Backspace if popup.palette => {
                popup.query.pop();
                popup.selected = 0;
            }
            KeyCode::Char(c)
                if popup.palette
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                popup.query.push(c);
                popup.selected = 0;
            }
            _ => {}
        }
        Ok(false)
    }
}
