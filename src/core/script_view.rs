//! Typed, read-only view over a queried `scripts` table row.
//!
//! Most renderers (`src/output.rs`, the TUI detail/metadata panes, `src/core::diff`)
//! operate on a [`JsonRow`] and re-parse the same well-known `scripts` columns by
//! hand — `logical_path`, `language`, `tags`, `entry_points`, `related`,
//! `metadata_json`, `vc_warnings`, the `checkout_*` fields, and so on. That spread
//! the column-name string literals across the binary and let the JSON, CSV, table,
//! and TUI renderers drift in how they extract and fall back on each field.
//!
//! [`ScriptView`] wraps a single `scripts` row and exposes typed accessors for the
//! known columns plus parsed helpers for the JSON-encoded list/metadata fields, so
//! the renderers share one definition of each column's name and parsing semantics.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::core::db::{JsonRow, row_str};

/// Directory portion of a logical path (everything before the last `/`).
///
/// A top-level path like `/foo.py` yields `/`; a bare name with no `/` yields
/// `""`.
pub fn logical_parent_dir(logical_path: &str) -> &str {
    match logical_path.rfind('/') {
        Some(0) => "/",
        Some(idx) => &logical_path[..idx],
        None => "",
    }
}

/// How a symlink's target should be written next to the script that points at
/// it: bare filename for a target in the same directory, full logical path for
/// one anywhere else.
///
/// vc's active-version symlinks point at a sibling `<script>_<timestamp>` file,
/// so repeating the shared directory on every arrow costs the width that the
/// interesting part — which version is live — needs, and in a narrow terminal
/// or results pane pushes it out of view entirely. A target somewhere else
/// keeps its full path, where the location *is* the point.
pub fn symlink_target_display<'a>(logical_path: &str, symlink_target: &'a str) -> &'a str {
    let same_dir = logical_parent_dir(logical_path) == logical_parent_dir(symlink_target);
    match symlink_target.rsplit_once('/') {
        Some((_, name)) if same_dir && !name.is_empty() => name,
        _ => symlink_target,
    }
}

/// JSON-encoded list columns of the `scripts` table.
///
/// Each stores a JSON array string (for example `["ops","nightly"]`) that the
/// renderers parse on demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListField {
    /// The `tags` column.
    Tags,
    /// The `entry_points` column.
    EntryPoints,
    /// The `related` column.
    Related,
}

impl ListField {
    /// The underlying `scripts` column name.
    pub const fn column(self) -> &'static str {
        match self {
            ListField::Tags => "tags",
            ListField::EntryPoints => "entry_points",
            ListField::Related => "related",
        }
    }
}

/// A typed, read-only view over a queried `scripts` table row.
///
/// The view borrows the underlying [`JsonRow`] and never copies it; accessors
/// return borrowed string slices or freshly parsed values. Missing, null, and
/// wrong-typed columns fall back to empty/`None` exactly as the previous raw
/// `JsonRow` lookups did, so output stays byte-identical.
#[derive(Clone, Copy)]
pub struct ScriptView<'a> {
    row: &'a JsonRow,
}

impl<'a> ScriptView<'a> {
    /// Wrap a queried `scripts` row.
    pub fn new(row: &'a JsonRow) -> Self {
        Self { row }
    }

    /// The underlying raw row.
    pub fn row(&self) -> &'a JsonRow {
        self.row
    }

    // -- text columns (empty string when missing, null, or non-string) --

    /// The `logical_path` column.
    pub fn logical_path(&self) -> &'a str {
        row_str(self.row, "logical_path")
    }

    /// The `language` column.
    pub fn language(&self) -> &'a str {
        row_str(self.row, "language")
    }

    /// The `owner` column.
    pub fn owner(&self) -> &'a str {
        row_str(self.row, "owner")
    }

    /// The `purpose` column.
    pub fn purpose(&self) -> &'a str {
        row_str(self.row, "purpose")
    }

    /// The `symlink_target` column.
    pub fn symlink_target(&self) -> &'a str {
        row_str(self.row, "symlink_target")
    }

    /// The directory portion of `logical_path` (see [`logical_parent_dir`]).
    pub fn parent_dir(&self) -> &'a str {
        logical_parent_dir(self.logical_path())
    }

    /// The `indexed_at` column.
    pub fn indexed_at(&self) -> &'a str {
        row_str(self.row, "indexed_at")
    }

    /// The `content` column.
    pub fn content(&self) -> &'a str {
        row_str(self.row, "content")
    }

    /// The `checkout_user` column.
    pub fn checkout_user(&self) -> &'a str {
        row_str(self.row, "checkout_user")
    }

    /// The `checkout_timestamp` column.
    pub fn checkout_timestamp(&self) -> &'a str {
        row_str(self.row, "checkout_timestamp")
    }

    /// The `checkout_os` column.
    pub fn checkout_os(&self) -> &'a str {
        row_str(self.row, "checkout_os")
    }

    // -- numeric columns --

    /// The `size` column as a signed integer, when present and numeric.
    pub fn size(&self) -> Option<i64> {
        self.row.get("size").and_then(Value::as_i64)
    }

    /// The `mtime` column as a floating-point Unix timestamp, when present and numeric.
    pub fn mtime(&self) -> Option<f64> {
        self.row.get("mtime").and_then(Value::as_f64)
    }

    // -- raw JSON values (borrowed; `None` when the column is absent) --

    /// Raw `logical_path` value.
    pub fn logical_path_value(&self) -> Option<&'a Value> {
        self.row.get("logical_path")
    }

    /// Raw `language` value.
    pub fn language_value(&self) -> Option<&'a Value> {
        self.row.get("language")
    }

    /// Raw `owner` value.
    pub fn owner_value(&self) -> Option<&'a Value> {
        self.row.get("owner")
    }

    /// Raw `purpose` value.
    pub fn purpose_value(&self) -> Option<&'a Value> {
        self.row.get("purpose")
    }

    /// Raw `size` value.
    pub fn size_value(&self) -> Option<&'a Value> {
        self.row.get("size")
    }

    /// Raw `mtime` value.
    pub fn mtime_value(&self) -> Option<&'a Value> {
        self.row.get("mtime")
    }

    /// Raw `indexed_at` value.
    pub fn indexed_at_value(&self) -> Option<&'a Value> {
        self.row.get("indexed_at")
    }

    /// Raw `symlink_target` value.
    pub fn symlink_target_value(&self) -> Option<&'a Value> {
        self.row.get("symlink_target")
    }

    // -- JSON-encoded list columns --

    /// Parse a list column into its string elements.
    ///
    /// Returns an empty vector when the column is missing or does not hold a
    /// JSON array of strings.
    pub fn string_list(&self, field: ListField) -> Vec<String> {
        self.row
            .get(field.column())
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
            .unwrap_or_default()
    }

    /// Parse a list column into a JSON array value, falling back to an empty
    /// array when the column is missing, unparseable, or not an array.
    ///
    /// A list field always serializes as a list — JSON callers share this so the
    /// `show`, `search`, and `diff` outputs agree on `[]` for an absent list.
    pub fn list_value_or_empty(&self, field: ListField) -> Value {
        self.row
            .get(field.column())
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .filter(Value::is_array)
            .unwrap_or_else(|| Value::Array(Vec::new()))
    }

    // -- derived / parsed helpers --

    /// A human-readable checkout label for the row's `checkout_*` columns.
    ///
    /// Yields `clean` when no user holds the checkout, otherwise
    /// `checked out by <user>` with ` since <timestamp>` appended when a
    /// timestamp is recorded.
    pub fn checkout_label(&self) -> String {
        let user = self.checkout_user();
        if user.is_empty() {
            return "clean".to_string();
        }
        let timestamp = self.checkout_timestamp();
        if timestamp.is_empty() {
            format!("checked out by {user}")
        } else {
            format!("checked out by {user} since {timestamp}")
        }
    }

    /// Parse the `metadata_json` column into its object map.
    ///
    /// Returns an empty map when the column is missing or does not hold a JSON
    /// object.
    pub fn metadata(&self) -> serde_json::Map<String, Value> {
        self.row
            .get("metadata_json")
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    }

    /// Parse the `vc_warnings` column into the list of warning objects.
    fn vc_warnings(&self) -> Vec<Value> {
        match serde_json::from_str::<Value>(row_str(self.row, "vc_warnings")) {
            Ok(Value::Array(warnings)) => warnings,
            _ => Vec::new(),
        }
    }

    /// The `message` field of each recorded vc warning.
    pub fn vc_warning_messages(&self) -> Vec<String> {
        self.vc_warnings()
            .iter()
            .filter_map(|warning| warning.get("message").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    /// The `kind` field of each recorded vc warning.
    pub fn vc_warning_kinds(&self) -> Vec<String> {
        self.vc_warnings()
            .iter()
            .filter_map(|warning| warning.get("kind").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    /// The unique, sorted list of contributors for the script.
    ///
    /// Contributors are the union of the `owner` column and every `author`
    /// recorded in `history_entries` inside `metadata_json`.
    pub fn contributors(&self) -> Vec<String> {
        let mut set: BTreeSet<String> = BTreeSet::new();

        let owner = self.owner().trim();
        if !owner.is_empty() {
            set.insert(owner.to_string());
        }

        if let Some(Value::Array(entries)) = self.metadata().get("history_entries") {
            for entry in entries {
                if let Some(Value::String(author)) = entry.get("author") {
                    let trimmed = author.trim();
                    if !trimmed.is_empty() {
                        set.insert(trimmed.to_string());
                    }
                }
            }
        }

        set.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_row() -> JsonRow {
        let mut row = JsonRow::new();
        row.insert("logical_path".into(), json!("/catalog/scripts/checkmc.py"));
        row.insert("language".into(), json!("python"));
        row.insert("owner".into(), json!("Alice"));
        row.insert("size".into(), json!(1536));
        row.insert("tags".into(), json!(r#"["ops","nightly"]"#));
        row.insert("entry_points".into(), json!(r#"["main"]"#));
        row.insert("checkout_user".into(), json!("bob"));
        row.insert("checkout_timestamp".into(), json!("2025-01-01T00:00:00"));
        row.insert(
            "vc_warnings".into(),
            json!(json!([{"kind": "stale", "message": "stale checkout"}]).to_string()),
        );
        row.insert(
            "metadata_json".into(),
            json!(json!({"history_entries": [{"author": "Carol"}]}).to_string()),
        );
        row
    }

    #[test]
    fn typed_accessors_read_known_columns() {
        let row = sample_row();
        let view = ScriptView::new(&row);
        assert_eq!(view.logical_path(), "/catalog/scripts/checkmc.py");
        assert_eq!(view.language(), "python");
        assert_eq!(view.size(), Some(1536));
        assert_eq!(view.string_list(ListField::Tags), vec!["ops", "nightly"]);
        assert_eq!(view.string_list(ListField::EntryPoints), vec!["main"]);
    }

    #[test]
    fn logical_parent_dir_cases() {
        assert_eq!(
            logical_parent_dir("/catalog/scripts/jobs/x.sh"),
            "/catalog/scripts/jobs"
        );
        assert_eq!(logical_parent_dir("/foo.py"), "/");
        assert_eq!(logical_parent_dir("bare.py"), "");
    }

    #[test]
    fn symlink_target_display_shortens_a_sibling_target() {
        assert_eq!(
            symlink_target_display(
                "/catalog/scripts/prepare_release",
                "/catalog/scripts/prepare_release_20260729_140513"
            ),
            "prepare_release_20260729_140513"
        );
    }

    #[test]
    fn symlink_target_display_keeps_a_target_in_another_directory() {
        assert_eq!(
            symlink_target_display("/catalog/scripts/tool.py", "/catalog/shared/tool_v2.py"),
            "/catalog/shared/tool_v2.py"
        );
    }

    #[test]
    fn parent_dir_reads_from_row() {
        let row = sample_row();
        assert_eq!(ScriptView::new(&row).parent_dir(), "/catalog/scripts");
    }

    #[test]
    fn checkout_label_reports_holder_and_timestamp() {
        let row = sample_row();
        let view = ScriptView::new(&row);
        assert_eq!(
            view.checkout_label(),
            "checked out by bob since 2025-01-01T00:00:00"
        );
    }

    #[test]
    fn checkout_label_is_clean_without_holder() {
        let row = JsonRow::new();
        assert_eq!(ScriptView::new(&row).checkout_label(), "clean");
    }

    #[test]
    fn list_value_falls_back_to_empty_array() {
        let row = JsonRow::new();
        let view = ScriptView::new(&row);
        // An absent list column serializes as `[]`, never `null`.
        assert_eq!(
            view.list_value_or_empty(ListField::Tags),
            Value::Array(Vec::new())
        );
    }

    #[test]
    fn warnings_and_contributors_are_parsed() {
        let row = sample_row();
        let view = ScriptView::new(&row);
        assert_eq!(view.vc_warning_kinds(), vec!["stale"]);
        assert_eq!(view.vc_warning_messages(), vec!["stale checkout"]);
        // Owner plus the history author, de-duplicated and sorted.
        assert_eq!(view.contributors(), vec!["Alice", "Carol"]);
    }

    // -----------------------------------------------------------------------
    // Further edge-case coverage
    // -----------------------------------------------------------------------

    #[test]
    fn string_list_falls_back_to_empty_on_malformed_json() {
        let mut row = JsonRow::new();
        row.insert("tags".into(), json!("not valid json"));
        let view = ScriptView::new(&row);
        assert!(view.string_list(ListField::Tags).is_empty());
    }

    #[test]
    fn string_list_falls_back_to_empty_when_column_missing() {
        let row = JsonRow::new();
        let view = ScriptView::new(&row);
        assert!(view.string_list(ListField::EntryPoints).is_empty());
        assert!(view.string_list(ListField::Related).is_empty());
    }

    #[test]
    fn list_value_falls_back_to_empty_array_for_non_array_json() {
        let mut row = JsonRow::new();
        // A JSON object, not an array, must not be returned as-is.
        row.insert("related".into(), json!(r#"{"not":"a list"}"#));
        let view = ScriptView::new(&row);
        assert_eq!(
            view.list_value_or_empty(ListField::Related),
            Value::Array(Vec::new())
        );
    }

    #[test]
    fn metadata_falls_back_to_empty_map_on_malformed_json() {
        let mut row = JsonRow::new();
        row.insert("metadata_json".into(), json!("{not json"));
        let view = ScriptView::new(&row);
        assert!(view.metadata().is_empty());
    }

    #[test]
    fn metadata_falls_back_to_empty_map_when_not_an_object() {
        let mut row = JsonRow::new();
        row.insert("metadata_json".into(), json!("[1,2,3]"));
        let view = ScriptView::new(&row);
        assert!(view.metadata().is_empty());
    }

    #[test]
    fn vc_warnings_fall_back_to_empty_on_malformed_json() {
        let mut row = JsonRow::new();
        row.insert("vc_warnings".into(), json!("not json"));
        let view = ScriptView::new(&row);
        assert!(view.vc_warning_kinds().is_empty());
        assert!(view.vc_warning_messages().is_empty());
    }

    #[test]
    fn contributors_ignores_whitespace_only_owner_and_authors() {
        let mut row = JsonRow::new();
        row.insert("owner".into(), json!("   "));
        row.insert(
            "metadata_json".into(),
            json!(json!({"history_entries": [{"author": "  "}, {"author": "Dana"}]}).to_string()),
        );
        let view = ScriptView::new(&row);
        assert_eq!(view.contributors(), vec!["Dana"]);
    }

    #[test]
    fn contributors_empty_when_no_owner_and_no_history() {
        let row = JsonRow::new();
        let view = ScriptView::new(&row);
        assert!(view.contributors().is_empty());
    }

    #[test]
    fn checkout_label_ignores_timestamp_without_user() {
        let mut row = JsonRow::new();
        row.insert("checkout_timestamp".into(), json!("2025-01-01T00:00:00"));
        let view = ScriptView::new(&row);
        // No holder means "clean" even if a stray timestamp is present.
        assert_eq!(view.checkout_label(), "clean");
    }

    #[test]
    fn checkout_label_without_timestamp() {
        let mut row = JsonRow::new();
        row.insert("checkout_user".into(), json!("alice"));
        let view = ScriptView::new(&row);
        assert_eq!(view.checkout_label(), "checked out by alice");
    }

    #[test]
    fn logical_parent_dir_empty_input() {
        assert_eq!(logical_parent_dir(""), "");
    }

    #[test]
    fn symlink_target_display_handles_empty_target() {
        assert_eq!(symlink_target_display("/catalog/scripts/a.py", ""), "");
    }

    #[test]
    fn size_and_mtime_are_none_for_non_numeric_values() {
        let mut row = JsonRow::new();
        row.insert("size".into(), json!("not a number"));
        row.insert("mtime".into(), json!("also not a number"));
        let view = ScriptView::new(&row);
        assert_eq!(view.size(), None);
        assert_eq!(view.mtime(), None);
    }
}
