use super::*;

impl TuiApp {
    pub(super) fn xref_call_sites(&self) -> Option<&[FunctionCallSite]> {
        self.function_xref
            .as_ref()
            .and_then(|function_name| self.function_call_sites.get(function_name))
            .map(Vec::as_slice)
            .filter(|sites| !sites.is_empty())
    }

    pub(super) fn dependency_target_count(&self) -> usize {
        self.xref_call_sites()
            .map_or_else(|| self.deps.len(), |sites| sites.len())
    }

    pub(super) fn open_selected_dependency(&mut self) -> Result<()> {
        let target = if let Some(call_sites) = self.xref_call_sites() {
            call_sites
                .get(self.deps_selected)
                .map(|site| site.caller_path.clone())
        } else {
            self.deps
                .get(self.deps_selected)
                .map(|item| item.logical_path.clone())
        };

        if let Some(target) = target
            && let Some(current_path) = self.selected_logical_path()
        {
            self.dep_backstack.push(current_path);
            self.navigate_to_path(&target)?;
        }
        Ok(())
    }

    pub(super) fn jump_to_selected_function(&mut self) {
        if let Some(function) = self.functions.get(self.functions_selected) {
            self.preview_scroll = function.line.saturating_sub(1);
            self.function_xref = Some(function.name.clone());
            self.deps_selected = 0;
            self.focus = Focus::Preview;
        }
    }

    pub(super) fn navigate_to_path(&mut self, logical_path: &str) -> Result<()> {
        let target_index = self.results.iter().position(|row| {
            ScriptView::new(row)
                .logical_path_value()
                .and_then(Value::as_str)
                .is_some_and(|value| value == logical_path)
        });

        let selected = if let Some(index) = target_index {
            index
        } else {
            let mut row = JsonRow::new();
            row.insert(
                "logical_path".to_string(),
                Value::String(logical_path.to_string()),
            );
            self.results.push(row);
            self.results.len().saturating_sub(1)
        };
        self.selected = selected;
        self.load_selected()
    }

    pub(super) fn selected_logical_path(&self) -> Option<String> {
        self.detail
            .as_ref()
            .map(ScriptView::new)
            .and_then(|view| view.logical_path_value())
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.results
                    .get(self.selected)
                    .map(ScriptView::new)
                    .and_then(|view| view.logical_path_value())
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned)
            })
    }
}
