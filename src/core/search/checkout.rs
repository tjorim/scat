use serde_json::Value;

use crate::core::db::{JsonRow, query_rows};
use crate::core::vc::REVISION_TYPE_DEVELOP;
use crate::error::Result;

use super::SearchApi;

impl SearchApi {
    // ------------------------------------------------------------------
    // Checkout / vc state
    // ------------------------------------------------------------------

    /// Return checkout/status rows, optionally scoped to one logical path.
    pub fn checkout_status(&self, logical_path: Option<&str>) -> Result<Vec<JsonRow>> {
        if let Some(path) = logical_path {
            if let Some(row) = self.script_checkout_status(path)? {
                return Ok(vec![row]);
            }
            return self.orphan_checkout_status(Some(path));
        }

        let mut rows = query_rows(
            &self.conn,
            "SELECT s.*,
                    r.checkout_user,
                    r.checkout_timestamp,
                    r.checkout_os,
                    r.checkout_age_seconds
             FROM scripts s
             LEFT JOIN (
                 SELECT logical_path,
                        GROUP_CONCAT(DISTINCT user)      AS checkout_user,
                        MAX(timestamp)                   AS checkout_timestamp,
                        GROUP_CONCAT(DISTINCT os_flavor) AS checkout_os,
                        MAX(age_seconds)                 AS checkout_age_seconds
                 FROM revisions
                 WHERE revision_type = ?1
                 GROUP BY logical_path
             ) r ON r.logical_path = s.logical_path
             WHERE r.checkout_user IS NOT NULL
                OR (vc_warnings IS NOT NULL AND vc_warnings != '[]')
             ORDER BY s.logical_path",
            &[&REVISION_TYPE_DEVELOP],
        )?;

        let indexed: std::collections::HashSet<String> = rows
            .iter()
            .filter_map(|r| {
                r.get("logical_path")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();

        for orphan in self.orphan_checkout_status(None)? {
            let p = orphan
                .get("logical_path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !indexed.contains(&p) {
                rows.push(orphan);
            }
        }

        rows.sort_by(|a, b| {
            let pa = a.get("logical_path").and_then(Value::as_str).unwrap_or("");
            let pb = b.get("logical_path").and_then(Value::as_str).unwrap_or("");
            pa.cmp(pb)
        });
        Ok(rows)
    }

    /// Return raw checkout rows for one logical path.
    pub fn checkouts_for(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        self.revisions_for(logical_path)
    }

    /// Return indexed revision rows for one logical path.
    pub fn revisions_for(&self, logical_path: &str) -> Result<Vec<JsonRow>> {
        query_rows(
            &self.conn,
            "SELECT logical_path, physical_path, revision_type, os_flavor, user, timestamp, age_seconds
             FROM revisions WHERE logical_path=?
             ORDER BY revision_type, timestamp DESC, physical_path",
            &[&logical_path],
        )
    }

    fn script_checkout_status(&self, logical_path: &str) -> Result<Option<JsonRow>> {
        let mut rows = query_rows(
            &self.conn,
            "SELECT s.*,
                    r.checkout_user,
                    r.checkout_timestamp,
                    r.checkout_os,
                    r.checkout_age_seconds
             FROM scripts s
             LEFT JOIN (
                 SELECT logical_path,
                        GROUP_CONCAT(DISTINCT user)      AS checkout_user,
                        MAX(timestamp)                   AS checkout_timestamp,
                        GROUP_CONCAT(DISTINCT os_flavor) AS checkout_os,
                        MAX(age_seconds)                 AS checkout_age_seconds
                 FROM revisions
                 WHERE revision_type = ?1
                 GROUP BY logical_path
             ) r ON r.logical_path = s.logical_path
             WHERE s.logical_path = ?2",
            &[&REVISION_TYPE_DEVELOP, &logical_path],
        )?;
        Ok(rows.pop())
    }

    fn orphan_checkout_status(&self, logical_path: Option<&str>) -> Result<Vec<JsonRow>> {
        let orphan_warning = serde_json::json!([{
            "kind": "checkout_without_catalog_entry",
            "message": "Revision exists in DEVELOP/ARCHIVE but no active catalog entry was indexed.",
            "details": {}
        }])
        .to_string();

        let base_rows = if let Some(path) = logical_path {
            query_rows(
                &self.conn,
                "SELECT c.logical_path,
                        GROUP_CONCAT(DISTINCT c.user)      AS checkout_user,
                        MAX(c.timestamp)                   AS checkout_timestamp,
                        GROUP_CONCAT(DISTINCT c.os_flavor) AS checkout_os,
                        MAX(c.age_seconds)                 AS checkout_age_seconds
                 FROM revisions c
                 LEFT JOIN scripts s ON s.logical_path = c.logical_path
                 WHERE s.id IS NULL
                   AND c.revision_type = ?
                   AND c.logical_path = ?
                 GROUP BY c.logical_path ORDER BY c.logical_path",
                &[&REVISION_TYPE_DEVELOP, &path],
            )?
        } else {
            query_rows(
                &self.conn,
                "SELECT c.logical_path,
                        GROUP_CONCAT(DISTINCT c.user)      AS checkout_user,
                        MAX(c.timestamp)                   AS checkout_timestamp,
                        GROUP_CONCAT(DISTINCT c.os_flavor) AS checkout_os,
                        MAX(c.age_seconds)                 AS checkout_age_seconds
                 FROM revisions c
                 LEFT JOIN scripts s ON s.logical_path = c.logical_path
                 WHERE s.id IS NULL
                   AND c.revision_type = ?
                 GROUP BY c.logical_path ORDER BY c.logical_path",
                &[&REVISION_TYPE_DEVELOP],
            )?
        };

        Ok(base_rows
            .into_iter()
            .map(|mut row| {
                row.insert("language".into(), Value::Null);
                row.insert("owner".into(), Value::Null);
                row.insert("purpose".into(), Value::Null);
                row.insert("vc_warnings".into(), Value::String(orphan_warning.clone()));
                row
            })
            .collect())
    }
}
