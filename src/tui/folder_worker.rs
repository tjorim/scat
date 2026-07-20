use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};

use scat_core::core::db::JsonRow;
use scat_core::core::search::open_search_api;

#[derive(Debug, Clone)]
pub struct FolderRequest {
    pub id: u64,
    pub dir: String,
}

/// A folder listing: immediate subdirectory names plus the scripts directly
/// in the folder.
#[derive(Debug)]
pub struct FolderListing {
    pub dirs: Vec<String>,
    pub scripts: Vec<JsonRow>,
}

#[derive(Debug)]
pub struct FolderResponse {
    pub id: u64,
    pub dir: String,
    pub result: std::result::Result<FolderListing, String>,
}

pub struct FolderWorker {
    request_tx: Option<Sender<FolderRequest>>,
    response_rx: Receiver<FolderResponse>,
    join_handle: Option<JoinHandle<()>>,
}

impl FolderWorker {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db_path = db_path.to_path_buf();
        let (request_tx, request_rx) = mpsc::channel::<FolderRequest>();
        let (response_tx, response_rx) = mpsc::channel::<FolderResponse>();
        let join_handle = thread::Builder::new()
            .name("tui-folder-worker".to_string())
            .spawn(move || worker_loop(db_path, request_rx, response_tx))
            .context("failed to spawn TUI folder worker thread")?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            join_handle: Some(join_handle),
        })
    }

    pub fn send(&self, request: FolderRequest) -> Result<()> {
        self.request_tx
            .as_ref()
            .ok_or_else(|| anyhow!("folder worker request channel closed"))?
            .send(request)
            .map_err(|_| anyhow!("folder worker request channel closed"))
    }

    pub fn try_recv(&self) -> Result<Option<FolderResponse>> {
        match self.response_rx.try_recv() {
            Ok(response) => Ok(Some(response)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(anyhow!("folder worker channel disconnected")),
        }
    }
}

impl Drop for FolderWorker {
    fn drop(&mut self) {
        self.request_tx.take();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    db_path: PathBuf,
    request_rx: Receiver<FolderRequest>,
    response_tx: Sender<FolderResponse>,
) {
    let api = open_search_api(&db_path).map_err(|err| err.to_string());
    while let Ok(mut request) = request_rx.recv() {
        while let Ok(next) = request_rx.try_recv() {
            request = next;
        }
        let result = match &api {
            Ok(api) => api
                .scripts_in_dir(&request.dir)
                .and_then(|scripts| {
                    api.subdirs_of(&request.dir)
                        .map(|dirs| FolderListing { dirs, scripts })
                })
                .map_err(|err| err.to_string()),
            Err(err) => Err(err.clone()),
        };
        if response_tx
            .send(FolderResponse {
                id: request.id,
                dir: request.dir,
                result,
            })
            .is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{FolderRequest, FolderWorker};

    fn recv_response(worker: &FolderWorker) -> super::FolderResponse {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(response) = worker.try_recv().unwrap() {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for folder worker response"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn folder_request_lists_scripts_and_subdirs() {
        let db = super::super::make_test_db();
        let conn = rusqlite::Connection::open(db.path()).unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose)
             VALUES ('/catalog/scripts/jobs/b.py','python','print(2)','bob','')",
            [],
        )
        .unwrap();
        drop(conn);

        let worker = FolderWorker::new(db.path()).unwrap();
        worker
            .send(FolderRequest {
                id: 3,
                dir: "/catalog/scripts".to_string(),
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 3);
        assert_eq!(response.dir, "/catalog/scripts");
        let listing = response.result.unwrap();
        assert_eq!(listing.dirs, vec!["jobs"]);
        assert_eq!(listing.scripts.len(), 1);
        assert_eq!(
            listing.scripts[0]["logical_path"].as_str().unwrap(),
            "/catalog/scripts/a.py"
        );
    }

    #[test]
    fn folder_request_empty_dir_returns_empty() {
        let db = super::super::make_test_db();
        let worker = FolderWorker::new(db.path()).unwrap();
        worker
            .send(FolderRequest {
                id: 4,
                dir: String::new(),
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 4);
        let listing = response.result.unwrap();
        assert!(listing.dirs.is_empty());
        assert!(listing.scripts.is_empty());
    }
}
