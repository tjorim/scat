use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::core::db::{query_rows, row_string};
use crate::core::script_view::{ListField, ScriptView};
use crate::error::Result;

use super::{CatalogDiff, CatalogDiffChange, CatalogDiffScript};

/// Compare two complete catalog database snapshots.
pub fn compare_catalogs(old_db_path: &Path, new_db_path: &Path) -> Result<CatalogDiff> {
    let old_conn = crate::core::db::open_db(old_db_path)?;
    let new_conn = crate::core::db::open_db(new_db_path)?;

    let old_scripts = scripts_for_diff(&old_conn)?;
    let new_scripts = scripts_for_diff(&new_conn)?;
    let old_deps = dependencies_for_diff(&old_conn)?;
    let new_deps = dependencies_for_diff(&new_conn)?;

    let old_paths: BTreeSet<&String> = old_scripts.keys().collect();
    let new_paths: BTreeSet<&String> = new_scripts.keys().collect();

    let added = new_paths
        .difference(&old_paths)
        .map(|path| script_diff_row(&new_scripts[*path]))
        .collect();
    let removed = old_paths
        .difference(&new_paths)
        .map(|path| script_diff_row(&old_scripts[*path]))
        .collect();

    let mut changed = Vec::new();
    for path in old_paths.intersection(&new_paths) {
        let old_script = &old_scripts[*path];
        let new_script = &new_scripts[*path];
        let mut fields = BTreeMap::new();
        for field in DIFF_FIELDS {
            let old_value = old_script
                .fields
                .get(*field)
                .cloned()
                .unwrap_or(Value::Null);
            let new_value = new_script
                .fields
                .get(*field)
                .cloned()
                .unwrap_or(Value::Null);
            if old_value != new_value {
                fields.insert((*field).to_string(), [old_value, new_value]);
            }
        }

        let empty = BTreeSet::new();
        let old_script_deps = old_deps.get(*path).unwrap_or(&empty);
        let new_script_deps = new_deps.get(*path).unwrap_or(&empty);
        let deps_added = new_script_deps
            .difference(old_script_deps)
            .cloned()
            .collect::<Vec<_>>();
        let deps_removed = old_script_deps
            .difference(new_script_deps)
            .cloned()
            .collect::<Vec<_>>();

        if !fields.is_empty() || !deps_added.is_empty() || !deps_removed.is_empty() {
            changed.push(CatalogDiffChange {
                logical_path: (*path).clone(),
                fields,
                deps_added,
                deps_removed,
            });
        }
    }

    Ok(CatalogDiff {
        added,
        removed,
        changed,
    })
}

const DIFF_FIELDS: &[&str] = &[
    "purpose",
    "techowner",
    "funcowner",
    "owner",
    "language",
    "tags",
    "entry_points",
    "related",
];

struct DiffScript {
    logical_path: String,
    fields: BTreeMap<String, Value>,
}

fn script_diff_row(script: &DiffScript) -> CatalogDiffScript {
    CatalogDiffScript {
        logical_path: script.logical_path.clone(),
        language: script
            .fields
            .get("language")
            .cloned()
            .unwrap_or(Value::Null),
        owner: script.fields.get("owner").cloned().unwrap_or(Value::Null),
    }
}

fn scripts_for_diff(conn: &rusqlite::Connection) -> Result<BTreeMap<String, DiffScript>> {
    let rows = query_rows(
        conn,
        "SELECT logical_path, purpose, owner, language, tags, entry_points, related, metadata_json
         FROM scripts
         ORDER BY logical_path",
        &[],
    )?;
    let mut scripts = BTreeMap::new();
    for row in rows {
        let view = ScriptView::new(&row);
        let logical_path = view.logical_path().to_string();
        let metadata = view.metadata();

        let mut fields = BTreeMap::new();
        fields.insert(
            "purpose".to_string(),
            view.purpose_value().cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "techowner".to_string(),
            metadata.get("techowner").cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "funcowner".to_string(),
            metadata.get("funcowner").cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "owner".to_string(),
            view.owner_value().cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "language".to_string(),
            view.language_value().cloned().unwrap_or(Value::Null),
        );
        fields.insert(
            "tags".to_string(),
            view.list_value_or_empty(ListField::Tags),
        );
        fields.insert(
            "entry_points".to_string(),
            view.list_value_or_empty(ListField::EntryPoints),
        );
        fields.insert(
            "related".to_string(),
            view.list_value_or_empty(ListField::Related),
        );

        scripts.insert(
            logical_path.clone(),
            DiffScript {
                logical_path,
                fields,
            },
        );
    }
    Ok(scripts)
}

fn dependencies_for_diff(
    conn: &rusqlite::Connection,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let rows = query_rows(
        conn,
        "SELECT s.logical_path, d.depends_on_path
         FROM dependencies d
         JOIN scripts s ON s.id = d.script_id
         ORDER BY s.logical_path, d.depends_on_path",
        &[],
    )?;
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        deps.entry(row_string(&row, "logical_path"))
            .or_default()
            .insert(row_string(&row, "depends_on_path"));
    }
    Ok(deps)
}
