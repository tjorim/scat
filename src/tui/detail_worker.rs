use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};

use scat_core::core::db::JsonRow;
use scat_core::core::search::{SearchApi, open_search_api};

#[derive(Debug, Clone)]
pub struct DetailRequest {
    pub id: u64,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct DependencyItem {
    pub kind: String,
    pub logical_path: String,
}

#[derive(Debug, Clone)]
pub struct FunctionItem {
    pub name: String,
    pub kind: String,
    pub line: u16,
    pub docstring: String,
}

#[derive(Debug, Clone)]
pub struct FunctionCallSite {
    pub caller_path: String,
    pub line: u16,
    pub caller: String,
    pub callee: String,
}

#[derive(Debug)]
pub struct DetailPayload {
    pub detail: Option<JsonRow>,
    pub contributors: Vec<String>,
    pub deps: Vec<DependencyItem>,
    pub functions: Vec<FunctionItem>,
    pub function_call_sites: BTreeMap<String, Vec<FunctionCallSite>>,
    pub checkouts: Vec<JsonRow>,
    pub cached_preview: String,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct DetailResponse {
    pub id: u64,
    pub payload: DetailPayload,
}

pub struct DetailWorker {
    request_tx: Option<Sender<DetailRequest>>,
    response_rx: Receiver<DetailResponse>,
    join_handle: Option<JoinHandle<()>>,
}

impl DetailWorker {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db_path = db_path.to_path_buf();
        let (request_tx, request_rx) = mpsc::channel::<DetailRequest>();
        let (response_tx, response_rx) = mpsc::channel::<DetailResponse>();
        let join_handle = thread::Builder::new()
            .name("tui-detail-worker".to_string())
            .spawn(move || worker_loop(db_path, request_rx, response_tx))
            .context("failed to spawn TUI detail worker thread")?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            join_handle: Some(join_handle),
        })
    }

    pub fn send(&self, request: DetailRequest) -> Result<()> {
        self.request_tx
            .as_ref()
            .ok_or_else(|| anyhow!("detail worker request channel closed"))?
            .send(request)
            .map_err(|_| anyhow!("detail worker request channel closed"))
    }

    pub fn try_recv(&self) -> Result<Option<DetailResponse>> {
        match self.response_rx.try_recv() {
            Ok(response) => Ok(Some(response)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(anyhow!("detail worker channel disconnected")),
        }
    }
}

impl Drop for DetailWorker {
    fn drop(&mut self) {
        self.request_tx.take();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    db_path: PathBuf,
    request_rx: Receiver<DetailRequest>,
    response_tx: Sender<DetailResponse>,
) {
    let api = open_search_api(&db_path).map_err(|err| err.to_string());
    while let Ok(mut request) = request_rx.recv() {
        while let Ok(next) = request_rx.try_recv() {
            request = next;
        }
        let payload = match &api {
            Ok(api) => load_detail(api, &request.path),
            Err(err) => DetailPayload {
                detail: None,
                contributors: vec![],
                deps: vec![],
                functions: vec![],
                function_call_sites: BTreeMap::new(),
                checkouts: vec![],
                cached_preview: String::new(),
                error: Some(err.clone()),
            },
        };
        if response_tx
            .send(DetailResponse {
                id: request.id,
                payload,
            })
            .is_err()
        {
            break;
        }
    }
}

/// Return the unique, sorted contributors for a script row (owner + history authors).
fn contributors_from_row(row: &JsonRow) -> Vec<String> {
    crate::output::contributors_from_script(row)
}

fn load_detail(api: &SearchApi, path: &str) -> DetailPayload {
    let mut result = DetailPayload {
        detail: None,
        contributors: vec![],
        deps: vec![],
        functions: vec![],
        function_call_sites: BTreeMap::new(),
        checkouts: vec![],
        cached_preview: String::new(),
        error: None,
    };

    match api.get_script(path) {
        Ok(d) => {
            result.contributors = d.as_ref().map(contributors_from_row).unwrap_or_default();
            result.detail = d;
        }
        Err(e) => {
            result.error = Some(e.to_string());
            return result;
        }
    }

    let graph = match api.dependency_graph(path) {
        Ok(g) => Some(g),
        Err(e) => {
            result.error = Some(e.to_string());
            None
        }
    };

    result.checkouts = match api.revisions_for(path) {
        Ok(c) => c,
        Err(e) => {
            if result.error.is_none() {
                result.error = Some(e.to_string());
            }
            vec![]
        }
    };

    let mut dep_seen = BTreeSet::new();
    let mut deps = Vec::new();
    if let Some(g) = graph {
        for item in g.uses {
            let key = ("uses".to_string(), item.logical_path.clone());
            if dep_seen.insert(key.clone()) {
                deps.push(DependencyItem {
                    kind: key.0,
                    logical_path: key.1,
                });
            }
        }
        for item in g.used_by {
            let lp = super::str_field(&item, "logical_path");
            if lp.is_empty() {
                continue;
            }
            let key = ("used by".to_string(), lp);
            if dep_seen.insert(key.clone()) {
                deps.push(DependencyItem {
                    kind: key.0,
                    logical_path: key.1,
                });
            }
        }
    }
    result.deps = deps;

    result.functions = match api.get_functions_defined_in(path) {
        Ok(rows) => rows
            .into_iter()
            .map(|row| {
                let raw_line = row
                    .get("line")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(1);
                let line = u16::try_from(raw_line.max(1)).unwrap_or(u16::MAX);
                let docstring = super::str_field(&row, "docstring")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                FunctionItem {
                    name: super::str_field(&row, "name"),
                    kind: super::str_field(&row, "kind"),
                    line,
                    docstring,
                }
            })
            .collect(),
        Err(e) => {
            if result.error.is_none() {
                result.error = Some(e.to_string());
            }
            vec![]
        }
    };

    let mut call_sites = BTreeMap::new();
    for function in &result.functions {
        call_sites
            .entry(function.name.clone())
            .or_insert_with(Vec::new);
    }
    match api.get_callers_of_functions_in(path) {
        Ok(rows) => {
            for row in rows {
                let function_name = super::str_field(&row, "target_function");
                if function_name.is_empty() {
                    continue;
                }
                let raw_line = row
                    .get("line")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(1);
                let line = u16::try_from(raw_line.max(1)).unwrap_or(u16::MAX);
                call_sites
                    .entry(function_name)
                    .or_insert_with(Vec::new)
                    .push(FunctionCallSite {
                        caller_path: super::str_field(&row, "logical_path"),
                        line,
                        caller: super::str_field(&row, "caller"),
                        callee: super::str_field(&row, "callee"),
                    });
            }
        }
        Err(e) => {
            if result.error.is_none() {
                result.error = Some(e.to_string());
            }
        }
    }
    result.function_call_sites = call_sites;

    let content = result
        .detail
        .as_ref()
        .map(|row| super::str_field(row, "content"))
        .unwrap_or_default();
    result.cached_preview = if content.is_empty() {
        "No content indexed.".to_string()
    } else {
        content
            .lines()
            .take(super::PREVIEW_LINES)
            .collect::<Vec<_>>()
            .join("\n")
    };

    result
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{DetailRequest, DetailWorker};

    fn recv_response(worker: &DetailWorker) -> super::DetailResponse {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(response) = worker.try_recv().unwrap() {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for detail worker response"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn detail_request_returns_payload() {
        let db = super::super::make_test_db();
        let worker = DetailWorker::new(db.path()).unwrap();
        worker
            .send(DetailRequest {
                id: 5,
                path: "/catalog/scripts/a.py".to_string(),
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 5);
        assert!(response.payload.detail.is_some());
        assert_eq!(response.payload.error, None);
    }

    #[test]
    fn detail_request_precomputes_contributors() {
        let db = super::super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "UPDATE scripts SET metadata_json = ?1 WHERE logical_path = '/catalog/scripts/a.py'",
            [r#"{"history_entries":[{"author":"bob"},{"author":"alice"}]}"#],
        )
        .unwrap();
        drop(conn);

        let worker = DetailWorker::new(db.path()).unwrap();
        worker
            .send(DetailRequest {
                id: 6,
                path: "/catalog/scripts/a.py".to_string(),
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 6);
        assert_eq!(response.payload.contributors, vec!["alice", "bob"]);
    }

    #[test]
    fn missing_script_returns_empty_detail() {
        let db = super::super::make_test_db();
        let worker = DetailWorker::new(db.path()).unwrap();
        worker
            .send(DetailRequest {
                id: 8,
                path: "/catalog/scripts/missing.py".to_string(),
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 8);
        assert!(response.payload.detail.is_none());
        assert!(response.payload.error.is_none());
    }
}
