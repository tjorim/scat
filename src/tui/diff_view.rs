use scat_core::core::db::row_string as str_field;

use super::{DiffRequest, DiffResponse, Result, TuiApp};

impl TuiApp {
    /// Diff the active script against the most-recent DEVELOP checkout.
    pub(super) fn dispatch_diff(&mut self) -> Result<()> {
        self.dispatch_diff_impl(None, None)
    }

    /// Diff the active script against the revision currently selected in
    /// the Revisions pane (`checkouts[revisions_selected]`, pre-sorted so
    /// this matches what's on screen) — reaches ARCHIVE/WORKING/ROLLBACK
    /// revisions, unlike `dispatch_diff`'s fixed "most-recent DEVELOP".
    pub(super) fn dispatch_diff_against_selected_revision(&mut self) -> Result<()> {
        let Some(revision) = self.checkouts.get(self.revisions_selected) else {
            self.set_no_revision_selected();
            return Ok(());
        };
        let revision_physical_path = str_field(revision, "physical_path");
        if revision_physical_path.is_empty() {
            self.set_no_revision_selected();
            return Ok(());
        }
        let revision_type = str_field(revision, "revision_type");
        self.dispatch_diff_impl(Some(revision_physical_path), Some(revision_type))
    }

    fn set_no_revision_selected(&mut self) {
        self.detail_diff_scroll = 0;
        self.inflight_diff_id = None;
        self.detail_diff_loading = false;
        self.detail_diff_output = "No revision selected".to_string();
    }

    fn dispatch_diff_impl(
        &mut self,
        revision_physical_path: Option<String>,
        revision_type: Option<String>,
    ) -> Result<()> {
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
            revision_physical_path,
            revision_type,
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
