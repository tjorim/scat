use super::{FileCheckRequest, FileCheckResponse, PathBuf, Result, ScriptView, TuiApp, viewer};

impl TuiApp {
    pub(super) fn queue_catalog_view(&mut self) {
        match self.catalog_view_target() {
            Ok(target) => {
                self.error = None;
                self.pending_view = Some(viewer::ViewTarget::Catalog(target));
            }
            Err(err) => {
                self.error = Some(err.to_string());
            }
        }
    }

    pub(super) fn queue_live_source_view(&mut self) {
        if let Err(err) = self.dispatch_live_source_check() {
            self.error = Some(err.to_string());
        }
    }

    /// Validate the selection and resolve the native path, then hand the
    /// (potentially blocking) file-existence check to the background worker.
    /// The view target or a clear error is produced from the worker response in
    /// [`Self::apply_file_check_response`].
    pub(super) fn dispatch_live_source_check(&mut self) -> Result<()> {
        if self.detail_loading {
            anyhow::bail!("Script is still loading.");
        }
        let Some(row) = self.detail.as_ref() else {
            anyhow::bail!("No script selected.");
        };
        let logical_path = ScriptView::new(row).logical_path().to_string();
        if logical_path.is_empty() {
            anyhow::bail!("Selected script has no logical path.");
        }
        let native = self.resolver.to_native(&logical_path);
        let mapped = native != logical_path;
        let native_path = PathBuf::from(native);

        let id = self.next_filecheck_id;
        self.next_filecheck_id = self.next_filecheck_id.saturating_add(1);
        self.inflight_filecheck_id = Some(id);
        self.error = None;
        self.file_check_worker.send(FileCheckRequest {
            id,
            logical_path,
            native_path,
            mapped,
        })
    }

    pub(super) fn drain_file_check_channel(&mut self) {
        loop {
            match self.file_check_worker.try_recv() {
                Ok(Some(response)) => {
                    self.apply_file_check_response(response);
                    self.needs_redraw = true;
                }
                Ok(None) => break,
                Err(_) => {
                    self.inflight_filecheck_id = None;
                    self.error = Some("File-check worker disconnected unexpectedly.".to_string());
                    self.needs_redraw = true;
                    break;
                }
            }
        }
    }

    pub(super) fn apply_file_check_response(&mut self, response: FileCheckResponse) {
        if Some(response.id) != self.inflight_filecheck_id {
            return;
        }
        self.inflight_filecheck_id = None;
        if response.exists {
            // The file may exist at the logical path itself (e.g. running scat
            // on the host where the catalog was built, where logical paths are
            // real filesystem paths), so a missing mapping is not an error here.
            self.error = None;
            self.pending_view = Some(viewer::ViewTarget::LiveSource {
                logical_path: response.logical_path,
                native_path: response.native_path,
            });
        } else if response.mapped {
            self.error = Some(format!(
                "Live source not found at {}",
                response.native_path.display()
            ));
        } else {
            self.error = Some(format!(
                "No filesystem mapping for {}, and no file exists at that path; configure a path mapping to open the live source.",
                response.logical_path
            ));
        }
    }

    pub(super) fn catalog_view_target(&self) -> Result<viewer::CatalogView> {
        if self.detail_loading {
            anyhow::bail!("Script is still loading.");
        }
        let Some(row) = self.detail.as_ref() else {
            anyhow::bail!("No script selected.");
        };
        let view = ScriptView::new(row);
        let logical_path = view.logical_path().to_string();
        if logical_path.is_empty() {
            anyhow::bail!("Selected script has no logical path.");
        }
        Ok(viewer::CatalogView {
            logical_path,
            content: view.content().to_string(),
        })
    }
}
