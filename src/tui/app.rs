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
        };
        app.dispatch_query()?;
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

    pub(super) fn dispatch_query(&mut self) -> Result<()> {
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

    pub(super) fn apply_results(&mut self) -> Result<()> {
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

    pub(super) fn apply_search_results(&mut self, results: Vec<JsonRow>) -> Result<()> {
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
        self.pending_query = Some(self.query.clone());
        self.last_keystroke_at = Some(Instant::now());
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
            self.siblings.clear();
            self.sibling_dirs.clear();
            self.folder_dir = None;
            self.folder_focused = false;
            self.siblings_selected = 0;
            self.inflight_folder_id = None;
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
        self.sibling_dirs = sibling_dirs;
        self.cached_preview = cached_preview;
        self.preview_total_lines = preview_total_lines;
        if error.is_some() {
            self.error = error;
        }
    }
}
