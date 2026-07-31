use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
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

mod app;
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
mod mouse;
mod render;
mod search_worker;
#[cfg(test)]
mod tests;
mod viewer;
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

// The results list only builds `ListItem`s for the rows visible in the pane
// (see `render::draw_results`), so this can be well beyond a single screen
// without making rendering more expensive.
const RESULT_LIMIT: usize = 2000;
const PREVIEW_LINES: usize = 500;
const DEBOUNCE_MS: u64 = 150;
const POLL_TICK_MS: u64 = 50;
const PAGE_SCROLL_LINES: i16 = 40;
const HALF_PAGE_SCROLL_LINES: i16 = 20;
const FULL_PAGE_SCROLL_LINES: i16 = 40;
/// Two left-clicks within this window on the same row count as a double-click.
const DOUBLE_CLICK_MS: u128 = 400;
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
#[derive(Debug, Clone, Copy)]
struct ClickRegion {
    /// Inner content rect (inside the pane border).
    area: Rect,
    kind: RegionKind,
    /// Index of the first row/line visible inside `area` (its scroll offset),
    /// so a click at `area.y + n` maps to entry `scroll + n`.
    scroll: usize,
}

/// Return the pane a click at `(col, row)` fell in, plus the row/line index
/// within that pane's content (accounting for the pane's scroll offset).
/// Regions are searched in recorded order; panes never overlap so the first
/// hit is unambiguous.
fn hit_test(regions: &[ClickRegion], col: u16, row: u16) -> Option<(RegionKind, usize)> {
    regions.iter().find_map(|region| {
        let a = region.area;
        let inside = col >= a.x
            && col < a.x.saturating_add(a.width)
            && row >= a.y
            && row < a.y.saturating_add(a.height);
        inside.then(|| (region.kind, region.scroll + usize::from(row - a.y)))
    })
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
}
fn inner_rect(outer: Rect) -> Rect {
    Rect {
        x: outer.x.saturating_add(1),
        y: outer.y.saturating_add(1),
        width: outer.width.saturating_sub(2),
        height: outer.height.saturating_sub(2),
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
            app.needs_redraw = true;
        }
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

        // Advance the spinner only while something is loading; a frozen tick
        // when idle keeps the buffer identical so no repaint is emitted.
        if app.is_animating() {
            app.tick = app.tick.wrapping_add(1);
            app.needs_redraw = true;
        }

        if app.force_full_redraw {
            terminal.force_full_redraw()?;
            app.force_full_redraw = false;
            app.needs_redraw = true;
        }

        if app.needs_redraw {
            terminal.draw(|frame| draw(frame, &mut app))?;
            app.needs_redraw = false;
        }

        if event::poll(poll_tick)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.flash = None;
                    if app.handle_key(key)? {
                        break;
                    }
                    // A handled keypress may change scroll, selection, or
                    // mode; repaint once (user-paced, so never a flood).
                    app.needs_redraw = true;
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
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        // Mouse capture lets the app respond to clicks/scroll; native text
        // selection is still available via Shift+drag in common terminals.
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
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

    /// Physically clear the screen and discard ratatui's diffed idea of
    /// what's currently on it, so the next `draw` unconditionally rewrites
    /// every cell instead of trusting a possibly-stale diff.
    fn force_full_redraw(&mut self) -> io::Result<()> {
        self.terminal.clear()
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
            EnableMouseCapture
        )?;
        self.terminal.clear()?;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
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
