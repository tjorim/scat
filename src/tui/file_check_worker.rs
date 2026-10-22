use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
    /// The `scripts.language` column, carried through so the viewer can hint
    /// the external editor's syntax highlighting for extensionless scripts.
    pub language: String,
}

pub struct FileCheckResponse {
    pub id: u64,
    pub logical_path: String,
    pub native_path: PathBuf,
    pub mapped: bool,
    pub exists: bool,
    pub error: Option<String>,
    pub language: String,
}

/// Background worker that performs the (potentially blocking, e.g. on a slow
/// network mount) `is_file` check for the live-source viewer, keeping the TUI
/// render loop responsive. Each stat runs in an isolated subprocess with a
/// six-second deadline; shutdown cancels it before joining the worker.
pub struct FileCheckWorker {
    request_tx: Option<Sender<FileCheckRequest>>,
    response_rx: Receiver<FileCheckResponse>,
    join_handle: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
}

impl FileCheckWorker {
    pub fn new() -> Result<Self> {
        let executable = std::env::current_exe()?;
        Self::with_probe(
            move |path| {
                let mut command = Command::new(&executable);
                command.arg("file-check").arg("--").arg(path);
                command
            },
            Duration::from_secs(6),
        )
    }

    pub(super) fn with_probe(
        probe: impl Fn(&Path) -> Command + Send + 'static,
        timeout: Duration,
    ) -> Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (request_tx, request_rx) = mpsc::channel::<FileCheckRequest>();
        let (response_tx, response_rx) = mpsc::channel::<FileCheckResponse>();
        let join_handle = thread::Builder::new()
            .name("tui-file-check-worker".to_string())
            .spawn(move || worker_loop(request_rx, response_tx, probe, timeout, worker_cancel))
            .context("failed to spawn TUI file-check worker thread")?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            join_handle: Some(join_handle),
            cancel,
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
        self.cancel.store(true, Ordering::Relaxed);
        self.request_tx.take();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    request_rx: Receiver<FileCheckRequest>,
    response_tx: Sender<FileCheckResponse>,
    probe: impl Fn(&Path) -> Command,
    timeout: Duration,
    cancel: Arc<AtomicBool>,
) {
    while let Ok(mut request) = request_rx.recv() {
        // Drain stale requests — only process the latest.
        while let Ok(next) = request_rx.try_recv() {
            request = next;
        }
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let result =
            crate::subprocess::capture(&mut probe(&request.native_path), &cancel, Some(timeout))
                .and_then(|output| {
                    anyhow::ensure!(output.status.success(), "File check failed");
                    match output.stdout.as_slice() {
                        b"true\n" => Ok(true),
                        b"false\n" => Ok(false),
                        _ => Err(anyhow!("Invalid file-check response")),
                    }
                });
        let (exists, error) = match result {
            Ok(exists) => (exists, None),
            Err(err) => (false, Some(err.to_string())),
        };
        if response_tx
            .send(FileCheckResponse {
                id: request.id,
                logical_path: request.logical_path,
                native_path: request.native_path,
                mapped: request.mapped,
                exists,
                error,
                language: request.language,
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

        let worker = FileCheckWorker::with_probe(
            |path| {
                let mut cmd = std::process::Command::new("/bin/sh");
                cmd.args([
                    "-c",
                    "if test -f \"$1\"; then echo true; else echo false; fi",
                    "probe",
                ])
                .arg(path);
                cmd
            },
            Duration::from_secs(2),
        )
        .unwrap();
        worker
            .send(FileCheckRequest {
                id: 7,
                logical_path: "/catalog/scripts/foo.py".to_string(),
                native_path: script.clone(),
                mapped: true,
                language: "python".to_string(),
            })
            .unwrap();

        let response = recv_response(&worker);
        assert_eq!(response.id, 7);
        assert!(response.exists);
        assert_eq!(response.native_path, script);
        assert!(response.mapped);
        assert_eq!(response.language, "python");
    }

    #[test]
    fn reports_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.py");

        let worker = FileCheckWorker::with_probe(
            |path| {
                let mut cmd = std::process::Command::new("/bin/sh");
                cmd.args([
                    "-c",
                    "if test -f \"$1\"; then echo true; else echo false; fi",
                    "probe",
                ])
                .arg(path);
                cmd
            },
            Duration::from_secs(2),
        )
        .unwrap();
        worker
            .send(FileCheckRequest {
                id: 1,
                logical_path: "/catalog/scripts/missing.py".to_string(),
                native_path: missing,
                mapped: false,
                language: "python".to_string(),
            })
            .unwrap();

        let response = recv_response(&worker);
        assert_eq!(response.id, 1);
        assert!(!response.exists);
        assert!(!response.mapped);
    }
    #[test]
    fn slow_probe_times_out_and_worker_shutdown_cancels_inflight_check() {
        for timeout in [Duration::from_millis(40), Duration::from_secs(60)] {
            let dir = tempfile::tempdir().unwrap();
            let ready = dir.path().join("ready");
            let marker = ready.clone();
            let worker = FileCheckWorker::with_probe(
                move |_| {
                    let mut command = std::process::Command::new("/bin/sh");
                    command
                        .args(["-c", "touch \"$1\"; exec sleep 60", "probe"])
                        .arg(&marker);
                    command
                },
                timeout,
            )
            .unwrap();
            worker
                .send(FileCheckRequest {
                    id: 1,
                    logical_path: "/slow".into(),
                    native_path: "/slow".into(),
                    mapped: false,
                    language: "shell".into(),
                })
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ready.exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            if timeout < Duration::from_secs(1) {
                let response = recv_response(&worker);
                assert!(!response.exists);
                assert!(response.error.unwrap().contains("timed out"));
            }
            let start = Instant::now();
            drop(worker);
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }
}
