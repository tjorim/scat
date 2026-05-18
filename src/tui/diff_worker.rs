use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};

use scat_core::core::db::open_db;
use scat_core::core::diff::{diff_catalog_vs_checkout, render_diff_text};

pub struct DiffRequest {
    pub id: u64,
    pub path: String,
}

pub struct DiffResponse {
    pub id: u64,
    pub output: String,
}

pub struct DiffWorker {
    request_tx: Option<Sender<DiffRequest>>,
    response_rx: Receiver<DiffResponse>,
    join_handle: Option<JoinHandle<()>>,
}

impl DiffWorker {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db_path = db_path.to_path_buf();
        let (request_tx, request_rx) = mpsc::channel::<DiffRequest>();
        let (response_tx, response_rx) = mpsc::channel::<DiffResponse>();
        let join_handle = thread::Builder::new()
            .name("tui-diff-worker".to_string())
            .spawn(move || worker_loop(db_path, request_rx, response_tx))
            .context("failed to spawn TUI diff worker thread")?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            join_handle: Some(join_handle),
        })
    }

    pub fn send(&self, request: DiffRequest) -> Result<()> {
        self.request_tx
            .as_ref()
            .ok_or_else(|| anyhow!("diff worker request channel closed"))?
            .send(request)
            .map_err(|_| anyhow!("diff worker request channel closed"))
    }

    pub fn try_recv(&self) -> Result<Option<DiffResponse>> {
        match self.response_rx.try_recv() {
            Ok(response) => Ok(Some(response)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(anyhow!("diff worker channel disconnected")),
        }
    }
}

impl Drop for DiffWorker {
    fn drop(&mut self) {
        self.request_tx.take();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    db_path: PathBuf,
    request_rx: Receiver<DiffRequest>,
    response_tx: Sender<DiffResponse>,
) {
    while let Ok(mut request) = request_rx.recv() {
        // Drain stale requests — only process the latest.
        while let Ok(next) = request_rx.try_recv() {
            request = next;
        }
        let output = compute_diff(&db_path, &request.path);
        if response_tx
            .send(DiffResponse {
                id: request.id,
                output,
            })
            .is_err()
        {
            break;
        }
    }
}

fn compute_diff(db_path: &Path, logical_path: &str) -> String {
    match open_db(db_path)
        .map_err(anyhow::Error::from)
        .and_then(|conn| {
            diff_catalog_vs_checkout(&conn, logical_path)
                .map(|result| render_diff_text(&result))
                .map_err(anyhow::Error::from)
        }) {
        Ok(text) => text,
        Err(err) => format!("Diff unavailable for '{logical_path}'.\n\n{err}"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{DiffRequest, DiffWorker};

    fn recv_response(worker: &DiffWorker) -> super::DiffResponse {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(response) = worker.try_recv().unwrap() {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for diff worker response"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn diff_worker_returns_error_text_for_missing_script() {
        let db = super::super::make_test_db();
        let worker = DiffWorker::new(db.path()).unwrap();
        worker
            .send(DiffRequest {
                id: 1,
                path: "/catalog/scripts/missing.py".to_string(),
            })
            .unwrap();
        let response = recv_response(&worker);
        assert_eq!(response.id, 1);
        assert!(
            response.output.contains("Diff unavailable"),
            "expected error text, got: {}",
            response.output
        );
    }
}
