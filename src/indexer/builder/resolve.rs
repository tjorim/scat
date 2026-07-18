use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::Value;

use crate::core::db::query_rows;
use crate::error::Result;

pub(super) fn module_name_from_logical_path(logical_path: &str) -> String {
    let mut path = logical_path.replace('\\', "/");
    path = path.trim_start_matches('/').to_string();
    if path.ends_with(".py") {
        path.truncate(path.len().saturating_sub(3));
    }
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.last().copied() == Some("__init__") {
        parts.pop();
    }
    parts.join(".")
}

fn module_candidates_from_logical_path(logical_path: &str) -> Vec<String> {
    let module = module_name_from_logical_path(logical_path);
    if module.is_empty() {
        return vec![];
    }
    let parts: Vec<&str> = module.split('.').collect();
    // Returns candidates longest-first (full path before suffixes), so `module_to_script`
    // insertion via `entry().or_insert()` prefers the most-specific match when the same
    // short suffix appears in multiple scripts (e.g. two files both generating key "os").
    (0..parts.len()).map(|i| parts[i..].join(".")).collect()
}

/// Build a `module_name → script_id` map from every indexed script.
/// Candidates are generated longest-first so the first insertion wins for any
/// given suffix, preferring the most-specific match.
pub(super) fn build_module_map(conn: &Connection) -> Result<HashMap<String, i64>> {
    let mut map: HashMap<String, i64> = HashMap::default();
    let rows = query_rows(conn, "SELECT id, logical_path FROM scripts", &[])?;
    for row in rows {
        let id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
        let path = row
            .get("logical_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        for candidate in module_candidates_from_logical_path(path) {
            map.entry(candidate).or_insert(id);
        }
    }
    Ok(map)
}

/// Resolve a relative import path (e.g. `".utils"`, `"..helpers"`) against
/// the caller's dotted module name.  Returns `None` when resolution would go
/// above the root package.
///
/// `is_package` should be `true` when the caller is a package (`__init__.py`).
/// For a regular module file, each leading dot strips one extra component
/// (the module name itself counts as the first level), so `from . import x`
/// in `pkg/mod.py` resolves to `pkg.x` with `levels_up = 1`.  For a package
/// `__init__.py`, one dot refers to the package itself (`levels_up = 0`).
fn resolve_relative_dep(dep: &str, caller_module: &str, is_package: bool) -> Option<String> {
    let dots = dep.chars().take_while(|&c| c == '.').count();
    if dots == 0 {
        return None;
    }
    let suffix = &dep[dots..];
    let mut parts: Vec<&str> = caller_module.split('.').collect();
    let levels_up = if is_package {
        dots.saturating_sub(1)
    } else {
        dots
    };
    if levels_up >= parts.len() {
        return None;
    }
    parts.truncate(parts.len() - levels_up);
    if !suffix.is_empty() {
        parts.push(suffix);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

/// Populate `dependencies.resolved_script_id` for every row whose module name
/// can be matched to an indexed script, handling both absolute and relative
/// import paths.
pub(super) fn resolve_dependency_targets(
    conn: &Connection,
    module_map: &HashMap<String, i64>,
) -> Result<()> {
    let rows = query_rows(
        conn,
        "SELECT d.id, d.depends_on_path, s.logical_path AS caller_path
         FROM dependencies d
         JOIN scripts s ON s.id = d.script_id
         WHERE d.resolved_script_id IS NULL AND d.kind = 'import'",
        &[],
    )?;

    let mut updates: Vec<(i64, i64)> = Vec::new();
    for row in rows {
        let dep_id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
        let dep_path = row
            .get("depends_on_path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let caller_path = row
            .get("caller_path")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let candidates: Vec<String> = if dep_path.starts_with('.') {
            let caller_module = module_name_from_logical_path(caller_path);
            match resolve_relative_dep(
                dep_path,
                &caller_module,
                caller_path.ends_with("__init__.py"),
            ) {
                Some(resolved) => {
                    let parts: Vec<&str> = resolved.split('.').collect();
                    (0..parts.len()).map(|i| parts[i..].join(".")).collect()
                }
                None => vec![],
            }
        } else {
            let parts: Vec<&str> = dep_path.split('.').collect();
            (0..parts.len()).map(|i| parts[i..].join(".")).collect()
        };

        for candidate in &candidates {
            if let Some(&target_id) = module_map.get(candidate.as_str()) {
                updates.push((target_id, dep_id));
                break;
            }
        }
    }

    if !updates.is_empty() {
        let mut stmt =
            conn.prepare("UPDATE dependencies SET resolved_script_id = ?1 WHERE id = ?2")?;
        for (target_id, dep_id) in updates {
            stmt.execute(rusqlite::params![target_id, dep_id])?;
        }
    }

    Ok(())
}

/// Resolve `referenced` path-literal edges by exact match against indexed
/// logical paths, then drop any that stay unresolved.
///
/// Unlike imports (whose unresolved edges point at real external libraries and
/// are worth keeping), an unresolved path literal is almost always an unrelated
/// string — a log file, a temp path, a copy destination that differs from the
/// source's own logical path — rather than a genuine script edge. Keeping only
/// exact logical-path matches is what makes this heuristic high-precision.
pub(super) fn resolve_reference_targets(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE dependencies
         SET resolved_script_id = (
             SELECT s.id FROM scripts s WHERE s.logical_path = dependencies.depends_on_path
         )
         WHERE kind = 'referenced' AND resolved_script_id IS NULL",
        [],
    )?;
    conn.execute(
        "DELETE FROM dependencies WHERE kind = 'referenced' AND resolved_script_id IS NULL",
        [],
    )?;
    Ok(())
}

pub(super) fn resolve_function_targets(
    conn: &Connection,
    module_map: &HashMap<String, i64>,
) -> Result<()> {
    // Pass 1: resolve local calls (no external module reference) in a single bulk UPDATE.
    conn.execute(
        "UPDATE function_calls
         SET resolved_target_script_id = (
             SELECT fd.script_id
             FROM function_definitions fd
             WHERE fd.script_id = function_calls.script_id
               AND fd.name = CASE
                   WHEN INSTR(function_calls.callee, '.') > 0
                   THEN SUBSTR(function_calls.callee, 1, INSTR(function_calls.callee, '.') - 1)
                   ELSE function_calls.callee
                 END
             LIMIT 1
         )
         WHERE resolved_target_script_id IS NULL
           AND (resolved_target_name IS NULL OR resolved_target_name = '')
           AND callee != ''
           AND EXISTS (
               SELECT 1 FROM function_definitions fd
               WHERE fd.script_id = function_calls.script_id
                 AND fd.name = CASE
                     WHEN INSTR(function_calls.callee, '.') > 0
                     THEN SUBSTR(function_calls.callee, 1, INSTR(function_calls.callee, '.') - 1)
                     ELSE function_calls.callee
                   END
           )",
        [],
    )?;

    // Pass 2: resolve module-referenced calls using the in-memory map, then batch-update.
    let call_rows = query_rows(
        conn,
        "SELECT id, resolved_target_name
         FROM function_calls
         WHERE resolved_target_script_id IS NULL
           AND resolved_target_name IS NOT NULL
           AND resolved_target_name != ''",
        &[],
    )?;

    let mut updates: Vec<(i64, i64)> = Vec::new();
    for row in call_rows {
        let call_id = row.get("id").and_then(Value::as_i64).unwrap_or_default();
        let resolved_name = row
            .get("resolved_target_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let parts: Vec<&str> = resolved_name.split('.').collect();
        for i in (1..=parts.len()).rev() {
            let candidate = parts[..i].join(".");
            if let Some(&id) = module_map.get(&candidate) {
                updates.push((id, call_id));
                break;
            }
        }
    }

    if !updates.is_empty() {
        let mut stmt =
            conn.prepare("UPDATE function_calls SET resolved_target_script_id = ?1 WHERE id = ?2")?;
        for (target_id, call_id) in updates {
            stmt.execute(rusqlite::params![target_id, call_id])?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_relative_dep;

    #[test]
    fn resolve_relative_dep_regular_module_one_dot() {
        // from . import utils  in pkg/mod.py (module pkg.mod, not a package)
        // → pkg.utils, NOT pkg.mod.utils
        assert_eq!(
            resolve_relative_dep(".utils", "pkg.mod", false),
            Some("pkg.utils".to_string())
        );
    }

    #[test]
    fn resolve_relative_dep_package_one_dot() {
        // from . import utils  in pkg/__init__.py (module pkg, is a package)
        // → pkg.utils
        assert_eq!(
            resolve_relative_dep(".utils", "pkg", true),
            Some("pkg.utils".to_string())
        );
    }

    #[test]
    fn resolve_relative_dep_two_dots_goes_to_parent() {
        // from .. import utils  in pkg/sub/mod.py → pkg.utils
        assert_eq!(
            resolve_relative_dep("..utils", "pkg.sub.mod", false),
            Some("pkg.utils".to_string())
        );
    }

    #[test]
    fn resolve_relative_dep_above_root_returns_none() {
        // from .. import utils  in top-level mod.py — can't go above root
        assert_eq!(resolve_relative_dep("..utils", "mod", false), None);
    }

    #[test]
    fn resolve_relative_dep_bare_dot_returns_package() {
        // from . import *  with empty suffix in pkg/mod.py → pkg
        assert_eq!(
            resolve_relative_dep(".", "pkg.mod", false),
            Some("pkg".to_string())
        );
    }
}
