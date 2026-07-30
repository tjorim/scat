use super::{DiffRequest, DiffResponse, Result, TuiApp};

impl TuiApp {
    pub(super) fn dispatch_diff(&mut self) -> Result<()> {
        self.detail_diff_scroll = 0;
        let Some(logical_path) = self.selected_logical_path() else {
            self.detail_diff_output = "No script selected.".to_string();
            self.detail_diff_loading = false;
            return Ok(());
        };
        let id = self.next_diff_id;
        self.next_diff_id = self.next_diff_id.saturating_add(1);
        self.detail_diff_loading = true;
        self.detail_diff_output = String::new();
        self.inflight_diff_id = Some(id);
        self.diff_worker.send(DiffRequest {
            id,
            path: logical_path,
        })?;
        Ok(())
    }

    pub(super) fn drain_diff_channel(&mut self) {
        loop {
            match self.diff_worker.try_recv() {
                Ok(Some(response)) => {
                    self.apply_diff_response(response);
                    self.needs_redraw = true;
                }
                Ok(None) => break,
                Err(_) => {
                    self.inflight_diff_id = None;
                    self.detail_diff_loading = false;
                    self.detail_diff_output = "Diff worker disconnected unexpectedly.".to_string();
                    self.needs_redraw = true;
                    break;
                }
            }
        }
    }

    pub(super) fn apply_diff_response(&mut self, response: DiffResponse) {
        if Some(response.id) != self.inflight_diff_id {
            return;
        }
        self.inflight_diff_id = None;
        self.detail_diff_loading = false;
        self.detail_diff_output = response.output;
    }
}
