use super::{
    FolderListing, FolderRequest, FolderResponse, Result, ScriptView, TuiApp, logical_parent_dir,
};

impl TuiApp {
    pub(super) fn drain_folder_channel(&mut self) {
        loop {
            match self.folder_worker.try_recv() {
                Ok(Some(response)) => {
                    self.apply_folder_response(response);
                    self.needs_redraw = true;
                }
                Ok(None) => break,
                Err(_) => {
                    self.inflight_folder_id = None;
                    self.error = Some("Folder worker disconnected unexpectedly".to_string());
                    self.needs_redraw = true;
                    break;
                }
            }
        }
    }

    pub(super) fn apply_folder_response(&mut self, response: FolderResponse) {
        if Some(response.id) != self.inflight_folder_id {
            return;
        }
        self.inflight_folder_id = None;
        match response.result {
            Ok(FolderListing { dirs, scripts }) => {
                self.error = None;
                self.folder_dir = Some(response.dir);
                self.sibling_dirs = dirs;
                self.siblings = scripts;
                self.siblings_selected = 0;
            }
            Err(err) => self.error = Some(err),
        }
    }

    /// The Folder section's currently displayed directory: the browsed
    /// override when set, otherwise the selected script's own parent.
    pub(super) fn folder_display_dir(&self) -> String {
        self.folder_dir.clone().unwrap_or_else(|| {
            self.detail
                .as_ref()
                .map(|row| ScriptView::new(row).parent_dir().to_string())
                .unwrap_or_default()
        })
    }

    /// Whether the Folder section has anything to browse (Tab only takes
    /// effect when the selected script has a parent directory).
    pub(super) fn can_focus_folder(&self) -> bool {
        !self.folder_display_dir().is_empty()
    }

    pub(super) fn dispatch_folder_request(&mut self, dir: String) -> Result<()> {
        let id = self.next_folder_id;
        self.next_folder_id = self.next_folder_id.saturating_add(1);
        self.inflight_folder_id = Some(id);
        self.folder_worker.send(FolderRequest { id, dir })?;
        Ok(())
    }

    /// Browse up to the parent of the currently displayed folder. A no-op at
    /// the root (`/`), which has no parent to browse into.
    pub(super) fn folder_go_up(&mut self) -> Result<()> {
        let current = self.folder_display_dir();
        if current.is_empty() || current == "/" {
            return Ok(());
        }
        let up_dir = logical_parent_dir(&current).to_string();
        self.dispatch_folder_request(up_dir)
    }

    /// Total entries in the Folder browse list: subdirectories first, then
    /// sibling scripts, sharing one selection index.
    pub(super) fn folder_entry_count(&self) -> usize {
        self.sibling_dirs.len() + self.siblings.len()
    }

    /// Open the currently highlighted Folder entry: descend into it when it
    /// is a subdirectory, otherwise jump the detail view to the sibling
    /// script (pushing the current script onto the folder backstack so it
    /// can be revisited).
    pub(super) fn open_selected_folder_entry(&mut self) -> Result<()> {
        let idx = self.siblings_selected;
        if let Some(name) = self.sibling_dirs.get(idx) {
            let dir = self.folder_display_dir();
            let child = if dir == "/" {
                format!("/{name}")
            } else {
                format!("{dir}/{name}")
            };
            return self.dispatch_folder_request(child);
        }

        let Some(target) = self
            .siblings
            .get(idx - self.sibling_dirs.len())
            .map(|row| ScriptView::new(row).logical_path().to_string())
            .filter(|path| !path.is_empty())
        else {
            return Ok(());
        };
        if let Some(current_path) = self.selected_logical_path() {
            self.folder_backstack.push(current_path);
        }
        self.folder_focused = false;
        self.navigate_to_path(&target)
    }
}
