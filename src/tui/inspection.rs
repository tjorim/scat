//! Worker-backed catalog inspection. Rows retain targets separately from labels.
use super::{KeyCode, KeyEvent, TuiApp, ViewMode};
use anyhow::{Result, anyhow};
use scat_core::core::{
    script_view::ScriptView,
    search::{DepsTreeNode, SearchApi, TreeDirection, open_search_api},
};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    Similar,
    Status,
    Tree,
    Symlinks,
}
impl Kind {
    fn title(self) -> &'static str {
        match self {
            Self::Similar => "Similar scripts",
            Self::Status => "Catalog checkout status",
            Self::Tree => "Dependency trees",
            Self::Symlinks => "Symlink inspection",
        }
    }
}
#[derive(Debug)]
pub(super) struct Row {
    pub label: String,
    pub target: Option<String>,
}
pub(super) struct Page {
    kind: Kind,
    path: String,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub message: String,
    pub loading: bool,
    id: u64,
    pub limit: usize,
    pub depth: usize,
}
impl Page {
    pub(super) fn export_command(&self) -> String {
        let path = shell_words::quote(&self.path);
        match self.kind {
            Kind::Similar => format!("similar {path} --limit {} --output json", self.limit),
            Kind::Status => "status --all --output json".into(),
            Kind::Tree => format!("deps {path} --tree --depth {} --output json", self.depth),
            Kind::Symlinks => format!("symlinks {path} --output json"),
        }
    }
}
pub(super) struct Worker {
    tx: Sender<Request>,
    rx: Receiver<Response>,
    next: u64,
}
struct Request {
    id: u64,
    kind: Kind,
    path: String,
    limit: usize,
    depth: usize,
}
struct Response {
    id: u64,
    result: Result<(Vec<Row>, String), String>,
}
impl Worker {
    pub fn new(db: &Path, embeddings: &Path) -> Result<Self> {
        let (tx, requests) = mpsc::channel::<Request>();
        let (responses, rx) = mpsc::channel();
        let db = db.to_owned();
        let embeddings = embeddings.to_owned();
        std::thread::Builder::new()
            .name("tui-inspection".into())
            .spawn(move || {
                let api = open_search_api(&db).map_err(|e| e.to_string());
                while let Ok(mut request) = requests.recv() {
                    while let Ok(newer) = requests.try_recv() {
                        request = newer;
                    }
                    let result = match &api {
                        Ok(api) => load(api, &embeddings, &request).map_err(|e| e.to_string()),
                        Err(e) => Err(e.clone()),
                    };
                    if responses
                        .send(Response {
                            id: request.id,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self { tx, rx, next: 0 })
    }
}
fn target(label: String, path: &str) -> Row {
    Row {
        label,
        target: Some(path.to_owned()),
    }
}
fn tree_rows(node: &DepsTreeNode, level: usize, rows: &mut Vec<Row>) {
    let flags = format!(
        "{}{}{}{}",
        if !node.indexed { " [not indexed]" } else { "" },
        if node.cycle { " [cycle]" } else { "" },
        if node.repeated { " [repeated]" } else { "" },
        if node.truncated { " [depth limit]" } else { "" }
    );
    rows.push(Row {
        label: format!(
            "{}{} {}{flags}",
            "  ".repeat(level),
            node.via_kind.as_deref().unwrap_or("root"),
            node.logical_path
        ),
        target: node.indexed.then(|| node.logical_path.clone()),
    });
    for child in &node.children {
        tree_rows(child, level + 1, rows);
    }
}
fn load(api: &SearchApi, embeddings: &Path, r: &Request) -> Result<(Vec<Row>, String)> {
    let mut rows = Vec::new();
    let mut message = String::new();
    match r.kind {
        Kind::Similar => {
            let (results, warning) =
                crate::commands::load_similar(api, embeddings, &r.path, r.limit)?;
            message = warning.unwrap_or_default();
            rows = results
                .into_iter()
                .map(|s| {
                    target(
                        format!("{:.3}  {}", s.score, s.logical_path),
                        &s.logical_path,
                    )
                })
                .collect();
        }
        Kind::Status => {
            rows = api
                .checkout_status(None)?
                .iter()
                .map(|row| {
                    let v = ScriptView::new(row);
                    target(
                        format!(
                            "{}  {}  {}  {}",
                            v.logical_path(),
                            v.checkout_label(),
                            v.checkout_os(),
                            v.vc_warning_kinds().join(", ")
                        ),
                        v.logical_path(),
                    )
                })
                .collect();
        }
        Kind::Tree => {
            anyhow::ensure!(r.depth > 0, "Depth must be at least 1");
            for (heading, direction) in [
                ("Uses", TreeDirection::Uses),
                ("Used by", TreeDirection::UsedBy),
            ] {
                rows.push(Row {
                    label: heading.into(),
                    target: None,
                });
                let tree = api
                    .dependency_tree(&r.path, direction, r.depth)?
                    .ok_or_else(|| anyhow!("Script not found: {}", r.path))?;
                tree_rows(&tree, 0, &mut rows);
            }
        }
        Kind::Symlinks => {
            if let Some(row) = api.get_script(&r.path)? {
                let v = ScriptView::new(&row);
                let path = v.symlink_target();
                if !path.is_empty() {
                    let indexed = api.get_script(path)?.is_some();
                    rows.push(Row {
                        label: format!(
                            "Points to: {path}{}",
                            if indexed {
                                ""
                            } else {
                                " [not indexed / missing target]"
                            }
                        ),
                        target: indexed.then(|| path.to_owned()),
                    });
                }
            }
            for row in api.symlinks_to(&r.path)? {
                let path = ScriptView::new(&row).logical_path().to_owned();
                rows.push(target(format!("Pointed to by: {path}"), &path));
            }
        }
    }
    if rows.is_empty() && message.is_empty() {
        message = "No results found.".into();
    }
    Ok((rows, message))
}
impl TuiApp {
    pub(super) fn open_inspection(&mut self, kind: Kind) -> Result<()> {
        self.inspection_return = None;
        self.inspection = Some(Page {
            kind,
            path: self
                .selected_logical_path()
                .or_else(|| {
                    self.results
                        .get(self.selected)
                        .map(|row| ScriptView::new(row).logical_path().to_owned())
                })
                .unwrap_or_default(),
            rows: vec![],
            selected: 0,
            message: String::new(),
            loading: false,
            id: 0,
            limit: 10,
            depth: 5,
        });
        self.refresh_inspection()
    }
    fn refresh_inspection(&mut self) -> Result<()> {
        let Some(page) = &mut self.inspection else {
            return Ok(());
        };
        let Some(worker) = &mut self.inspection_worker else {
            page.message = "Inspection worker unavailable".into();
            return Ok(());
        };
        worker.next = worker.next.saturating_add(1);
        page.id = worker.next;
        page.loading = true;
        page.message.clear();
        page.rows.clear();
        page.selected = 0;
        worker.tx.send(Request {
            id: page.id,
            kind: page.kind,
            path: page.path.clone(),
            limit: page.limit,
            depth: page.depth,
        })?;
        Ok(())
    }
    pub(super) fn drain_inspection(&mut self) {
        let Some(worker) = &self.inspection_worker else {
            return;
        };
        loop {
            let response = match worker.rx.try_recv() {
                Ok(response) => response,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if let Some(page) = &mut self.inspection
                        && page.loading
                    {
                        page.loading = false;
                        page.message =
                            "Inspection worker disconnected; Esc returns to browsing".into();
                        self.needs_redraw = true;
                    }
                    break;
                }
            };
            let Some(page) = &mut self.inspection else {
                continue;
            };
            if page.id != response.id {
                continue;
            }
            page.loading = false;
            match response.result {
                Ok((rows, message)) => {
                    page.rows = rows;
                    page.message = message;
                }
                Err(e) => page.message = e,
            }
            self.needs_redraw = true;
        }
    }
    pub(super) fn inspection_key(&mut self, key: KeyEvent) -> Result<()> {
        let page = self.inspection.as_mut().unwrap();
        match key.code {
            KeyCode::Esc => {
                self.inspection = None;
            }
            KeyCode::Up | KeyCode::Char('k') => page.selected = page.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                page.selected = (page.selected + 1).min(page.rows.len().saturating_sub(1));
            }
            KeyCode::Home => page.selected = 0,
            KeyCode::End => page.selected = page.rows.len().saturating_sub(1),
            KeyCode::Char('+' | '-') if matches!(page.kind, Kind::Tree | Kind::Similar) => {
                let value = if page.kind == Kind::Tree {
                    &mut page.depth
                } else {
                    &mut page.limit
                };
                *value = if key.code == KeyCode::Char('+') {
                    value.saturating_add(1)
                } else {
                    value.saturating_sub(1).max(1)
                };
                self.refresh_inspection()?;
            }
            KeyCode::Char('r') => self.refresh_inspection()?,
            KeyCode::Enter => {
                if let Some(path) = page.rows.get(page.selected).and_then(|r| r.target.clone()) {
                    self.inspection_return = self.inspection.take();
                    self.inspection_return_mode = self.mode;
                    self.navigate_to_path(&path)?;
                    self.mode = ViewMode::Detail;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
pub(super) fn draw(frame: &mut ratatui::Frame<'_>, page: &Page) {
    use ratatui::{
        style::{Color, Style},
        widgets::{Block, Borders, List, ListItem, ListState},
    };
    let title = format!(
        "{} | {} | limit {} depth {}",
        page.kind.title(),
        page.path,
        page.limit,
        page.depth
    );
    let items: Vec<ListItem<'_>> = if page.loading {
        vec![ListItem::new("Loading…")]
    } else {
        let mut items = page
            .rows
            .iter()
            .map(|r| ListItem::new(r.label.as_str()))
            .collect::<Vec<_>>();
        if !page.message.is_empty() {
            items.push(ListItem::new(page.message.as_str()));
        }
        items
    };
    let mut state = ListState::default().with_selected(Some(page.selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title).title_bottom("↑/↓ move · Enter details · +/- limit/depth · r reload · Esc back · F2 select"))
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        frame.area(),
        &mut state,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(kind: Kind) -> Request {
        Request {
            id: 1,
            kind,
            path: "/catalog/scripts/a.py".into(),
            limit: 1,
            depth: 5,
        }
    }
    #[test]
    fn stale_worker_responses_do_not_replace_reopened_view() {
        let db = super::super::make_test_db();
        let mut app = super::super::tests::make_app(db.path());
        let (tx, _requests) = mpsc::channel();
        let (responses, rx) = mpsc::channel();
        app.inspection_worker = Some(Worker { tx, rx, next: 0 });
        app.open_inspection(Kind::Status).unwrap();
        let old_id = app.inspection.as_ref().unwrap().id;
        app.inspection_key(KeyEvent::new(
            KeyCode::Esc,
            super::super::KeyModifiers::NONE,
        ))
        .unwrap();
        app.open_inspection(Kind::Symlinks).unwrap();
        let id = app.inspection.as_ref().unwrap().id;
        responses
            .send(Response {
                id: old_id,
                result: Err("stale error".into()),
            })
            .unwrap();
        app.drain_inspection();
        assert!(app.inspection.as_ref().unwrap().loading);
        assert!(app.inspection.as_ref().unwrap().message.is_empty());
        responses
            .send(Response {
                id,
                result: Ok((vec![], "No links".into())),
            })
            .unwrap();
        app.drain_inspection();
        assert!(!app.inspection.as_ref().unwrap().loading);
        assert_eq!(app.inspection.as_ref().unwrap().message, "No links");
    }

    #[test]
    fn shared_fixture_matches_status_and_preserves_symlink_directions() {
        let db = super::super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute_batch("UPDATE scripts SET vc_warnings = '[{\"kind\":\"stale\"}]';
            INSERT INTO scripts (logical_path, language, symlink_target) VALUES
            ('/alias1', 'python', '/catalog/scripts/a.py'),
            ('/alias2', 'python', '/catalog/scripts/a.py');
            INSERT INTO revisions (logical_path,physical_path,revision_type,os_flavor,user,timestamp) VALUES
            ('/orphan','/DEVELOP/orphan','DEVELOP','linux','bob','2024');").unwrap();
        let api = open_search_api(db.path()).unwrap();
        let (rows, _) = load(&api, Path::new("/missing"), &request(Kind::Status)).unwrap();
        let expected = api
            .checkout_status(None)
            .unwrap()
            .iter()
            .map(|r| ScriptView::new(r).logical_path().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            rows.iter()
                .filter_map(|r| r.target.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(rows.iter().any(|r| r.label.contains("stale")));
        assert!(expected.contains(&"/orphan".to_owned()));
        let (rows, _) = load(&api, Path::new("/missing"), &request(Kind::Symlinks)).unwrap();
        assert_eq!(rows.len(), 2);
        let mut r = request(Kind::Symlinks);
        r.path = "/alias1".into();
        assert!(
            load(&api, Path::new("/missing"), &r).unwrap().0[0]
                .label
                .contains("Points to:")
        );
        conn.execute(
            "UPDATE scripts SET symlink_target='/missing' WHERE logical_path='/alias1'",
            [],
        )
        .unwrap();
        let rows = load(&api, Path::new("/missing"), &r).unwrap().0;
        assert!(rows[0].target.is_none());
        assert!(rows[0].label.contains("missing target"));
    }
    #[test]
    fn sidecar_ranking_matches_cli_and_filters_stale_before_limit() {
        let db = super::super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path,language) VALUES ('/b','python')",
            [],
        )
        .unwrap();
        let sidecar = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(sidecar.path()).unwrap();
        conn.execute_batch("CREATE TABLE script_embeddings (script_id INTEGER PRIMARY KEY, logical_path TEXT, model TEXT, dim INTEGER, embedding BLOB);").unwrap();
        for (id, path, v) in [
            (1, "/catalog/scripts/a.py", [1.0_f32, 0.0]),
            (2, "/deleted", [1.0, 0.0]),
            (3, "/b", [0.8, 0.2]),
        ] {
            let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
            conn.execute(
                "INSERT INTO script_embeddings VALUES (?1,?2,'test',2,?3)",
                rusqlite::params![id, path, bytes],
            )
            .unwrap();
        }
        let api = open_search_api(db.path()).unwrap();
        let r = request(Kind::Similar);
        let expected = crate::commands::load_similar(&api, sidecar.path(), &r.path, r.limit)
            .unwrap()
            .0;
        let rows = load(&api, sidecar.path(), &r).unwrap().0;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.as_deref(), Some("/b"));
        assert_eq!(
            rows[0].label,
            format!("{:.3}  {}", expected[0].score, expected[0].logical_path)
        );
        assert!(
            load(&api, Path::new("/missing-sidecar"), &r)
                .unwrap_err()
                .to_string()
                .contains("missing or incompatible")
        );
        let mut absent = r;
        absent.path = "/b".into();
        conn.execute("DELETE FROM script_embeddings WHERE logical_path='/b'", [])
            .unwrap();
        assert!(
            load(&api, sidecar.path(), &absent)
                .unwrap_err()
                .to_string()
                .contains("no stored embedding")
        );
    }
    #[test]
    fn tree_matches_core_flags_and_rejects_zero_depth() {
        let db = super::super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute_batch("INSERT INTO scripts (logical_path,language) VALUES ('/b','python');
        INSERT INTO dependencies (script_id,depends_on_path,resolved_script_id,kind) VALUES
        (1,'/b',2,'import'),(1,'/b',2,'referenced'),(2,'/catalog/scripts/a.py',1,'referenced'),(1,'/missing',NULL,'inputfile');").unwrap();
        let api = open_search_api(db.path()).unwrap();
        let mut r = request(Kind::Tree);
        let rows = load(&api, Path::new("/missing"), &r).unwrap().0;
        assert!(rows.iter().any(|r| r.label.contains("[cycle]")));
        assert!(rows.iter().any(|r| r.label.contains("[repeated]")));
        assert!(rows.iter().any(|r| r.label.contains("inputfile /missing [not indexed]") && r.target.is_none()));
        r.depth = 1;
        assert!(
            load(&api, Path::new("/missing"), &r)
                .unwrap()
                .0
                .iter()
                .any(|r| r.label.contains("[depth limit]"))
        );
        r.depth = 0;
        assert!(load(&api, Path::new("/missing"), &r).is_err());
    }
    #[test]
    fn empty_views_have_an_explanation() {
        let db = super::super::make_test_db();
        let api = open_search_api(db.path()).unwrap();
        for kind in [Kind::Status, Kind::Symlinks] {
            let (rows, message) = load(&api, Path::new("/missing"), &request(kind)).unwrap();
            assert!(rows.is_empty());
            assert_eq!(message, "No results found.");
        }
    }
}
