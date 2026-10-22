use ratatui::text::Line;

use super::{
    DetailPayload, DetailRequest, DetailResponse, DetailWorker, DiffWorker, FileCheckWorker, Focus,
    FolderWorker, Instant, JsonRow, ListState, PathResolver, RESULT_LIMIT, Result, ScriptView,
    SearchRequest, SearchWorker, StatsRequest, StatsResponse, StatsWorker, TuiApp, Value, ViewMode,
    search_worker, sort_checkouts,
};

impl TuiApp {
    pub(super) fn new(
        search_worker: SearchWorker,
        detail_worker: DetailWorker,
        diff_worker: DiffWorker,
        folder_worker: FolderWorker,
        stats_worker: StatsWorker,
        resolver: PathResolver,
    ) -> Result<Self> {
        let mut app = Self {
            terminal_modes: super::capabilities::Capabilities::default(),
            workflow: None,
            pending_administration: None,
            workflow_config: None,
            doctor_flags: Vec::new(),
            workflow_sources: None,
            inspection_worker: None,
            inspection: None,
            inspection_return: None,
            inspection_return_mode: ViewMode::Browse,
            search_options: search_worker::SearchOptions::default(),
            result_limit: RESULT_LIMIT,
            bookmarks: std::collections::HashMap::default(),
            expanded_query: None,
            search_settings: None,
            search_worker,
            detail_worker,
            diff_worker,
            folder_worker,
            resolver,
            query: String::new(),
            query_cursor: 0,
            query_anchor: None,
            query_view_start: 0,
            query_area: super::Rect::default(),
            query_dragging: false,
            native_selection: false,
            popup: None,
            pane_tabs: Vec::new(),
            results: Vec::new(),
            selected: 0,
            focus: Focus::Search,
            detail: None,
            contributors: Vec::new(),
            deps: Vec::new(),
            deps_selected: 0,
            deps_state: ListState::default(),
            functions: Vec::new(),
            functions_selected: 0,
            functions_state: ListState::default(),
            function_call_sites: std::collections::BTreeMap::new(),
            function_xref: None,
            dep_backstack: Vec::new(),
            checkouts: Vec::new(),
            siblings: Vec::new(),
            sibling_dirs: Vec::new(),
            folder_dir: None,
            folder_focused: false,
            siblings_selected: 0,
            folder_backstack: Vec::new(),
            inflight_folder_id: None,
            next_folder_id: 0,
            error: None,
            preview_scroll: 0,
            revisions_scroll: 0,
            revisions_selected: 0,
            detail_scroll: 0,
            mode: ViewMode::Browse,
            diff_return_mode: ViewMode::Detail,
            detail_diff_output: String::new(),
            detail_diff_text: super::wrapped_text::WrappedText::default(),
            detail_diff_loading: false,
            inflight_diff_id: None,
            next_diff_id: 0,
            detail_diff_scroll: 0,
            results_state: ListState::default(),
            results_row_index: Vec::new(),
            cached_preview: String::new(),
            preview_text: super::wrapped_text::WrappedText::default(),
            preview_total_lines: 0,
            detail_loading: false,
            inflight_detail_id: None,
            next_detail_id: 0,
            filter_labels: Vec::new(),
            search: super::search_state::SearchState::default(),
            fullscreen: false,
            tick: 0,
            needs_redraw: true,
            force_full_redraw: false,
            click_regions: Vec::new(),
            last_click: None,
            flash: None,
            pending_view: None,
            file_check_worker: FileCheckWorker::new()?,
            inflight_filecheck_id: None,
            next_filecheck_id: 0,
            stats_worker,
            stats: None,
            stats_error: None,
            stats_loading: false,
            inflight_stats_id: None,
            next_stats_id: 0,
            stats_page: 0,
        };
        app.dispatch_query();
        Ok(app)
    }

    /// Fetch catalog stats for the full-screen stats view. Dispatched fresh
    /// each time the view is opened (see `handle_key`'s `s` binding) rather
    /// than cached indefinitely, so stats reflect the current catalog build
    /// even across a long-lived TUI session.
    pub(super) fn dispatch_stats(&mut self) -> Result<()> {
        let id = self.next_stats_id;
        self.next_stats_id = self.next_stats_id.saturating_add(1);
        self.stats_loading = true;
        self.stats_error = None;
        self.stats_page = 0;
        self.inflight_stats_id = Some(id);
        self.stats_worker.send(StatsRequest { id })?;
        Ok(())
    }

    pub(super) fn drain_stats_channel(&mut self) {
        loop {
            match self.stats_worker.try_recv() {
                Ok(Some(response)) => {
                    self.apply_stats_response(response);
                    self.needs_redraw = true;
                }
                Ok(None) => break,
                Err(_) => {
                    self.inflight_stats_id = None;
                    self.stats_loading = false;
                    self.stats_error = Some("Stats worker disconnected unexpectedly".to_string());
                    self.needs_redraw = true;
                    break;
                }
            }
        }
    }

    pub(super) fn apply_stats_response(&mut self, response: StatsResponse) {
        if Some(response.id) != self.inflight_stats_id {
            return;
        }
        self.inflight_stats_id = None;
        self.stats_loading = false;
        match response.result {
            Ok(stats) => {
                self.stats = Some(stats);
                self.stats_error = None;
            }
            Err(err) => {
                self.stats_error = Some(err);
            }
        }
    }

    pub(super) fn dispatch_query(&mut self) {
        self.error = None;
        let id = self.search.begin();
        let query = if self.search_options.mode == search_worker::SearchMode::Text {
            if let Some(alias) = self.query.strip_prefix('@') {
                let expanded = self
                    .bookmarks
                    .get(alias)
                    .cloned()
                    .unwrap_or_else(|| alias.to_owned());
                self.expanded_query = Some(format!(
                    "{} → {}{}",
                    self.query,
                    expanded,
                    if self.bookmarks.contains_key(alias) {
                        ""
                    } else {
                        " (bookmark missing; fallback)"
                    }
                ));
                expanded
            } else {
                self.expanded_query = None;
                self.query.clone()
            }
        } else {
            self.expanded_query = None;
            self.query.clone()
        };
        self.filter_labels = if self.search_options.mode == search_worker::SearchMode::Text {
            search_worker::parse_query_filters(&query).filter_labels()
        } else {
            Vec::new()
        };
        if let Err(error) = self.search_worker.send(SearchRequest {
            id,
            query,
            limit: self.result_limit,
            options: self.search_options.clone(),
        }) {
            self.search.fail(error.to_string());
            self.error = Some(error.to_string());
        }
    }

    pub(super) fn apply_results(&mut self) -> Result<()> {
        loop {
            let response = match self.search_worker.try_recv() {
                Ok(Some(response)) => response,
                Ok(None) => break,
                Err(error) => {
                    if self.search.is_loading() {
                        self.search.fail(error.to_string());
                        self.error = Some(error.to_string());
                        self.needs_redraw = true;
                    }
                    break;
                }
            };
            self.apply_search_response(response)?;
        }
        Ok(())
    }

    pub(super) fn apply_search_response(
        &mut self,
        response: super::search_worker::SearchResponse,
    ) -> Result<()> {
        if self.search.phase != (super::search_state::Phase::Loading { id: response.id }) {
            return Ok(());
        }
        if let Some(results) = self.search.accept(response) {
            self.apply_search_results(results)?;
        } else if let super::search_state::Phase::Error(error) = &self.search.phase {
            self.error = Some(error.clone());
        }
        self.needs_redraw = true;
        Ok(())
    }

    pub(super) fn apply_search_results(&mut self, results: Vec<JsonRow>) -> Result<()> {
        self.error = None;
        self.results = results;
        self.results_state = ListState::default();
        self.results_row_index.clear();
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
            self.revisions_selected = 0;
            self.siblings.clear();
            self.sibling_dirs.clear();
            self.folder_dir = None;
            self.folder_focused = false;
            self.siblings_selected = 0;
            self.folder_backstack.clear();
            self.inflight_folder_id = None;
            return Ok(());
        }
        if self.selected >= self.results.len() {
            self.selected = self.results.len() - 1;
        }
        self.load_selected()
    }

    pub(super) fn schedule_query(&mut self) {
        self.search.schedule(self.query.clone(), Instant::now());
        // Recompute the filter labels once per query change so the render loop
        // can read them without re-parsing the query every frame.
        self.filter_labels = search_worker::parse_query_filters(&self.query).filter_labels();
    }

    pub(super) fn load_selected(&mut self) -> Result<()> {
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
            self.revisions_selected = 0;
            self.siblings.clear();
            self.sibling_dirs.clear();
            self.folder_dir = None;
            self.folder_focused = false;
            self.siblings_selected = 0;
            self.inflight_folder_id = None;
            self.set_preview_content(String::new(), Vec::new(), 0);
            return Ok(());
        };

        let id = self.next_detail_id;
        self.next_detail_id = self.next_detail_id.saturating_add(1);
        self.detail_loading = true;
        self.error = None;
        self.preview_scroll = 0;
        self.revisions_scroll = 0;
        self.revisions_selected = 0;
        self.detail_scroll = 0;
        self.deps_selected = 0;
        self.functions_selected = 0;
        self.function_xref = None;
        self.folder_dir = None;
        self.folder_focused = false;
        self.siblings_selected = 0;
        self.inflight_folder_id = None;
        self.inflight_detail_id = Some(id);
        self.detail_worker.send(DetailRequest {
            id,
            path: path.to_owned(),
        })?;

        Ok(())
    }

    pub(super) fn drain_detail_channel(&mut self) {
        loop {
            match self.detail_worker.try_recv() {
                Ok(Some(response)) => {
                    self.apply_detail_response(response);
                    self.needs_redraw = true;
                }
                Ok(None) => break,
                Err(_) => {
                    self.inflight_detail_id = None;
                    self.detail_loading = false;
                    self.error = Some("Detail worker disconnected unexpectedly".to_string());
                    self.needs_redraw = true;
                    break;
                }
            }
        }
    }

    pub(super) fn apply_detail_response(&mut self, response: DetailResponse) {
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
            sibling_dirs,
            cached_preview,
            cached_preview_lines,
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
        self.revisions_selected = 0;
        self.siblings = siblings;
        self.sibling_dirs = sibling_dirs;
        self.set_preview_content(cached_preview, cached_preview_lines, preview_total_lines);
        if error.is_some() {
            self.error = error;
        }
    }
}

impl TuiApp {
    /// Replace the catalog preview (raw text, its rendered lines, and its
    /// source line count) shown by the preview pane and the detail view.
    pub(super) fn set_preview_content(
        &mut self,
        text: String,
        lines: Vec<Line<'static>>,
        total_lines: usize,
    ) {
        self.cached_preview = text;
        self.preview_text = super::wrapped_text::WrappedText::new(lines);
        self.preview_total_lines = total_lines;
    }

    /// Replace the diff view's body text.
    pub(super) fn set_diff_output(&mut self, output: String) {
        self.detail_diff_text = super::wrapped_text::WrappedText::from_plain(&output);
        self.detail_diff_output = output;
    }

    pub(super) fn revision_order(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = (0..self.checkouts.len()).collect();
        indices.sort_by_key(|i| {
            match scat_core::core::db::row_string(&self.checkouts[*i], "revision_type").as_str() {
                "" | "DEVELOP" => 0,
                "ROLLBACK" => 1,
                "WORKING" => 2,
                "ARCHIVE" => 3,
                _ => 4,
            }
        });
        indices
    }
    pub(super) fn move_revision(&mut self, delta: isize) {
        let indices = self.revision_order();
        let current = indices
            .iter()
            .position(|i| *i == self.revisions_selected)
            .unwrap_or(0);
        if let Some(index) = indices.get(super::move_selection(current, indices.len(), delta)) {
            self.revisions_selected = *index;
        }
    }
}
