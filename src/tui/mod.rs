use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use ratatui::{Frame, Terminal};
use serde_json::Value;

use scat_core::core::db::{JsonRow, row_string as str_field};
use scat_core::core::resolve::PathResolver;
use scat_core::core::script_view::{ScriptView, logical_parent_dir};

mod actions;
mod app;
pub mod capabilities;
mod clipboard;
mod detail;
mod detail_worker;
mod diff_view;
mod diff_worker;
mod file_check_worker;
mod file_view;
mod folder;
mod folder_worker;
mod handlers;
mod helpers;
mod highlight;
mod inspection;
mod mouse;
#[cfg(test)]
mod perf_tests;
mod render;
mod search_input;
mod search_settings;
mod search_state;
mod search_worker;
mod stats_worker;
#[cfg(test)]
mod tests;
pub mod viewer;
mod workflows;
mod wrapped_text;
mod xref;

use self::detail_worker::{
    DependencyItem, DetailPayload, DetailRequest, DetailResponse, DetailWorker, FunctionCallSite,
    FunctionItem,
};
use self::diff_worker::{DiffRequest, DiffResponse, DiffWorker};
use self::file_check_worker::{FileCheckRequest, FileCheckResponse, FileCheckWorker};
use self::folder_worker::{FolderListing, FolderRequest, FolderResponse, FolderWorker};
use self::helpers::{
    apply_scroll_key, move_selection, next_focus, previous_focus, scroll_by, search_title,
    sort_checkouts,
};
use self::render::draw;
use self::search_worker::{SearchRequest, SearchWorker};
use self::stats_worker::{StatsRequest, StatsResponse, StatsWorker};

// The results list only builds `ListItem`s for the rows visible in the pane
// (see `render::draw_results`), so this can be well beyond a single screen
// without making rendering more expensive.
const RESULT_LIMIT: usize = 2000;
const DEBOUNCE_MS: u64 = 150;
/// Input-poll timeout while a worker reply is pending or a spinner animates.
const POLL_TICK_MS: u64 = 50;
/// Input-poll timeout when nothing is pending: input (including resize)
/// wakes the loop immediately regardless, so this only bounds how late any
/// worker reply that `TuiApp::has_pending_work` misses would be noticed.
const IDLE_POLL_MS: u64 = 1000;
const PAGE_SCROLL_LINES: i16 = 40;
const HALF_PAGE_SCROLL_LINES: i16 = 20;
const FULL_PAGE_SCROLL_LINES: i16 = 40;
/// Two left-clicks within this window on the same row count as a double-click.
const DOUBLE_CLICK_MS: u128 = 400;
/// Number of pages in the full-screen stats view (`ViewMode::Stats`); see
/// `render::stats_view`.
const STATS_PAGE_COUNT: usize = 3;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Search,
    Results,
    Preview,
    Deps,
    Functions,
    Revisions,
}
/// Kind of on-screen pane, recorded per frame so a mouse click can be
/// hit-tested against the layout after rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegionKind {
    /// The top header line, which shows the selected script's path.
    Header,
    /// The search input box.
    Search,
    Results,
    Metadata,
    Preview,
    Deps,
    Functions,
    Revisions,
    /// The scrollable full-screen detail-view body.
    DetailBody,
    /// The scrollable full-screen script-diff body.
    DetailDiffBody,
}

/// A clickable pane recorded during render.
#[derive(Debug, Clone)]
struct ClickRegion {
    /// Inner content rect (inside the pane border).
    area: Rect,
    kind: RegionKind,
    /// Index of the first row/line visible inside `area` (its scroll offset),
    /// so a click at `area.y + n` maps to entry `scroll + n`. Ignored when
    /// `row_item_index` is set.
    scroll: usize,
    /// For a pane whose items can span more than one screen row (currently
    /// only Results, when a symlink target wraps to its own line): maps each
    /// rendered row within `area` (0-indexed from the top) to the item index
    /// it belongs to, so every row of a multi-row item resolves to the same
    /// entry. `None` for every other, uniform single-row-per-item pane,
    /// which falls back to `scroll + row`.
    row_item_index: Option<Vec<usize>>,
}

/// Return the pane a click at `(col, row)` fell in, plus the row/line index
/// within that pane's content (accounting for the pane's scroll offset, or
/// its `row_item_index` map for a variable-row-height pane). Regions are
/// searched in recorded order; panes never overlap so the first hit is
/// unambiguous.
fn hit_test(regions: &[ClickRegion], col: u16, row: u16) -> Option<(RegionKind, usize)> {
    regions.iter().find_map(|region| {
        let a = region.area;
        let inside = col >= a.x
            && col < a.x.saturating_add(a.width)
            && row >= a.y
            && row < a.y.saturating_add(a.height);
        if !inside {
            return None;
        }
        let row_in_area = usize::from(row - a.y);
        let index = match &region.row_item_index {
            // A click past the last rendered item (the pane's unused
            // trailing space) has no entry — no other region can claim this
            // (col, row) either, since panes never overlap, so the overall
            // hit-test correctly reports "nothing here" rather than falling
            // through to flat scroll+row arithmetic.
            Some(map) => *map.get(row_in_area)?,
            None => region.scroll + row_in_area,
        };
        Some((region.kind, index))
    })
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Browse,
    Detail,
    DetailDiff,
    Stats,
    DepGraph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollCommand {
    Delta(i16),
    Top,
    Bottom,
}
struct TuiApp {
    terminal_modes: capabilities::Capabilities,
    workflow: Option<workflows::Dialog>,
    pending_administration: Option<Vec<String>>,
    workflow_config: Option<PathBuf>,
    doctor_flags: Vec<String>,
    workflow_sources: Option<(PathBuf, PathBuf)>,
    inspection_worker: Option<inspection::Worker>,
    inspection: Option<inspection::Page>,
    inspection_return: Option<inspection::Page>,
    inspection_return_mode: ViewMode,
    search_options: search_worker::SearchOptions,
    result_limit: usize,
    bookmarks: std::collections::HashMap<String, String>,
    expanded_query: Option<String>,
    search_settings: Option<search_settings::Editor>,
    search_worker: SearchWorker,
    detail_worker: DetailWorker,
    diff_worker: DiffWorker,
    folder_worker: FolderWorker,
    resolver: PathResolver,
    query: String,
    /// UTF-8 byte offset of the search insertion point.
    query_cursor: usize,
    query_anchor: Option<usize>,
    query_view_start: usize,
    query_area: Rect,
    query_dragging: bool,
    /// Freeze the display and release mouse capture for terminal selection.
    native_selection: bool,
    popup: Option<actions::Popup>,
    pane_tabs: Vec<(Rect, Focus)>,
    results: Vec<JsonRow>,
    selected: usize,
    focus: Focus,
    detail: Option<JsonRow>,
    contributors: Vec<String>,
    deps: Vec<DependencyItem>,
    deps_selected: usize,
    /// Scroll offset for the Deps pane, kept in the same
    /// `ListState`-plus-manual-window shape as `results_state` so the two
    /// panes' scrolling behaves identically (see `render::scroll_window`).
    deps_state: ListState,
    functions: Vec<FunctionItem>,
    functions_selected: usize,
    /// Scroll offset for the Functions pane; see `deps_state`.
    functions_state: ListState,
    function_call_sites: std::collections::BTreeMap<String, Vec<FunctionCallSite>>,
    function_xref: Option<String>,
    dep_backstack: Vec<String>,
    checkouts: Vec<JsonRow>,
    siblings: Vec<JsonRow>,
    /// Immediate subdirectory names of the folder shown in the Folder
    /// section, listed before `siblings` in the browse list.
    sibling_dirs: Vec<String>,
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
    /// Index into `checkouts` (pre-sorted by `compare_revision_rows`, so
    /// this order matches the pane's visual grouping) of the entry Enter
    /// diffs the active script against. Up/Down move this instead of
    /// `revisions_scroll` directly; `revisions_scroll` auto-follows to keep
    /// the selection on screen — see `render::revisions`.
    revisions_selected: usize,
    detail_scroll: u16,
    mode: ViewMode,
    diff_return_mode: ViewMode,
    /// Raw diff text, kept for copying; `detail_diff_text` is what renders.
    /// Replace both only through `set_diff_output`.
    detail_diff_output: String,
    detail_diff_text: wrapped_text::WrappedText,
    detail_diff_loading: bool,
    inflight_diff_id: Option<u64>,
    next_diff_id: u64,
    detail_diff_scroll: u16,
    results_state: ListState,
    /// Rendered-row → item-index map for the results pane's current visible
    /// window, written by `render::draw_results` and read back by the
    /// `record_region_with_row_index` call after it — see
    /// `ClickRegion::row_item_index`.
    results_row_index: Vec<usize>,
    cached_preview: String,
    /// Syntax-highlighted (Python/Bash) or plain (any other language)
    /// rendering of `cached_preview`, computed once per detail load by
    /// `detail_worker::load_detail` and reused as-is by both the catalog
    /// preview pane (`render::preview`) and the detail view's content tail
    /// (`render::detail_view`), rather than re-parsing per pane/frame. Its
    /// wrapped layout is cached too, so a frame only touches visible lines.
    /// Replace it only through `set_preview_content`.
    preview_text: wrapped_text::WrappedText,
    preview_total_lines: usize,
    detail_loading: bool,
    inflight_detail_id: Option<u64>,
    next_detail_id: u64,
    /// Active `lang:`/`owner:`/`tag:` filter labels for the current query,
    /// recomputed only when the query changes (not per render frame).
    filter_labels: Vec<String>,
    search: search_state::SearchState,
    fullscreen: bool,
    tick: u64,
    /// Set whenever visible state changes; the run loop only repaints when
    /// this is set (or a spinner is animating), so an idle screen isn't
    /// redrawn every poll tick — a constant repaint wipes the terminal's
    /// own mouse text selection.
    needs_redraw: bool,
    /// Set after a clipboard write: the OSC 52 escape sequence goes straight
    /// to stdout, bypassing ratatui's `Terminal` (see `clipboard.rs`). Some
    /// terminals respond to it with a visible permission prompt, or don't
    /// swallow it cleanly, which can desync the physical screen from
    /// ratatui's diffed idea of what's on it (seen as the whole TUI shifting
    /// by a line). The run loop clears and fully repaints on the next frame
    /// to self-correct regardless of the exact terminal quirk.
    force_full_redraw: bool,
    /// Clickable panes recorded during the last render, for mouse hit-testing.
    click_regions: Vec<ClickRegion>,
    /// Position and time of the last left-click, for double-click detection.
    last_click: Option<(u16, u16, Instant)>,
    /// Transient status message (e.g. "Copied …"), shown in the footer until
    /// the next input event.
    flash: Option<String>,
    pending_view: Option<viewer::ViewTarget>,
    file_check_worker: FileCheckWorker,
    inflight_filecheck_id: Option<u64>,
    next_filecheck_id: u64,
    stats_worker: stats_worker::StatsWorker,
    /// Result of the most recent `stats` fetch, shown by the full-screen
    /// stats view (`ViewMode::Stats`). `None` before the first fetch
    /// completes or after a failed one.
    stats: Option<scat_core::core::search::StatsResult>,
    stats_error: Option<String>,
    stats_loading: bool,
    inflight_stats_id: Option<u64>,
    next_stats_id: u64,
    /// Which of the stats view's pages is showing (0-indexed). Reset to `0`
    /// each time the view is opened; see `render::stats_view`.
    stats_page: usize,
}
fn inner_rect(outer: Rect) -> Rect {
    Rect {
        x: outer.x.saturating_add(1),
        y: outer.y.saturating_add(1),
        width: outer.width.saturating_sub(2),
        height: outer.height.saturating_sub(2),
    }
}
pub struct RunOptions {
    pub doctor_flags: Vec<String>,
    pub bookmarks: std::collections::HashMap<String, String>,
    pub terminal_modes: capabilities::Capabilities,
}

/// No-op SIGINT handler: async-signal-safe, and reset to default by `exec`.
extern "C" fn ignore_sigint(_: libc::c_int) {}

pub fn run(
    db_path: &Path,
    embeddings_path: &Path,
    source_db: &Path,
    source_embeddings: &Path,
    source_config: Option<&Path>,
    resolver: PathResolver,
    options: RunOptions,
) -> Result<()> {
    options.terminal_modes.validate()?;
    // The workers open the catalog on their own threads, where a schema
    // mismatch would only surface as a per-query error inside the fullscreen
    // UI. Check once up front so it is reported on the terminal, with the
    // actionable message, before any terminal setup.
    scat_core::core::db::open_db(db_path)
        .map(drop)
        .with_context(|| format!("Failed to open database: {}", source_db.display()))?;
    let search_worker = SearchWorker::new(db_path)?;
    let detail_worker = DetailWorker::new(db_path)?;
    let diff_worker = DiffWorker::new(db_path)?;
    let folder_worker = FolderWorker::new(db_path)?;
    let stats_worker = StatsWorker::new(db_path)?;
    let mut app = TuiApp::new(
        search_worker,
        detail_worker,
        diff_worker,
        folder_worker,
        stats_worker,
        resolver,
    )?;
    app.inspection_worker = Some(inspection::Worker::new(db_path, embeddings_path)?);
    app.workflow_sources = Some((source_db.to_owned(), source_embeddings.to_owned()));
    app.workflow_config = source_config.map(Path::to_owned);
    app.doctor_flags = options.doctor_flags;
    app.bookmarks = options.bookmarks;
    app.terminal_modes = options.terminal_modes;
    // Keep the parent alive when the foreground child handles Ctrl+C. In raw
    // mode the TUI still receives Ctrl+C as a key and exits normally.
    // SIGINT only: SIGTERM and SIGHUP keep their default termination.
    let handler: extern "C" fn(libc::c_int) = ignore_sigint;
    // SAFETY: the handler is empty, hence async-signal-safe.
    if unsafe { libc::signal(libc::SIGINT, handler as libc::sighandler_t) } == libc::SIG_ERR {
        anyhow::bail!(
            "Could not install TUI interrupt handler: {}",
            io::Error::last_os_error()
        );
    }
    let mut terminal = TerminalGuard::enter()?;

    loop {
        app.advance(Instant::now())?;
        // A view target may be queued synchronously (catalog content) or
        // asynchronously once the file-check worker confirms the live source,
        // so open it from the loop body rather than only after a keystroke.
        // Opening suspends/resumes the terminal (clearing the screen), so a
        // full repaint is always required afterward.
        if let Some(target) = app.pending_view.take() {
            if let Err(err) = terminal.open_view(&target) {
                app.error = Some(format!("Failed to open viewer: {err}"));
            }
            app.needs_redraw = true;
        }

        if let Some(args) = app.take_administrative_command() {
            let result = terminal.suspend_for(|| {
                let status = std::process::Command::new(std::env::current_exe()?)
                    .args(args)
                    .status()?;
                println!("\nOperation finished: {status}. Press Enter to return to browsing.");
                let mut line = String::new();
                io::stdin().read_line(&mut line)?;
                anyhow::ensure!(status.success(), "{status}");
                Ok(())
            });
            if let Some(d) = &mut app.workflow {
                d.message = match result {
                    Ok(()) => {
                        "Operation completed. Restart TUI to read a newly published catalog.".into()
                    }
                    Err(e) => format!("Operation failed or interrupted: {e:#}"),
                };
            }
            app.needs_redraw = true;
        }
        if app.force_full_redraw {
            terminal.force_full_redraw()?;
            app.force_full_redraw = false;
            app.needs_redraw = true;
        }

        terminal.set_native_selection(app.native_selection)?;

        if app.needs_redraw {
            terminal.draw(|frame| draw(frame, &mut app))?;
            app.needs_redraw = false;
        }

        if event::poll(app.poll_timeout(Instant::now()))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let was_selecting = app.native_selection;
                    let had_popup = app.popup.is_some();
                    if !was_selecting {
                        app.flash = None;
                    }
                    if app.handle_key(key)? {
                        break;
                    }
                    // A handled keypress may change scroll, selection, or
                    // mode; repaint once (user-paced, so never a flood).
                    app.needs_redraw |= !app.native_selection
                        || was_selecting != app.native_selection
                        || had_popup
                        || app.popup.is_some();
                }
                Event::Paste(text) => {
                    app.needs_redraw |= app.handle_paste(&text);
                }
                Event::Mouse(mouse) => {
                    // Only acted-on events repaint; bare moves/drags leave the
                    // screen (and any Shift+drag selection) untouched.
                    app.needs_redraw |= app.handle_mouse(mouse)?;
                }
                // ratatui resizes its buffers on the next draw; force one.
                Event::Resize(_, _) => app.needs_redraw = true,
                _ => {}
            }
        }
    }
    Ok(())
}
impl TuiApp {
    /// The terminal-independent part of one run-loop iteration: apply worker
    /// replies, fire the search debounce once it has elapsed, and advance
    /// the spinner. Sets `needs_redraw` only when visible state changed, so
    /// an idle app draws nothing.
    fn advance(&mut self, now: Instant) -> Result<()> {
        if self.native_selection {
            return Ok(());
        }
        self.apply_results()?;
        self.drain_detail_channel();
        self.drain_diff_channel();
        self.drain_file_check_channel();
        self.drain_folder_channel();
        self.drain_stats_channel();
        self.drain_inspection();
        self.drain_workflow();
        if self.search.pending_query.is_some()
            && self.search.last_keystroke_at.is_some_and(|keystroke| {
                now.saturating_duration_since(keystroke) >= Duration::from_millis(DEBOUNCE_MS)
            })
        {
            self.search.last_keystroke_at = None;
            self.dispatch_query();
            self.needs_redraw = true;
        }
        // Advance the spinner only while something is loading; a frozen tick
        // when idle keeps the buffer identical so no repaint is emitted.
        if self.is_animating() {
            self.tick = self.tick.wrapping_add(1);
            self.needs_redraw = true;
        }
        Ok(())
    }

    /// Whether a worker reply is still expected. Replies arrive on channels
    /// the loop drains rather than as terminal events, so it keeps polling
    /// at `POLL_TICK_MS` until they land.
    fn has_pending_work(&self) -> bool {
        self.search.is_loading()
            || self.detail_loading
            || self.inflight_detail_id.is_some()
            || self.inflight_diff_id.is_some()
            || self.inflight_filecheck_id.is_some()
            || self.inflight_folder_id.is_some()
            || self.inflight_stats_id.is_some()
            || self.inspection.as_ref().is_some_and(|page| page.loading)
            || self.workflow.as_ref().is_some_and(|dialog| dialog.busy)
    }

    /// How long the loop may block waiting for input before `advance` has
    /// something to do: a short tick while animating or awaiting a worker,
    /// the rest of the search debounce while a query is pending, otherwise
    /// `IDLE_POLL_MS`. While the display is frozen for native selection,
    /// nothing advances until a key ends it.
    fn poll_timeout(&self, now: Instant) -> Duration {
        let tick = Duration::from_millis(POLL_TICK_MS);
        let idle = Duration::from_millis(IDLE_POLL_MS);
        if self.native_selection {
            return idle;
        }
        if self.is_animating() || self.has_pending_work() {
            return tick;
        }
        match (&self.search.pending_query, self.search.last_keystroke_at) {
            (Some(_), Some(keystroke)) => Duration::from_millis(DEBOUNCE_MS)
                .saturating_sub(now.saturating_duration_since(keystroke))
                .min(idle),
            _ => idle,
        }
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    mouse_capture: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        let backend = CrosstermBackend::new(io::stdout());
        let mut guard = Self {
            terminal: Terminal::new(backend)?,
            mouse_capture: false,
        };
        enable_raw_mode()?;
        // Construct the guard first so setup failures also restore the terminal.
        execute!(
            guard.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        guard.mouse_capture = true;
        Ok(guard)
    }

    fn set_native_selection(&mut self, selecting: bool) -> io::Result<()> {
        let capture = !selecting;
        if self.mouse_capture != capture {
            if selecting {
                execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
            } else {
                execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
            }
            self.mouse_capture = capture;
        }
        Ok(())
    }

    fn draw<F>(&mut self, f: F) -> io::Result<ratatui::CompletedFrame<'_>>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.terminal.draw(f)
    }

    /// Physically clear the screen and discard ratatui's diffed idea of
    /// what's currently on it, so the next `draw` unconditionally rewrites
    /// every cell instead of trusting a possibly-stale diff.
    fn force_full_redraw(&mut self) -> io::Result<()> {
        // Fullscreen resize clears the screen and resets both buffers without
        // querying the terminal cursor (which can block unsupported terminals).
        self.terminal.resize(self.terminal.size()?.into())
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
        execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        )?;
        self.terminal.show_cursor()?;
        Ok(())
    }

    fn resume(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableBracketedPaste
        )?;
        if self.mouse_capture {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
        }
        self.force_full_redraw()?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

#[cfg(test)]
pub fn make_test_db() -> tempfile::NamedTempFile {
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
