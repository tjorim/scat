use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::{Frame, Terminal};
use serde_json::Value;

use scat_core::core::db::{JsonRow, row_string as str_field};
use scat_core::core::resolve::PathResolver;
use scat_core::core::script_view::{ScriptView, logical_parent_dir};
use scat_core::core::vc::compare_revision_rows;

mod detail;
mod detail_worker;
mod diff_worker;
mod file_check_worker;
mod folder_worker;
mod render;
mod search_worker;
mod viewer;

use self::detail_worker::{
    DependencyItem, DetailPayload, DetailRequest, DetailResponse, DetailWorker, FunctionCallSite,
    FunctionItem,
};
use self::diff_worker::{DiffRequest, DiffResponse, DiffWorker};
use self::file_check_worker::{FileCheckRequest, FileCheckResponse, FileCheckWorker};
use self::folder_worker::{FolderRequest, FolderResponse, FolderWorker};
use self::render::draw;
use self::search_worker::{SearchRequest, SearchWorker};

const RESULT_LIMIT: usize = 200;
const PREVIEW_LINES: usize = 500;
const DEBOUNCE_MS: u64 = 150;
const POLL_TICK_MS: u64 = 50;
const PAGE_SCROLL_LINES: i16 = 40;
const HALF_PAGE_SCROLL_LINES: i16 = 20;
const FULL_PAGE_SCROLL_LINES: i16 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Search,
    Results,
    Preview,
    Deps,
    Functions,
    Revisions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Browse,
    Detail,
    DetailDiff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollCommand {
    Delta(i16),
    Top,
}

struct TuiApp {
    search_worker: SearchWorker,
    detail_worker: DetailWorker,
    diff_worker: DiffWorker,
    folder_worker: FolderWorker,
    resolver: PathResolver,
    query: String,
    results: Vec<JsonRow>,
    selected: usize,
    focus: Focus,
    detail: Option<JsonRow>,
    contributors: Vec<String>,
    deps: Vec<DependencyItem>,
    deps_selected: usize,
    functions: Vec<FunctionItem>,
    functions_selected: usize,
    function_call_sites: std::collections::BTreeMap<String, Vec<FunctionCallSite>>,
    function_xref: Option<String>,
    dep_backstack: Vec<String>,
    checkouts: Vec<JsonRow>,
    siblings: Vec<JsonRow>,
    /// Directory currently shown in the Folder section, when it differs from
    /// the selected script's own parent (set by "go up one folder level").
    /// `None` means "derive from the selected script's `parent_dir`".
    folder_dir: Option<String>,
    /// Whether the Folder section's sibling list is receiving Up/Down/Enter
    /// (toggled with Tab in the fullscreen detail view).
    folder_focused: bool,
    siblings_selected: usize,
    folder_backstack: Vec<String>,
    inflight_folder_id: Option<u64>,
    next_folder_id: u64,
    error: Option<String>,
    preview_scroll: u16,
    revisions_scroll: u16,
    detail_scroll: u16,
    mode: ViewMode,
    detail_diff_output: String,
    detail_diff_loading: bool,
    inflight_diff_id: Option<u64>,
    next_diff_id: u64,
    detail_diff_scroll: u16,
    results_state: ListState,
    cached_preview: String,
    preview_total_lines: usize,
    detail_loading: bool,
    inflight_detail_id: Option<u64>,
    next_detail_id: u64,
    last_keystroke_at: Option<Instant>,
    /// Active `lang:`/`owner:`/`tag:` filter labels for the current query,
    /// recomputed only when the query changes (not per render frame).
    filter_labels: Vec<String>,
    pending_query: Option<String>,
    inflight_query_id: Option<u64>,
    search_in_flight: bool,
    next_query_id: u64,
    fullscreen: bool,
    tick: u64,
    pending_view: Option<viewer::ViewTarget>,
    file_check_worker: FileCheckWorker,
    inflight_filecheck_id: Option<u64>,
    next_filecheck_id: u64,
}

impl TuiApp {
    fn new(
        search_worker: SearchWorker,
        detail_worker: DetailWorker,
        diff_worker: DiffWorker,
        folder_worker: FolderWorker,
        resolver: PathResolver,
    ) -> Result<Self> {
        let mut app = Self {
            search_worker,
            detail_worker,
            diff_worker,
            folder_worker,
            resolver,
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            focus: Focus::Search,
            detail: None,
            contributors: Vec::new(),
            deps: Vec::new(),
            deps_selected: 0,
            functions: Vec::new(),
            functions_selected: 0,
            function_call_sites: std::collections::BTreeMap::new(),
            function_xref: None,
            dep_backstack: Vec::new(),
            checkouts: Vec::new(),
            siblings: Vec::new(),
            folder_dir: None,
            folder_focused: false,
            siblings_selected: 0,
            folder_backstack: Vec::new(),
            inflight_folder_id: None,
            next_folder_id: 0,
            error: None,
            preview_scroll: 0,
            revisions_scroll: 0,
            detail_scroll: 0,
            mode: ViewMode::Browse,
            detail_diff_output: String::new(),
            detail_diff_loading: false,
            inflight_diff_id: None,
            next_diff_id: 0,
            detail_diff_scroll: 0,
            results_state: ListState::default(),
            cached_preview: String::new(),
            preview_total_lines: 0,
            detail_loading: false,
            inflight_detail_id: None,
            next_detail_id: 0,
            last_keystroke_at: None,
            filter_labels: Vec::new(),
            pending_query: None,
            inflight_query_id: None,
            search_in_flight: false,
            next_query_id: 0,
            fullscreen: false,
            tick: 0,
            pending_view: None,
            file_check_worker: FileCheckWorker::new()?,
            inflight_filecheck_id: None,
            next_filecheck_id: 0,
        };
        app.dispatch_query()?;
        Ok(app)
    }

    fn dispatch_query(&mut self) -> Result<()> {
        self.error = None;
        let id = self.next_query_id;
        self.next_query_id = self.next_query_id.saturating_add(1);
        self.search_worker.send(SearchRequest {
            id,
            query: self.query.clone(),
            limit: RESULT_LIMIT,
        })?;
        self.pending_query = None;
        self.inflight_query_id = Some(id);
        self.search_in_flight = true;
        Ok(())
    }

    fn apply_results(&mut self) -> Result<()> {
        while let Some(response) = self.search_worker.try_recv()? {
            if Some(response.id) != self.inflight_query_id {
                continue;
            }
            self.search_in_flight = false;
            self.inflight_query_id = None;
            match response.result {
                Ok(results) => {
                    self.error = None;
                    self.apply_search_results(results)?;
                }
                Err(err) => {
                    self.error = Some(err);
                }
            }
        }
        Ok(())
    }

    fn apply_search_results(&mut self, results: Vec<JsonRow>) -> Result<()> {
        self.error = None;
        self.results = results;
        self.results_state = ListState::default();
        if self.results.is_empty() {
            self.selected = 0;
            self.inflight_detail_id = None;
            self.detail_loading = false;
            self.detail = None;
            self.contributors.clear();
            self.deps.clear();
            self.deps_selected = 0;
            self.functions.clear();
            self.functions_selected = 0;
            self.function_call_sites.clear();
            self.function_xref = None;
            self.dep_backstack.clear();
            self.checkouts.clear();
            self.siblings.clear();
            self.folder_dir = None;
            self.folder_focused = false;
            self.siblings_selected = 0;
            self.folder_backstack.clear();
            return Ok(());
        }
        if self.selected >= self.results.len() {
            self.selected = self.results.len() - 1;
        }
        self.load_selected()
    }

    fn schedule_query(&mut self) {
        self.pending_query = Some(self.query.clone());
        self.last_keystroke_at = Some(Instant::now());
        // Recompute the filter labels once per query change so the render loop
        // can read them without re-parsing the query every frame.
        self.filter_labels = search_worker::parse_query_filters(&self.query).filter_labels();
    }

    fn load_selected(&mut self) -> Result<()> {
        self.results_state.select(if self.results.is_empty() {
            None
        } else {
            Some(self.selected)
        });

        let Some(path) = self
            .results
            .get(self.selected)
            .map(ScriptView::new)
            .and_then(|view| view.logical_path_value())
            .and_then(Value::as_str)
        else {
            self.inflight_detail_id = None;
            self.detail_loading = false;
            self.detail = None;
            self.contributors.clear();
            self.deps.clear();
            self.deps_selected = 0;
            self.functions.clear();
            self.functions_selected = 0;
            self.function_call_sites.clear();
            self.function_xref = None;
            self.checkouts.clear();
            self.siblings.clear();
            self.folder_dir = None;
            self.folder_focused = false;
            self.siblings_selected = 0;
            self.cached_preview.clear();
            self.preview_total_lines = 0;
            return Ok(());
        };

        let id = self.next_detail_id;
        self.next_detail_id = self.next_detail_id.saturating_add(1);
        self.detail_loading = true;
        self.error = None;
        self.preview_scroll = 0;
        self.revisions_scroll = 0;
        self.detail_scroll = 0;
        self.deps_selected = 0;
        self.functions_selected = 0;
        self.function_xref = None;
        self.folder_dir = None;
        self.folder_focused = false;
        self.siblings_selected = 0;
        self.inflight_detail_id = Some(id);
        self.detail_worker.send(DetailRequest {
            id,
            path: path.to_owned(),
        })?;

        Ok(())
    }

    fn drain_detail_channel(&mut self) {
        loop {
            match self.detail_worker.try_recv() {
                Ok(Some(response)) => self.apply_detail_response(response),
                Ok(None) => break,
                Err(_) => {
                    self.inflight_detail_id = None;
                    self.detail_loading = false;
                    self.error = Some("Detail worker disconnected unexpectedly".to_string());
                    break;
                }
            }
        }
    }

    fn apply_detail_response(&mut self, response: DetailResponse) {
        if Some(response.id) != self.inflight_detail_id {
            return;
        }
        self.inflight_detail_id = None;
        self.detail_loading = false;
        let mut payload = response.payload;
        sort_checkouts(&mut payload.checkouts);
        let DetailPayload {
            detail,
            contributors,
            deps,
            functions,
            function_call_sites,
            checkouts,
            siblings,
            cached_preview,
            preview_total_lines,
            error,
        } = payload;
        self.detail = detail;
        self.contributors = contributors;
        self.deps = deps;
        self.functions = functions;
        self.function_call_sites = function_call_sites;
        self.deps_selected = 0;
        self.functions_selected = 0;
        self.function_xref = None;
        self.checkouts = checkouts;
        self.siblings = siblings;
        self.cached_preview = cached_preview;
        self.preview_total_lines = preview_total_lines;
        if error.is_some() {
            self.error = error;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
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

    fn handle_detail_key(&mut self, key: KeyEvent) -> Result<bool> {
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
    fn handle_folder_browse_key(&mut self, key: KeyEvent) -> Result<bool> {
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
                    move_selection(self.siblings_selected, self.siblings.len(), -1);
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
                    move_selection(self.siblings_selected, self.siblings.len(), 1);
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                self.open_selected_sibling()?;
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

    fn handle_detail_diff_key(&mut self, key: KeyEvent) -> Result<bool> {
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

    fn dispatch_diff(&mut self) -> Result<()> {
        self.detail_diff_scroll = 0;
        let Some(logical_path) = self.selected_logical_path() else {
            self.detail_diff_output = "No script selected.".to_string();
            self.detail_diff_loading = false;
            return Ok(());
        };
        let id = self.next_diff_id;
        self.next_diff_id = self.next_diff_id.saturating_add(1);
        self.detail_diff_loading = true;
        self.detail_diff_output = String::new();
        self.inflight_diff_id = Some(id);
        self.diff_worker.send(DiffRequest {
            id,
            path: logical_path,
        })?;
        Ok(())
    }

    fn drain_diff_channel(&mut self) {
        loop {
            match self.diff_worker.try_recv() {
                Ok(Some(response)) => self.apply_diff_response(response),
                Ok(None) => break,
                Err(_) => {
                    self.inflight_diff_id = None;
                    self.detail_diff_loading = false;
                    self.detail_diff_output = "Diff worker disconnected unexpectedly.".to_string();
                    break;
                }
            }
        }
    }

    fn apply_diff_response(&mut self, response: DiffResponse) {
        if Some(response.id) != self.inflight_diff_id {
            return;
        }
        self.inflight_diff_id = None;
        self.detail_diff_loading = false;
        self.detail_diff_output = response.output;
    }

    fn drain_folder_channel(&mut self) {
        loop {
            match self.folder_worker.try_recv() {
                Ok(Some(response)) => self.apply_folder_response(response),
                Ok(None) => break,
                Err(_) => {
                    self.inflight_folder_id = None;
                    self.error = Some("Folder worker disconnected unexpectedly".to_string());
                    break;
                }
            }
        }
    }

    fn apply_folder_response(&mut self, response: FolderResponse) {
        if Some(response.id) != self.inflight_folder_id {
            return;
        }
        self.inflight_folder_id = None;
        match response.result {
            Ok(entries) => {
                self.error = None;
                self.folder_dir = Some(response.dir);
                self.siblings = entries;
                self.siblings_selected = 0;
            }
            Err(err) => self.error = Some(err),
        }
    }

    /// The Folder section's currently displayed directory: the browsed
    /// override when set, otherwise the selected script's own parent.
    fn folder_display_dir(&self) -> String {
        self.folder_dir.clone().unwrap_or_else(|| {
            self.detail
                .as_ref()
                .map(|row| ScriptView::new(row).parent_dir().to_string())
                .unwrap_or_default()
        })
    }

    /// Whether the Folder section has anything to browse (Tab only takes
    /// effect when the selected script has a parent directory).
    fn can_focus_folder(&self) -> bool {
        !self.folder_display_dir().is_empty()
    }

    fn dispatch_folder_request(&mut self, dir: String) -> Result<()> {
        let id = self.next_folder_id;
        self.next_folder_id = self.next_folder_id.saturating_add(1);
        self.inflight_folder_id = Some(id);
        self.folder_worker.send(FolderRequest { id, dir })?;
        Ok(())
    }

    /// Browse up to the parent of the currently displayed folder. A no-op at
    /// the root (`/`), which has no parent to browse into.
    fn folder_go_up(&mut self) -> Result<()> {
        let current = self.folder_display_dir();
        if current.is_empty() || current == "/" {
            return Ok(());
        }
        let up_dir = logical_parent_dir(&current).to_string();
        self.dispatch_folder_request(up_dir)
    }

    /// Jump the detail view to the currently highlighted sibling, pushing the
    /// current script onto the folder backstack so it can be revisited.
    fn open_selected_sibling(&mut self) -> Result<()> {
        let Some(target) = self
            .siblings
            .get(self.siblings_selected)
            .map(|row| ScriptView::new(row).logical_path().to_string())
            .filter(|path| !path.is_empty())
        else {
            return Ok(());
        };
        if let Some(current_path) = self.selected_logical_path() {
            self.folder_backstack.push(current_path);
        }
        self.folder_focused = false;
        self.navigate_to_path(&target)
    }

    fn xref_call_sites(&self) -> Option<&[FunctionCallSite]> {
        self.function_xref
            .as_ref()
            .and_then(|function_name| self.function_call_sites.get(function_name))
            .map(Vec::as_slice)
            .filter(|sites| !sites.is_empty())
    }

    fn dependency_target_count(&self) -> usize {
        self.xref_call_sites()
            .map_or_else(|| self.deps.len(), |sites| sites.len())
    }

    fn open_selected_dependency(&mut self) -> Result<()> {
        let target = if let Some(call_sites) = self.xref_call_sites() {
            call_sites
                .get(self.deps_selected)
                .map(|site| site.caller_path.clone())
        } else {
            self.deps
                .get(self.deps_selected)
                .map(|item| item.logical_path.clone())
        };

        if let Some(target) = target
            && let Some(current_path) = self.selected_logical_path()
        {
            self.dep_backstack.push(current_path);
            self.navigate_to_path(&target)?;
        }
        Ok(())
    }

    fn jump_to_selected_function(&mut self) {
        if let Some(function) = self.functions.get(self.functions_selected) {
            self.preview_scroll = function.line.saturating_sub(1);
            self.function_xref = Some(function.name.clone());
            self.deps_selected = 0;
            self.focus = Focus::Preview;
        }
    }

    fn navigate_to_path(&mut self, logical_path: &str) -> Result<()> {
        let target_index = self.results.iter().position(|row| {
            ScriptView::new(row)
                .logical_path_value()
                .and_then(Value::as_str)
                .is_some_and(|value| value == logical_path)
        });

        let selected = if let Some(index) = target_index {
            index
        } else {
            let mut row = JsonRow::new();
            row.insert(
                "logical_path".to_string(),
                Value::String(logical_path.to_string()),
            );
            self.results.push(row);
            self.results.len().saturating_sub(1)
        };
        self.selected = selected;
        self.load_selected()
    }

    fn selected_logical_path(&self) -> Option<String> {
        self.detail
            .as_ref()
            .map(ScriptView::new)
            .and_then(|view| view.logical_path_value())
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.results
                    .get(self.selected)
                    .map(ScriptView::new)
                    .and_then(|view| view.logical_path_value())
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
            })
    }

    fn queue_catalog_view(&mut self) {
        match self.catalog_view_target() {
            Ok(target) => {
                self.error = None;
                self.pending_view = Some(viewer::ViewTarget::Catalog(target));
            }
            Err(err) => {
                self.error = Some(err.to_string());
            }
        }
    }

    fn queue_live_source_view(&mut self) {
        if let Err(err) = self.dispatch_live_source_check() {
            self.error = Some(err.to_string());
        }
    }

    /// Validate the selection and resolve the native path, then hand the
    /// (potentially blocking) file-existence check to the background worker.
    /// The view target or a clear error is produced from the worker response in
    /// [`Self::apply_file_check_response`].
    fn dispatch_live_source_check(&mut self) -> Result<()> {
        if self.detail_loading {
            anyhow::bail!("Script is still loading.");
        }
        let Some(row) = self.detail.as_ref() else {
            anyhow::bail!("No script selected.");
        };
        let logical_path = ScriptView::new(row).logical_path().to_string();
        if logical_path.is_empty() {
            anyhow::bail!("Selected script has no logical path.");
        }
        let native = self.resolver.to_native(&logical_path);
        let mapped = native != logical_path;
        let native_path = PathBuf::from(native);

        let id = self.next_filecheck_id;
        self.next_filecheck_id = self.next_filecheck_id.saturating_add(1);
        self.inflight_filecheck_id = Some(id);
        self.error = None;
        self.file_check_worker.send(FileCheckRequest {
            id,
            logical_path,
            native_path,
            mapped,
        })
    }

    fn drain_file_check_channel(&mut self) {
        loop {
            match self.file_check_worker.try_recv() {
                Ok(Some(response)) => self.apply_file_check_response(response),
                Ok(None) => break,
                Err(_) => {
                    self.inflight_filecheck_id = None;
                    self.error = Some("File-check worker disconnected unexpectedly.".to_string());
                    break;
                }
            }
        }
    }

    fn apply_file_check_response(&mut self, response: FileCheckResponse) {
        if Some(response.id) != self.inflight_filecheck_id {
            return;
        }
        self.inflight_filecheck_id = None;
        if response.exists {
            // The file may exist at the logical path itself (e.g. running scat
            // on the host where the catalog was built, where logical paths are
            // real filesystem paths), so a missing mapping is not an error here.
            self.error = None;
            self.pending_view = Some(viewer::ViewTarget::LiveSource {
                logical_path: response.logical_path,
                native_path: response.native_path,
            });
        } else if response.mapped {
            self.error = Some(format!(
                "Live source not found at {}",
                response.native_path.display()
            ));
        } else {
            self.error = Some(format!(
                "No filesystem mapping for {}, and no file exists at that path; configure a path mapping to open the live source.",
                response.logical_path
            ));
        }
    }

    fn catalog_view_target(&self) -> Result<viewer::CatalogView> {
        if self.detail_loading {
            anyhow::bail!("Script is still loading.");
        }
        let Some(row) = self.detail.as_ref() else {
            anyhow::bail!("No script selected.");
        };
        let view = ScriptView::new(row);
        let logical_path = view.logical_path().to_string();
        if logical_path.is_empty() {
            anyhow::bail!("Selected script has no logical path.");
        }
        Ok(viewer::CatalogView {
            logical_path,
            content: view.content().to_string(),
        })
    }

    fn scroll_target(&mut self) -> Option<&mut u16> {
        match self.focus {
            Focus::Preview => Some(&mut self.preview_scroll),
            Focus::Revisions => Some(&mut self.revisions_scroll),
            _ => None,
        }
    }

    fn apply_focused_scroll(&mut self, key: KeyEvent) -> bool {
        let Some(scroll) = self.scroll_target() else {
            return false;
        };
        apply_scroll_key(scroll, key)
    }
}

pub fn run(db_path: &Path, resolver: PathResolver) -> Result<()> {
    let search_worker = SearchWorker::new(db_path)?;
    let detail_worker = DetailWorker::new(db_path)?;
    let diff_worker = DiffWorker::new(db_path)?;
    let folder_worker = FolderWorker::new(db_path)?;
    let mut app = TuiApp::new(
        search_worker,
        detail_worker,
        diff_worker,
        folder_worker,
        resolver,
    )?;
    let mut terminal = TerminalGuard::enter()?;
    let debounce = Duration::from_millis(DEBOUNCE_MS);
    let poll_tick = Duration::from_millis(POLL_TICK_MS);

    loop {
        app.apply_results()?;
        app.drain_detail_channel();
        app.drain_diff_channel();
        app.drain_file_check_channel();
        app.drain_folder_channel();
        if app
            .last_keystroke_at
            .is_some_and(|keystroke| keystroke.elapsed() >= debounce)
            && app.pending_query.is_some()
        {
            app.last_keystroke_at = None;
            app.dispatch_query()?;
        }
        // A view target may be queued synchronously (catalog content) or
        // asynchronously once the file-check worker confirms the live source,
        // so open it from the loop body rather than only after a keystroke.
        if let Some(target) = app.pending_view.take()
            && let Err(err) = terminal.open_view(&target)
        {
            app.error = Some(format!("Failed to open viewer: {err}"));
        }
        terminal.draw(|frame| draw(frame, &mut app))?;
        app.tick = app.tick.wrapping_add(1);
        if event::poll(poll_tick)?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if app.handle_key(key)? {
                break;
            }
        }
    }
    Ok(())
}

fn sort_checkouts(checkouts: &mut [JsonRow]) {
    checkouts.sort_by(compare_revision_rows);
}

fn search_title(has_error: bool, is_searching: bool) -> &'static str {
    if has_error {
        "Search (invalid query)"
    } else if is_searching {
        "Search (searching…)"
    } else {
        "Search"
    }
}

fn next_focus(focus: Focus) -> Focus {
    match focus {
        Focus::Search => Focus::Results,
        Focus::Results => Focus::Preview,
        Focus::Preview => Focus::Deps,
        Focus::Deps => Focus::Functions,
        Focus::Functions => Focus::Revisions,
        Focus::Revisions => Focus::Search,
    }
}

fn previous_focus(focus: Focus) -> Focus {
    match focus {
        Focus::Search => Focus::Revisions,
        Focus::Results => Focus::Search,
        Focus::Preview => Focus::Results,
        Focus::Deps => Focus::Preview,
        Focus::Functions => Focus::Deps,
        Focus::Revisions => Focus::Functions,
    }
}

fn move_selection(selected: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len - 1;
    selected.saturating_add_signed(delta).min(max)
}

fn scroll_by(current: u16, delta: i16) -> u16 {
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

fn apply_scroll_key(scroll: &mut u16, key: KeyEvent) -> bool {
    let Some(command) = scroll_command(key) else {
        return false;
    };
    apply_scroll_command(scroll, command);
    true
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    fn draw<F>(&mut self, f: F) -> io::Result<ratatui::CompletedFrame<'_>>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.terminal.draw(f)
    }

    fn open_view(&mut self, target: &viewer::ViewTarget) -> Result<()> {
        self.suspend_for(|| viewer::open_target(target))
    }

    fn suspend_for<F>(&mut self, action: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        self.suspend()?;
        let action_result = action();
        let resume_result = self.resume();

        match (action_result, resume_result) {
            (Err(err), _) => Err(err),
            (Ok(()), Err(err)) => Err(err.into()),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn suspend(&mut self) -> io::Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        Ok(())
    }

    fn resume(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(self.terminal.backend_mut(), EnterAlternateScreen)?;
        self.terminal.clear()?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
fn make_test_db() -> tempfile::NamedTempFile {
    use scat_core::core::db::{SCHEMA_VERSION, create_db};
    let db = tempfile::NamedTempFile::new().unwrap();
    let conn = create_db(db.path()).unwrap();
    conn.execute(
        "INSERT INTO index_metadata (id, build_timestamp, schema_version) VALUES (1, '2024-01-01T00:00:00', ?)",
        rusqlite::params![SCHEMA_VERSION],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO scripts (logical_path, language, content, owner, purpose) VALUES ('/catalog/scripts/a.py','python','print(1)','alice','')",
        [],
    )
    .unwrap();
    drop(conn);
    db
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::detail::native_path_for_row;
    use super::{
        DetailPayload, DetailResponse, DetailWorker, DiffWorker, Focus, FolderWorker, SearchWorker,
        TuiApp, ViewMode, move_selection, next_focus, previous_focus, scroll_by, search_title,
        viewer,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use scat_core::core::resolve::PathResolver;
    use scat_core::core::script_view::ScriptView;
    use serde_json::{Map, Value};

    #[test]
    fn focus_cycles_forward_and_backward() {
        assert_eq!(next_focus(Focus::Search), Focus::Results);
        assert_eq!(next_focus(Focus::Results), Focus::Preview);
        assert_eq!(next_focus(Focus::Preview), Focus::Deps);
        assert_eq!(next_focus(Focus::Deps), Focus::Functions);
        assert_eq!(next_focus(Focus::Functions), Focus::Revisions);
        assert_eq!(next_focus(Focus::Revisions), Focus::Search);

        assert_eq!(previous_focus(Focus::Search), Focus::Revisions);
        assert_eq!(previous_focus(Focus::Revisions), Focus::Functions);
        assert_eq!(previous_focus(Focus::Functions), Focus::Deps);
        assert_eq!(previous_focus(Focus::Deps), Focus::Preview);
        assert_eq!(previous_focus(Focus::Preview), Focus::Results);
        assert_eq!(previous_focus(Focus::Results), Focus::Search);
    }

    #[test]
    fn selection_movement_stays_in_bounds() {
        assert_eq!(move_selection(0, 0, 1), 0);
        assert_eq!(move_selection(0, 3, -1), 0);
        assert_eq!(move_selection(0, 3, 1), 1);
        assert_eq!(move_selection(2, 3, 1), 2);
        assert_eq!(move_selection(2, 3, -2), 0);
    }

    #[test]
    fn scroll_movement_saturates_at_zero() {
        assert_eq!(scroll_by(0, -1), 0);
        assert_eq!(scroll_by(5, -2), 3);
        assert_eq!(scroll_by(5, 10), 15);
    }

    #[test]
    fn native_path_uses_mapping_when_available() {
        let mut row = Map::new();
        row.insert(
            "logical_path".to_string(),
            Value::String("/catalog/scripts/tools/foo.py".to_string()),
        );
        let mut file = tempfile::Builder::new().suffix(".yml").tempfile().unwrap();
        writeln!(
            file,
            "mappings:\n  - logical_prefix: /catalog/scripts\n    windows: \"Z:\\\\scripts\"\n    linux: /net/scripts"
        )
        .unwrap();
        let resolver = PathResolver::from_file(file.path()).unwrap();
        let expected = resolver.to_native("/catalog/scripts/tools/foo.py");
        assert_eq!(native_path_for_row(&row, &resolver), Some(expected));
    }

    #[test]
    fn native_path_omits_identity_mapping() {
        let mut row = Map::new();
        row.insert(
            "logical_path".to_string(),
            Value::String("/catalog/scripts/tools/foo.py".to_string()),
        );
        let resolver = PathResolver::new();
        assert_eq!(native_path_for_row(&row, &resolver), None);
    }

    fn detail_row(logical_path: &str) -> Map<String, Value> {
        let mut row = Map::new();
        row.insert(
            "logical_path".to_string(),
            Value::String(logical_path.to_string()),
        );
        row
    }

    /// Build a `PathResolver` mapping `/catalog/scripts` onto `root` for the
    /// current platform (the other platform's field is a dummy value).
    fn mapping_resolver(root: &std::path::Path) -> PathResolver {
        let root = root.display().to_string();
        let mut file = tempfile::Builder::new().suffix(".yml").tempfile().unwrap();
        if cfg!(windows) {
            let root_escaped = root.replace('\\', "\\\\");
            writeln!(
                file,
                "mappings:\n  - logical_prefix: /catalog/scripts\n    windows: \"{root_escaped}\"\n    linux: /unused"
            )
            .unwrap();
        } else {
            writeln!(
                file,
                "mappings:\n  - logical_prefix: /catalog/scripts\n    linux: \"{root}\"\n    windows: \"Z:\\\\unused\""
            )
            .unwrap();
        }
        PathResolver::from_file(file.path()).unwrap()
    }

    /// Pump the file-check worker until the in-flight request resolves.
    fn drain_until_file_check(app: &mut TuiApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.inflight_filecheck_id.is_some() {
            app.drain_file_check_channel();
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for file-check worker response"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn live_source_view_resolves_mapped_path() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("foo.py");
        std::fs::write(&script, "print(1)\n").unwrap();

        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.resolver = mapping_resolver(dir.path());
        app.detail = Some(detail_row("/catalog/scripts/foo.py"));
        app.detail_loading = false;

        app.queue_live_source_view();
        drain_until_file_check(&mut app);

        let target = app.pending_view.take().expect("live source queued");
        match target {
            viewer::ViewTarget::LiveSource {
                logical_path,
                native_path,
            } => {
                assert_eq!(logical_path, "/catalog/scripts/foo.py");
                assert_eq!(native_path, script);
            }
            other => panic!("expected live source target, got {other:?}"),
        }
        assert!(app.error.is_none());
    }

    #[test]
    fn live_source_view_opens_logical_path_without_mapping_when_file_exists() {
        // No mapping configured, but the logical path is itself a real file on
        // disk (the catalog-build-host scenario): it should open directly.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("foo.py");
        std::fs::write(&script, "print(1)\n").unwrap();
        let logical = script.display().to_string();

        let db = super::make_test_db();
        let mut app = make_app(db.path()); // identity resolver
        app.detail = Some(detail_row(&logical));
        app.detail_loading = false;

        app.queue_live_source_view();
        drain_until_file_check(&mut app);

        let target = app
            .pending_view
            .take()
            .expect("existing file opens without mapping");
        match target {
            viewer::ViewTarget::LiveSource {
                logical_path,
                native_path,
            } => {
                assert_eq!(logical_path, logical);
                assert_eq!(native_path, script);
            }
            other => panic!("expected live source target, got {other:?}"),
        }
    }

    #[test]
    fn live_source_view_errors_without_mapping() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        // Identity resolver: no mapping resolves the logical path to disk.
        app.detail = Some(detail_row("/catalog/scripts/foo.py"));
        app.detail_loading = false;

        app.queue_live_source_view();
        drain_until_file_check(&mut app);

        assert!(app.pending_view.is_none());
        let err = app.error.as_deref().expect("error set");
        assert!(
            err.contains("No filesystem mapping"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn live_source_view_errors_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        // Note: no file is created at the resolved path.
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.resolver = mapping_resolver(dir.path());
        app.detail = Some(detail_row("/catalog/scripts/missing.py"));
        app.detail_loading = false;

        app.queue_live_source_view();
        drain_until_file_check(&mut app);

        assert!(app.pending_view.is_none());
        let err = app.error.as_deref().expect("error set");
        assert!(
            err.contains("Live source not found"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn search_title_prioritizes_error_then_searching() {
        assert_eq!(search_title(false, false), "Search");
        assert_eq!(search_title(false, true), "Search (searching…)");
        assert_eq!(search_title(true, true), "Search (invalid query)");
    }

    fn make_app(db_path: &std::path::Path) -> TuiApp {
        let search_worker = SearchWorker::new(db_path).unwrap();
        let detail_worker = DetailWorker::new(db_path).unwrap();
        let diff_worker = DiffWorker::new(db_path).unwrap();
        let folder_worker = FolderWorker::new(db_path).unwrap();
        TuiApp::new(
            search_worker,
            detail_worker,
            diff_worker,
            folder_worker,
            PathResolver::new(),
        )
        .unwrap()
    }

    fn drain_until_folder_loaded(app: &mut TuiApp, previous_dir: Option<&str>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            app.drain_folder_channel();
            if app.folder_dir.as_deref() != previous_dir {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for folder worker"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn drain_until_diff_loaded(app: &mut TuiApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            app.drain_diff_channel();
            if !app.detail_diff_loading {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for diff worker"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn stale_detail_response_is_ignored() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.inflight_detail_id = Some(99);
        app.detail_loading = true;
        app.detail = None;

        app.apply_detail_response(DetailResponse {
            id: 98,
            payload: DetailPayload {
                detail: Some(Map::new()),
                contributors: vec![],
                deps: vec![],
                functions: vec![],
                function_call_sites: std::collections::BTreeMap::new(),
                checkouts: vec![],
                siblings: vec![],
                cached_preview: "x".to_string(),
                preview_total_lines: 0,
                error: None,
            },
        });

        assert_eq!(app.inflight_detail_id, Some(99));
        assert!(app.detail_loading);
        assert!(app.detail.is_none());
        assert!(app.deps.is_empty());
        assert!(app.functions.is_empty());
        assert!(app.cached_preview.is_empty());
    }

    #[test]
    fn detail_response_sorts_checkouts_once_on_load() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.inflight_detail_id = Some(1);
        app.detail_loading = true;

        let checkout_row = |os: &str, user: &str, timestamp: &str| {
            let mut row = Map::new();
            row.insert("os_flavor".to_string(), Value::String(os.to_string()));
            row.insert("user".to_string(), Value::String(user.to_string()));
            row.insert(
                "timestamp".to_string(),
                Value::String(timestamp.to_string()),
            );
            row
        };

        app.apply_detail_response(DetailResponse {
            id: 1,
            payload: DetailPayload {
                detail: Some(Map::new()),
                contributors: vec![],
                deps: vec![],
                functions: vec![],
                function_call_sites: std::collections::BTreeMap::new(),
                checkouts: vec![
                    checkout_row("ZOS", "alice", "20240101_1000"),
                    checkout_row("LINUX", "bob", "20240101_0900"),
                    checkout_row("LINUX", "jdoe", "20240102_0900"),
                ],
                siblings: vec![],
                cached_preview: String::new(),
                preview_total_lines: 0,
                error: None,
            },
        });

        let ordered_users = app
            .checkouts
            .iter()
            .map(|row| row.get("user").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>();
        assert_eq!(ordered_users, vec!["jdoe", "bob", "alice"]);
    }

    #[test]
    fn detail_diff_key_shows_no_checkout_message_without_crashing() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.mode = ViewMode::Detail;
        app.detail = Some(
            serde_json::json!({ "logical_path": "/catalog/scripts/a.py" })
                .as_object()
                .unwrap()
                .clone(),
        );

        let should_quit = app
            .handle_detail_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
            .unwrap();

        assert!(!should_quit);
        assert_eq!(app.mode, ViewMode::DetailDiff);
        drain_until_diff_loaded(&mut app);
        assert!(app.detail_diff_output.contains("No vc checkouts found"));
    }

    #[test]
    fn detail_diff_key_renders_diff_output_when_checkout_exists() {
        let db = super::make_test_db();
        let checkout = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(checkout.path(), "print(2)\n").unwrap();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "INSERT INTO revisions
             (logical_path, physical_path, revision_type, os_flavor, user, timestamp, age_seconds)
             VALUES (?1, ?2, 'DEVELOP', 'linux', 'alice', '20240101_1200', 10.0)",
            rusqlite::params![
                "/catalog/scripts/a.py",
                checkout.path().display().to_string()
            ],
        )
        .unwrap();
        drop(conn);

        let mut app = make_app(db.path());
        app.mode = ViewMode::Detail;
        app.detail = Some(
            serde_json::json!({ "logical_path": "/catalog/scripts/a.py" })
                .as_object()
                .unwrap()
                .clone(),
        );

        app.handle_detail_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.mode, ViewMode::DetailDiff);
        drain_until_diff_loaded(&mut app);
        assert!(
            app.detail_diff_output
                .contains("--- catalog:/catalog/scripts/a.py")
        );
        assert!(app.detail_diff_output.contains("+++"));
    }

    #[test]
    fn diff_view_escape_returns_to_detail_view() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.mode = ViewMode::DetailDiff;

        let should_quit = app
            .handle_detail_diff_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();

        assert!(!should_quit);
        assert_eq!(app.mode, ViewMode::Detail);
    }

    #[test]
    fn revisions_pane_scrolls_independently() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.focus = Focus::Revisions;

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.revisions_scroll, 1);

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.revisions_scroll, 2);

        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.revisions_scroll, 0);
    }

    #[test]
    fn revisions_pane_tab_navigation() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());

        app.focus = Focus::Deps;
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focus, Focus::Functions);

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focus, Focus::Revisions);

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focus, Focus::Search);

        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focus, Focus::Revisions);

        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focus, Focus::Functions);

        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focus, Focus::Deps);
    }

    #[test]
    fn deps_enter_navigates_and_backspace_returns() {
        let db = super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose)
             VALUES ('/catalog/scripts/b.py','python','def b():\\n    pass\\n','bob','')",
            [],
        )
        .unwrap();
        let source_id: i64 = conn
            .query_row(
                "SELECT id FROM scripts WHERE logical_path = '/catalog/scripts/a.py'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let target_id: i64 = conn
            .query_row(
                "SELECT id FROM scripts WHERE logical_path = '/catalog/scripts/b.py'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO dependencies (script_id, depends_on_path, resolved_script_id)
             VALUES (?1, '/catalog/scripts/b.py', ?2)",
            rusqlite::params![source_id, target_id],
        )
        .unwrap();
        drop(conn);

        let mut app = make_app(db.path());
        app.results = vec![
            serde_json::json!({ "logical_path": "/catalog/scripts/a.py" })
                .as_object()
                .unwrap()
                .clone(),
        ];
        app.selected = 0;
        app.load_selected().unwrap();
        let detail_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            app.drain_detail_channel();
            if !app.detail_loading {
                break;
            }
            assert!(std::time::Instant::now() < detail_deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        app.focus = Focus::Deps;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        let detail_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            app.drain_detail_channel();
            if !app.detail_loading {
                break;
            }
            assert!(std::time::Instant::now() < detail_deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            app.selected_logical_path().as_deref(),
            Some("/catalog/scripts/b.py")
        );

        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        let detail_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            app.drain_detail_channel();
            if !app.detail_loading {
                break;
            }
            assert!(std::time::Instant::now() < detail_deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(app.focus, Focus::Deps);
        assert_eq!(
            app.selected_logical_path().as_deref(),
            Some("/catalog/scripts/a.py")
        );
    }

    fn drain_until_detail_loaded(app: &mut TuiApp) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            app.drain_detail_channel();
            if !app.detail_loading {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for detail worker"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn folder_tab_toggles_browse_focus() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.results = vec![
            serde_json::json!({ "logical_path": "/catalog/scripts/a.py" })
                .as_object()
                .unwrap()
                .clone(),
        ];
        app.selected = 0;
        app.load_selected().unwrap();
        drain_until_detail_loaded(&mut app);

        app.mode = ViewMode::Detail;
        assert!(!app.folder_focused);

        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        assert!(app.folder_focused);

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(!app.folder_focused);
        // Esc while folder-focused only exits browse mode, not the detail view.
        assert_eq!(app.mode, ViewMode::Detail);
    }

    #[test]
    fn folder_enter_jumps_to_selected_sibling_and_backspace_returns() {
        let db = super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose)
             VALUES ('/catalog/scripts/b.py','python','print(2)\\n','bob','')",
            [],
        )
        .unwrap();
        drop(conn);

        let mut app = make_app(db.path());
        app.results = vec![
            serde_json::json!({ "logical_path": "/catalog/scripts/a.py" })
                .as_object()
                .unwrap()
                .clone(),
        ];
        app.selected = 0;
        app.load_selected().unwrap();
        drain_until_detail_loaded(&mut app);
        assert_eq!(app.siblings.len(), 1);

        app.mode = ViewMode::Detail;
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        assert!(app.folder_focused);

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        drain_until_detail_loaded(&mut app);
        assert_eq!(
            app.selected_logical_path().as_deref(),
            Some("/catalog/scripts/b.py")
        );
        assert!(!app.folder_focused);
        assert_eq!(
            app.folder_backstack.last().map(String::as_str),
            Some("/catalog/scripts/a.py")
        );

        app.mode = ViewMode::Detail;
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        drain_until_detail_loaded(&mut app);
        assert_eq!(
            app.selected_logical_path().as_deref(),
            Some("/catalog/scripts/a.py")
        );
        assert!(app.folder_backstack.is_empty());
    }

    #[test]
    fn folder_up_browses_parent_directory() {
        let db = super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "UPDATE scripts SET logical_path = '/catalog/scripts/jobs/deep.py'
             WHERE logical_path = '/catalog/scripts/a.py'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose)
             VALUES ('/catalog/scripts/other.py','python','print(2)\\n','bob','')",
            [],
        )
        .unwrap();
        drop(conn);

        let mut app = make_app(db.path());
        app.results = vec![
            serde_json::json!({ "logical_path": "/catalog/scripts/jobs/deep.py" })
                .as_object()
                .unwrap()
                .clone(),
        ];
        app.selected = 0;
        app.load_selected().unwrap();
        drain_until_detail_loaded(&mut app);
        assert!(app.siblings.is_empty());
        assert_eq!(app.folder_display_dir(), "/catalog/scripts/jobs");

        app.mode = ViewMode::Detail;
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE))
            .unwrap();
        drain_until_folder_loaded(&mut app, None);

        assert_eq!(app.folder_dir.as_deref(), Some("/catalog/scripts"));
        let paths: Vec<String> = app
            .siblings
            .iter()
            .map(|row| ScriptView::new(row).logical_path().to_string())
            .collect();
        assert_eq!(paths, vec!["/catalog/scripts/other.py".to_string()]);

        // "/catalog/scripts" -> "/catalog".
        app.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE))
            .unwrap();
        drain_until_folder_loaded(&mut app, Some("/catalog/scripts"));
        assert_eq!(app.folder_dir.as_deref(), Some("/catalog"));

        // "/catalog" -> "/" (root).
        app.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE))
            .unwrap();
        drain_until_folder_loaded(&mut app, Some("/catalog"));
        assert_eq!(app.folder_dir.as_deref(), Some("/"));

        // "[" at the root has nowhere to go, so it must not dispatch another request.
        let next_id_before = app.next_folder_id;
        app.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.next_folder_id, next_id_before);
        assert_eq!(app.folder_dir.as_deref(), Some("/"));
    }

    #[test]
    fn functions_enter_jumps_preview_and_enables_xref() {
        let db = super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose)
             VALUES ('/catalog/scripts/b.py','python','def b():\\n    run()\\n','bob','')",
            [],
        )
        .unwrap();
        let script_a_id: i64 = conn
            .query_row(
                "SELECT id FROM scripts WHERE logical_path = '/catalog/scripts/a.py'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let script_b_id: i64 = conn
            .query_row(
                "SELECT id FROM scripts WHERE logical_path = '/catalog/scripts/b.py'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO function_definitions (script_id, name, kind, line, docstring)
             VALUES (?1, 'run', 'function', 3, 'Runs something.\\nMore details.')",
            rusqlite::params![script_a_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO function_calls
             (script_id, caller, callee, line, resolved_target_name, resolved_target_script_id)
             VALUES (?1, 'b', 'run', 2, 'run', ?2)",
            rusqlite::params![script_b_id, script_a_id],
        )
        .unwrap();
        drop(conn);

        let mut app = make_app(db.path());
        app.results = vec![
            serde_json::json!({ "logical_path": "/catalog/scripts/a.py" })
                .as_object()
                .unwrap()
                .clone(),
        ];
        app.selected = 0;
        app.load_selected().unwrap();
        let detail_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            app.drain_detail_channel();
            if !app.detail_loading {
                break;
            }
            assert!(std::time::Instant::now() < detail_deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        app.focus = Focus::Functions;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.preview_scroll, 2);
        assert_eq!(app.function_xref.as_deref(), Some("run"));
        assert_eq!(app.focus, Focus::Preview);
    }

    #[test]
    fn v_key_queues_full_catalog_content_for_viewer() {
        let db = super::make_test_db();
        let full_content = (0..=super::PREVIEW_LINES)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "UPDATE scripts SET content = ?1 WHERE logical_path = '/catalog/scripts/a.py'",
            [&full_content],
        )
        .unwrap();
        drop(conn);

        let mut app = make_app(db.path());
        app.results = vec![
            serde_json::json!({ "logical_path": "/catalog/scripts/a.py" })
                .as_object()
                .unwrap()
                .clone(),
        ];
        app.selected = 0;
        app.load_selected().unwrap();
        let detail_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            app.drain_detail_channel();
            if !app.detail_loading {
                break;
            }
            assert!(std::time::Instant::now() < detail_deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        app.focus = Focus::Preview;
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        let target = app.pending_view.take().expect("viewer request queued");
        let viewer::ViewTarget::Catalog(view) = target else {
            panic!("expected catalog view target");
        };
        assert_eq!(view.logical_path, "/catalog/scripts/a.py");
        assert_eq!(view.content, full_content);
        assert!(
            view.content
                .contains(&format!("line {}", super::PREVIEW_LINES))
        );
        assert!(
            !app.cached_preview
                .contains(&format!("line {}", super::PREVIEW_LINES))
        );
    }

    #[test]
    fn v_key_in_search_updates_query_instead_of_opening_viewer() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.focus = Focus::Search;

        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.query, "v");
        assert!(app.pending_view.is_none());
    }

    #[test]
    fn f_key_toggles_fullscreen_when_not_in_search() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.focus = Focus::Results;

        assert!(!app.fullscreen);
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.fullscreen, "f should enable fullscreen");

        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))
            .unwrap();
        assert!(!app.fullscreen, "f again should disable fullscreen");
    }

    #[test]
    fn f_key_does_not_toggle_fullscreen_in_search() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.focus = Focus::Search;

        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE))
            .unwrap();
        // f in search pane types into the query, not toggling fullscreen
        assert!(!app.fullscreen);
        assert_eq!(app.query, "f");
    }

    #[test]
    fn esc_exits_fullscreen_before_quitting() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.focus = Focus::Results;
        app.fullscreen = true;

        let should_quit = app
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(!should_quit, "Esc should exit fullscreen, not quit");
        assert!(!app.fullscreen);

        let should_quit = app
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert!(should_quit, "second Esc should quit");
    }
}
