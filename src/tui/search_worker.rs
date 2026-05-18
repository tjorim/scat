use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};

use scat_core::core::db::JsonRow;
use scat_core::core::search::{SearchApi, open_search_api};

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub id: u64,
    pub query: String,
    pub limit: usize,
}

#[derive(Debug)]
pub struct SearchResponse {
    pub id: u64,
    pub result: std::result::Result<Vec<JsonRow>, String>,
}

pub struct SearchWorker {
    request_tx: Option<Sender<SearchRequest>>,
    response_rx: Receiver<SearchResponse>,
    join_handle: Option<JoinHandle<()>>,
}

impl SearchWorker {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db_path = db_path.to_path_buf();
        let (request_tx, request_rx) = mpsc::channel::<SearchRequest>();
        let (response_tx, response_rx) = mpsc::channel::<SearchResponse>();
        let join_handle = thread::Builder::new()
            .name("tui-search-worker".to_string())
            .spawn(move || worker_loop(db_path, request_rx, response_tx))
            .context("failed to spawn TUI search worker thread")?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            join_handle: Some(join_handle),
        })
    }

    pub fn send(&self, request: SearchRequest) -> Result<()> {
        self.request_tx
            .as_ref()
            .ok_or_else(|| anyhow!("search worker request channel closed"))?
            .send(request)
            .map_err(|_| anyhow!("search worker request channel closed"))
    }

    pub fn try_recv(&self) -> Result<Option<SearchResponse>> {
        match self.response_rx.try_recv() {
            Ok(response) => Ok(Some(response)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(anyhow!("search worker channel disconnected")),
        }
    }
}

impl Drop for SearchWorker {
    fn drop(&mut self) {
        // Dropping the sender closes the channel, signaling the worker loop to exit.
        self.request_tx.take();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    db_path: PathBuf,
    request_rx: Receiver<SearchRequest>,
    response_tx: Sender<SearchResponse>,
) {
    let api = open_search_api(&db_path).map_err(|err| err.to_string());
    while let Ok(mut request) = request_rx.recv() {
        while let Ok(next) = request_rx.try_recv() {
            request = next;
        }
        let result = match &api {
            Ok(api) => {
                run_search(api, &request.query, request.limit).map_err(|err| err.to_string())
            }
            Err(err) => Err(err.clone()),
        };
        if response_tx
            .send(SearchResponse {
                id: request.id,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

fn run_search(api: &SearchApi, query: &str, limit: usize) -> Result<Vec<JsonRow>> {
    let query = query.trim();
    if query.is_empty() {
        return api
            .list_scripts(None, None, None, limit, 0)
            .context("failed to list scripts");
    }
    api.search(query, limit, None)
        .context("failed to run search query")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{SearchRequest, SearchWorker};
    use scat_core::core::db::{SCHEMA_VERSION, create_db};
    use tempfile::NamedTempFile;

    fn recv_response(worker: &SearchWorker) -> super::SearchResponse {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(response) = worker.try_recv().unwrap() {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for search worker response"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn make_db() -> NamedTempFile {
        let db = NamedTempFile::new().unwrap();
        let conn = create_db(db.path()).unwrap();
        conn.execute(
            "INSERT INTO index_metadata (id, build_timestamp, schema_version) VALUES (1, '2024-01-01T00:00:00', ?)",
            rusqlite::params![SCHEMA_VERSION],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose) VALUES ('/catalog/scripts/a.py','python','needle alpha','alice','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose) VALUES ('/catalog/scripts/b.py','python','beta only','bob','')",
            [],
        )
        .unwrap();
        drop(conn);
        db
    }

    #[test]
    fn empty_query_returns_all_scripts() {
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();
        worker
            .send(SearchRequest {
                id: 1,
                query: String::new(),
                limit: 200,
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 1);
        let rows = response.result.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn invalid_query_returns_error_string() {
        let db = make_db();
        let worker = SearchWorker::new(db.path()).unwrap();
        worker
            .send(SearchRequest {
                id: 7,
                query: "\"".to_string(),
                limit: 200,
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 7);
        assert!(response.result.is_err());
    }
}
