use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};

/// Request to verify a live-source file off the UI thread.
///
/// The resolution context (`logical_path`, `mapped`) travels with the request so
/// the response can build the final view target or a clear error without
/// re-deriving it on the main thread.
pub struct FileCheckRequest {
    pub id: u64,
    pub logical_path: String,
    pub native_path: PathBuf,
    /// Whether a path mapping was applied (`native_path != logical_path`).
    pub mapped: bool,
}

pub struct FileCheckResponse {
    pub id: u64,
    pub logical_path: String,
    pub native_path: PathBuf,
    pub mapped: bool,
    pub exists: bool,
}

/// Background worker that performs the (potentially blocking, e.g. on a slow
/// network mount) `is_file` check for the live-source viewer, keeping the TUI
/// render loop responsive.
pub struct FileCheckWorker {
    request_tx: Option<Sender<FileCheckRequest>>,
    response_rx: Receiver<FileCheckResponse>,
    join_handle: Option<JoinHandle<()>>,
}

impl FileCheckWorker {
    pub fn new() -> Result<Self> {
        let (request_tx, request_rx) = mpsc::channel::<FileCheckRequest>();
        let (response_tx, response_rx) = mpsc::channel::<FileCheckResponse>();
        let join_handle = thread::Builder::new()
            .name("tui-file-check-worker".to_string())
            .spawn(move || worker_loop(request_rx, response_tx))
            .context("failed to spawn TUI file-check worker thread")?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            join_handle: Some(join_handle),
        })
    }

    pub fn send(&self, request: FileCheckRequest) -> Result<()> {
        self.request_tx
            .as_ref()
            .ok_or_else(|| anyhow!("file-check worker request channel closed"))?
            .send(request)
            .map_err(|_| anyhow!("file-check worker request channel closed"))
    }

    pub fn try_recv(&self) -> Result<Option<FileCheckResponse>> {
        match self.response_rx.try_recv() {
            Ok(response) => Ok(Some(response)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(anyhow!("file-check worker channel disconnected"))
            }
        }
    }
}

impl Drop for FileCheckWorker {
    fn drop(&mut self) {
        self.request_tx.take();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(request_rx: Receiver<FileCheckRequest>, response_tx: Sender<FileCheckResponse>) {
    while let Ok(mut request) = request_rx.recv() {
        // Drain stale requests — only process the latest.
        while let Ok(next) = request_rx.try_recv() {
            request = next;
        }
        let exists = request.native_path.is_file();
        if response_tx
            .send(FileCheckResponse {
                id: request.id,
                logical_path: request.logical_path,
                native_path: request.native_path,
                mapped: request.mapped,
                exists,
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

    use super::{FileCheckRequest, FileCheckResponse, FileCheckWorker};

    fn recv_response(worker: &FileCheckWorker) -> FileCheckResponse {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(response) = worker.try_recv().unwrap() {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for file-check worker response"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn reports_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("foo.py");
        std::fs::write(&script, "print(1)\n").unwrap();

        let worker = FileCheckWorker::new().unwrap();
        worker
            .send(FileCheckRequest {
                id: 7,
                logical_path: "/catalog/scripts/foo.py".to_string(),
                native_path: script.clone(),
                mapped: true,
            })
            .unwrap();

        let response = recv_response(&worker);
        assert_eq!(response.id, 7);
        assert!(response.exists);
        assert_eq!(response.native_path, script);
        assert!(response.mapped);
    }

    #[test]
    fn reports_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.py");

        let worker = FileCheckWorker::new().unwrap();
        worker
            .send(FileCheckRequest {
                id: 1,
                logical_path: "/catalog/scripts/missing.py".to_string(),
                native_path: missing,
                mapped: false,
            })
            .unwrap();

        let response = recv_response(&worker);
        assert_eq!(response.id, 1);
        assert!(!response.exists);
        assert!(!response.mapped);
    }
}
