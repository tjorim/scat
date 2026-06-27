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
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListState;
use ratatui::{Frame, Terminal};
use serde_json::Value;

use scat_core::core::db::{JsonRow, row_display, row_string as str_field};
use scat_core::core::resolve::PathResolver;
use scat_core::core::vc::{compare_revision_rows, relative_age};

mod detail_worker;
mod diff_worker;
mod render;
mod search_worker;
mod viewer;

use self::detail_worker::{
    DependencyItem, DetailPayload, DetailRequest, DetailResponse, DetailWorker, FunctionCallSite,
    FunctionItem,
};
use self::diff_worker::{DiffRequest, DiffResponse, DiffWorker};
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
    detail_loading: bool,
    inflight_detail_id: Option<u64>,
    next_detail_id: u64,
    last_keystroke_at: Option<Instant>,
    pending_query: Option<String>,
    inflight_query_id: Option<u64>,
    search_in_flight: bool,
    next_query_id: u64,
    fullscreen: bool,
    tick: u64,
    pending_view: Option<viewer::ViewTarget>,
}

impl TuiApp {
    fn new(
        search_worker: SearchWorker,
        detail_worker: DetailWorker,
        diff_worker: DiffWorker,
        resolver: PathResolver,
    ) -> Result<Self> {
        let mut app = Self {
            search_worker,
            detail_worker,
            diff_worker,
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
            detail_loading: false,
            inflight_detail_id: None,
            next_detail_id: 0,
            last_keystroke_at: None,
            pending_query: None,
            inflight_query_id: None,
            search_in_flight: false,
            next_query_id: 0,
            fullscreen: false,
            tick: 0,
            pending_view: None,
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
            .and_then(|row| row.get("logical_path"))
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
            self.cached_preview.clear();
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
            cached_preview,
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
        self.cached_preview = cached_preview;
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
            _ => {
                apply_scroll_key(&mut self.detail_scroll, key);
            }
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
            row.get("logical_path")
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
            .and_then(|row| row.get("logical_path"))
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.results
                    .get(self.selected)
                    .and_then(|row| row.get("logical_path"))
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
        match self.live_source_target() {
            Ok(target) => {
                self.error = None;
                self.pending_view = Some(target);
            }
            Err(err) => {
                self.error = Some(err.to_string());
            }
        }
    }

    fn catalog_view_target(&self) -> Result<viewer::CatalogView> {
        if self.detail_loading {
            anyhow::bail!("Script is still loading.");
        }
        let Some(row) = self.detail.as_ref() else {
            anyhow::bail!("No script selected.");
        };
        let logical_path = str_field(row, "logical_path");
        if logical_path.is_empty() {
            anyhow::bail!("Selected script has no logical path.");
        }
        Ok(viewer::CatalogView {
            logical_path,
            content: str_field(row, "content"),
        })
    }

    fn live_source_target(&self) -> Result<viewer::ViewTarget> {
        if self.detail_loading {
            anyhow::bail!("Script is still loading.");
        }
        let Some(row) = self.detail.as_ref() else {
            anyhow::bail!("No script selected.");
        };
        let logical_path = str_field(row, "logical_path");
        if logical_path.is_empty() {
            anyhow::bail!("Selected script has no logical path.");
        }
        let native = self.resolver.to_native(&logical_path);
        if native == logical_path {
            anyhow::bail!(
                "No filesystem mapping for {logical_path}; configure a path mapping to open the live source."
            );
        }
        let native_path = PathBuf::from(native);
        if !native_path.exists() {
            anyhow::bail!("Live source not found at {}", native_path.display());
        }
        Ok(viewer::ViewTarget::LiveSource {
            logical_path,
            native_path,
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
    let mut app = TuiApp::new(search_worker, detail_worker, diff_worker, resolver)?;
    let mut terminal = TerminalGuard::enter()?;
    let debounce = Duration::from_millis(DEBOUNCE_MS);
    let poll_tick = Duration::from_millis(POLL_TICK_MS);

    loop {
        app.apply_results()?;
        app.drain_detail_channel();
        app.drain_diff_channel();
        if app
            .last_keystroke_at
            .is_some_and(|keystroke| keystroke.elapsed() >= debounce)
            && app.pending_query.is_some()
        {
            app.last_keystroke_at = None;
            app.dispatch_query()?;
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
            if let Some(target) = app.pending_view.take()
                && let Err(err) = terminal.open_view(&target)
            {
                app.error = Some(format!("Failed to open viewer: {err}"));
            }
        }
    }
    Ok(())
}

fn detail_lines(app: &TuiApp) -> Vec<Line<'static>> {
    if app.detail_loading {
        return vec![Line::from("Loading…")];
    }
    let Some(row) = app.detail.as_ref() else {
        return vec![Line::from("No script selected.")];
    };

    let mut lines = vec![
        section("Script"),
        field_line("Path", str_field(row, "logical_path")),
        field_line("Language", display_field(row, "language")),
        field_line("Owner", display_field(row, "owner")),
        field_line("Purpose", display_field(row, "purpose")),
        field_line("Size", format!("{} bytes", display_field(row, "size"))),
        field_line("Indexed", display_field(row, "indexed_at")),
        field_line("Checkout", checkout_label(row)),
    ];
    if let Some(native) = native_path_for_row(row, &app.resolver) {
        lines.push(field_line("OS path", native));
    }

    for (label, key) in [
        ("Tags", "tags"),
        ("Entry points", "entry_points"),
        ("Related metadata", "related"),
    ] {
        let values = json_string_array(row, key);
        if !values.is_empty() {
            lines.push(field_line(label, values.join(", ")));
        }
    }

    let warnings = warning_messages(row);
    if !warnings.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Warnings"));
        for warning in warnings {
            lines.push(bullet_line(warning));
        }
    }

    if !app.deps.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Dependencies"));
        for item in &app.deps {
            lines.push(bullet_line(format!("{} {}", item.kind, item.logical_path)));
        }
    }

    if !app.checkouts.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Checkouts"));
        for checkout in &app.checkouts {
            let user = display_field(checkout, "user");
            let os = display_field(checkout, "os_flavor");
            let timestamp = display_field(checkout, "timestamp");
            let path = display_field(checkout, "physical_path");
            lines.push(bullet_line(format!("{user} on {os} since {timestamp}")));
            lines.push(Line::from(format!("    {path}")));
        }
    }

    let content = str_field(row, "content");
    if !content.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Preview"));
        for line in content.lines().take(PREVIEW_LINES) {
            lines.push(Line::from(line.to_string()));
        }
    }

    lines
}

fn sort_checkouts(checkouts: &mut [JsonRow]) {
    checkouts.sort_by(compare_revision_rows);
}

fn display_field(row: &JsonRow, key: &str) -> String {
    row_display(row, key, "-")
}

fn checkout_label(row: &JsonRow) -> String {
    let user = str_field(row, "checkout_user");
    if user.is_empty() {
        return "clean".to_string();
    }
    let timestamp = str_field(row, "checkout_timestamp");
    if timestamp.is_empty() {
        format!("checked out by {user}")
    } else {
        format!("checked out by {user} since {timestamp}")
    }
}

fn native_path_for_row(row: &JsonRow, resolver: &PathResolver) -> Option<String> {
    let path = row.get("logical_path")?.as_str()?;
    if path.is_empty() {
        return None;
    }
    let native = resolver.to_native(path);
    if native == path { None } else { Some(native) }
}

fn warning_summary(row: &JsonRow) -> String {
    warning_messages(row).join("; ")
}

fn warning_messages(row: &JsonRow) -> Vec<String> {
    let raw = str_field(row, "vc_warnings");
    let Ok(Value::Array(warnings)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    warnings
        .iter()
        .filter_map(|warning| warning.get("message").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn json_string_array(row: &JsonRow, key: &str) -> Vec<String> {
    let raw = str_field(row, key);
    let Ok(Value::Array(values)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

fn section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn field_line(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<16}"), label_style()),
        Span::raw(value),
    ])
}

fn bullet_line(value: String) -> Line<'static> {
    Line::from(format!("  - {value}"))
}

fn label_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
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

    use super::{
        DetailPayload, DetailResponse, DetailWorker, DiffWorker, Focus, SearchWorker, TuiApp,
        ViewMode, json_string_array, move_selection, native_path_for_row, next_focus,
        previous_focus, scroll_by, search_title, viewer, warning_messages,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use scat_core::core::resolve::PathResolver;
    use serde_json::{Map, Value, json};

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
    fn parses_string_arrays_for_detail_view() {
        let mut row = Map::new();
        row.insert(
            "tags".to_string(),
            Value::String(json!(["one", "two"]).to_string()),
        );
        assert_eq!(json_string_array(&row, "tags"), vec!["one", "two"]);
    }

    #[test]
    fn parses_warning_messages_for_detail_view() {
        let mut row = Map::new();
        row.insert(
            "vc_warnings".to_string(),
            Value::String(json!([{"message": "stale checkout"}]).to_string()),
        );
        assert_eq!(warning_messages(&row), vec!["stale checkout"]);
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

    #[test]
    fn live_source_target_resolves_mapped_path() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("foo.py");
        std::fs::write(&script, "print(1)\n").unwrap();

        let root = dir.path().display().to_string();
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
        let resolver = PathResolver::from_file(file.path()).unwrap();

        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.resolver = resolver;
        app.detail = Some(detail_row("/catalog/scripts/foo.py"));
        app.detail_loading = false;

        let target = app.live_source_target().expect("live source resolves");
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
    }

    #[test]
    fn live_source_target_errors_without_mapping() {
        let db = super::make_test_db();
        let mut app = make_app(db.path());
        // Identity resolver: no mapping resolves the logical path to disk.
        app.detail = Some(detail_row("/catalog/scripts/foo.py"));
        app.detail_loading = false;

        let err = app
            .live_source_target()
            .expect_err("identity mapping should fail to resolve live source");
        assert!(
            err.to_string().contains("No filesystem mapping"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn live_source_target_errors_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        // Note: no file is created at the resolved path.
        let root = dir.path().display().to_string();
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
        let resolver = PathResolver::from_file(file.path()).unwrap();

        let db = super::make_test_db();
        let mut app = make_app(db.path());
        app.resolver = resolver;
        app.detail = Some(detail_row("/catalog/scripts/missing.py"));
        app.detail_loading = false;

        let err = app
            .live_source_target()
            .expect_err("missing file should fail clearly");
        assert!(
            err.to_string().contains("Live source not found"),
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
        TuiApp::new(
            search_worker,
            detail_worker,
            diff_worker,
            PathResolver::new(),
        )
        .unwrap()
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
                cached_preview: "x".to_string(),
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
                cached_preview: String::new(),
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
