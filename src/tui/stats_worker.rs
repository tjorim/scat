use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow};

use scat_core::core::search::{StatsResult, open_search_api};

#[derive(Debug, Clone, Copy)]
pub struct StatsRequest {
    pub id: u64,
}

pub struct StatsResponse {
    pub id: u64,
    pub result: std::result::Result<StatsResult, String>,
}

/// Background worker for `scat_core`'s aggregate `stats()` query, so opening
/// the TUI's full-screen stats view never blocks the render loop on a
/// (host-local-cached, but still disk-backed) query.
pub struct StatsWorker {
    request_tx: Option<Sender<StatsRequest>>,
    response_rx: Receiver<StatsResponse>,
    join_handle: Option<JoinHandle<()>>,
}

impl StatsWorker {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db_path = db_path.to_path_buf();
        let (request_tx, request_rx) = mpsc::channel::<StatsRequest>();
        let (response_tx, response_rx) = mpsc::channel::<StatsResponse>();
        let join_handle = thread::Builder::new()
            .name("tui-stats-worker".to_string())
            .spawn(move || worker_loop(db_path, request_rx, response_tx))
            .context("failed to spawn TUI stats worker thread")?;
        Ok(Self {
            request_tx: Some(request_tx),
            response_rx,
            join_handle: Some(join_handle),
        })
    }

    pub fn send(&self, request: StatsRequest) -> Result<()> {
        self.request_tx
            .as_ref()
            .ok_or_else(|| anyhow!("stats worker request channel closed"))?
            .send(request)
            .map_err(|_| anyhow!("stats worker request channel closed"))
    }

    pub fn try_recv(&self) -> Result<Option<StatsResponse>> {
        match self.response_rx.try_recv() {
            Ok(response) => Ok(Some(response)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(anyhow!("stats worker channel disconnected")),
        }
    }
}

impl Drop for StatsWorker {
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
    request_rx: Receiver<StatsRequest>,
    response_tx: Sender<StatsResponse>,
) {
    let api = open_search_api(&db_path).map_err(|err| err.to_string());
    while let Ok(mut request) = request_rx.recv() {
        // Drain stale requests — only the latest matters, same as the other
        // one-shot workers (file-check, folder).
        while let Ok(next) = request_rx.try_recv() {
            request = next;
        }
        let result = match &api {
            Ok(api) => api.stats().map_err(|err| err.to_string()),
            Err(err) => Err(err.clone()),
        };
        if response_tx
            .send(StatsResponse {
                id: request.id,
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

    use super::{StatsRequest, StatsResponse, StatsWorker};
    use scat_core::core::db::{SCHEMA_VERSION, create_db};
    use tempfile::NamedTempFile;

    fn recv_response(worker: &StatsWorker) -> StatsResponse {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(response) = worker.try_recv().unwrap() {
                return response;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for stats worker response"
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
            "INSERT INTO scripts (logical_path, language, content, owner, purpose) VALUES ('/catalog/scripts/a.py','python','print(1)','alice','')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content, owner, purpose) VALUES ('/catalog/scripts/b.sh','shell','echo hi','bob','')",
            [],
        )
        .unwrap();
        drop(conn);
        db
    }

    #[test]
    fn reports_language_and_owner_counts() {
        let db = make_db();
        let worker = StatsWorker::new(db.path()).unwrap();
        worker.send(StatsRequest { id: 1 }).unwrap();

        let response = recv_response(&worker);
        assert_eq!(response.id, 1);
        let stats = response.result.unwrap();
        assert_eq!(stats.total_scripts, 2);
        assert!(
            stats
                .by_language
                .iter()
                .any(|l| l.language == "python" && l.count == 1)
        );
        assert!(
            stats
                .by_owner
                .iter()
                .any(|o| o.owner == "alice" && o.count == 1)
        );
    }

    #[test]
    fn only_the_latest_of_several_queued_requests_is_answered() {
        let db = make_db();
        let worker = StatsWorker::new(db.path()).unwrap();
        worker.send(StatsRequest { id: 1 }).unwrap();
        worker.send(StatsRequest { id: 2 }).unwrap();

        // The worker may already be blocked in `recv()` on the first request
        // before the second is queued, so at most two responses arrive; the
        // id we care about is the last one sent.
        let mut last = recv_response(&worker);
        while let Ok(Some(next)) = worker.try_recv() {
            last = next;
        }
        assert_eq!(last.id, 2);
        assert!(last.result.is_ok());
    }
}
