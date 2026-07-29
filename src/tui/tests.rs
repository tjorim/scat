use std::io::Write;

use super::detail::native_path_for_row;
use super::{
    ClickRegion, DetailPayload, DetailResponse, DetailWorker, DiffWorker, Focus, FolderListing,
    FolderResponse, FolderWorker, RegionKind, SearchWorker, TuiApp, ViewMode, hit_test,
    move_selection, next_focus, previous_focus, scroll_by, search_title, viewer,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
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
fn hit_test_maps_click_to_pane_and_scrolled_index() {
    let regions = vec![
        ClickRegion {
            area: Rect::new(0, 1, 30, 10),
            kind: RegionKind::Results,
            scroll: 5,
        },
        ClickRegion {
            area: Rect::new(31, 1, 40, 6),
            kind: RegionKind::Deps,
            scroll: 0,
        },
    ];

    // First row of the results pane maps to its scroll offset (5).
    assert_eq!(hit_test(&regions, 2, 1), Some((RegionKind::Results, 5)));
    // Three rows down → offset 5 + 3.
    assert_eq!(hit_test(&regions, 2, 4), Some((RegionKind::Results, 8)));
    // A click in the deps pane, second row, scroll 0 → index 1.
    assert_eq!(hit_test(&regions, 40, 2), Some((RegionKind::Deps, 1)));
    // Outside every region.
    assert_eq!(hit_test(&regions, 100, 100), None);
    // On a pane's border (row 0, above inner area) → miss.
    assert_eq!(hit_test(&regions, 2, 0), None);
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
            sibling_dirs: vec![],
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
            sibling_dirs: vec![],
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
fn is_animating_only_while_a_spinner_is_on_screen() {
    let db = super::make_test_db();
    let mut app = make_app(db.path());

    // Fresh app dispatches the initial query, so a search is in flight
    // with no results yet — the "Searching…" spinner animates.
    assert!(app.is_animating());

    // Once results and the detail load settle, nothing animates and the
    // run loop can leave the screen (and any selection) untouched.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while app.search_in_flight {
        app.apply_results().unwrap();
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for search worker"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    drain_until_detail_loaded(&mut app);
    assert!(!app.results.is_empty());
    assert!(!app.is_animating());

    // A detail load in progress animates its "Loading…" spinners.
    app.detail_loading = true;
    assert!(app.is_animating());
    app.detail_loading = false;

    // A diff load animates the diff view spinner.
    app.detail_diff_loading = true;
    assert!(app.is_animating());
    app.detail_diff_loading = false;

    // An in-flight search with results already shown has no visible
    // spinner, so it must not animate.
    app.search_in_flight = true;
    assert!(!app.results.is_empty());
    assert!(!app.is_animating());
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
fn folder_selected_line_tracks_marker_position() {
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
    app.folder_focused = true;
    app.siblings_selected = 0;

    let lines = super::detail::detail_lines(&app);
    let selected = super::detail::folder_selected_line(&app, &lines).unwrap();
    let text: String = lines[selected as usize]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert!(text.starts_with("> "), "selected line: {text:?}");
}

#[test]
fn folder_enter_descends_into_selected_subdirectory() {
    let db = super::make_test_db();
    let conn = rusqlite::Connection::open(db.path()).unwrap();
    conn.execute(
        "INSERT INTO scripts (logical_path, language, content, owner, purpose)
         VALUES ('/catalog/scripts/jobs/deep.py','python','print(2)\\n','bob','')",
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
    // The initial detail load already lists the subdirectory.
    assert_eq!(app.sibling_dirs, vec!["jobs"]);
    assert!(app.siblings.is_empty());

    app.mode = ViewMode::Detail;
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    // Selection starts on the "jobs/" directory entry; Enter descends
    // into it instead of jumping to a script.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    drain_until_folder_loaded(&mut app, None);

    assert_eq!(app.folder_dir.as_deref(), Some("/catalog/scripts/jobs"));
    assert!(app.sibling_dirs.is_empty());
    let paths: Vec<String> = app
        .siblings
        .iter()
        .map(|row| ScriptView::new(row).logical_path().to_string())
        .collect();
    assert_eq!(paths, vec!["/catalog/scripts/jobs/deep.py".to_string()]);
    // Still browsing: descending must not leave folder-browse mode.
    assert!(app.folder_focused);

    // Enter on the script inside the subfolder jumps the detail view.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    drain_until_detail_loaded(&mut app);
    assert_eq!(
        app.selected_logical_path().as_deref(),
        Some("/catalog/scripts/jobs/deep.py")
    );
    assert!(!app.folder_focused);
}

#[test]
fn switching_scripts_ignores_a_stale_folder_response() {
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

    // Simulate a folder-browse request still in flight (e.g. "go up")
    // from the script that was selected a moment ago.
    app.dispatch_folder_request("/catalog".to_string()).unwrap();
    let stale_id = app.inflight_folder_id.unwrap();

    // The user switches to a different script before that request resolves.
    app.selected = 0;
    app.load_selected().unwrap();
    drain_until_detail_loaded(&mut app);
    assert_ne!(app.inflight_folder_id, Some(stale_id));
    let siblings_before = app.siblings.clone();
    let folder_dir_before = app.folder_dir.clone();

    // The stale response now arrives; it must be ignored rather than
    // overwriting the newly selected script's folder state.
    app.apply_folder_response(FolderResponse {
        id: stale_id,
        dir: "/catalog".to_string(),
        result: Ok(FolderListing {
            dirs: vec![],
            scripts: vec![],
        }),
    });
    assert_eq!(app.folder_dir, folder_dir_before);
    assert_eq!(app.siblings, siblings_before);
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
