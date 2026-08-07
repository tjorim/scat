use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::core::db::{SCHEMA_VERSION, query_rows, row_string};
use crate::core::vc::{REVISION_TYPE_ARCHIVE, REVISION_TYPE_DEVELOP, REVISION_TYPE_WORKING};
use crate::error::{Error, Result};

use super::{
    AuditFinding, AuditOptions, AuditResult, AuditSummary, CheckCount, CheckoutUserCount,
    DailyCount, DependentCount, HistogramBucket, IndexMetadata, LangCount, OsFlavorCount,
    OwnerCount, RevisionStats, STATS_RANKING_LIMIT, SearchApi, StatsResult, TagCount,
};

/// `stale_days` threshold for `stats()`'s internal audit run
/// (`findings_by_check`), matching `scat catalog audit`'s own CLI default.
const STATS_AUDIT_STALE_DAYS: i64 = 90;

/// Ascending byte-size buckets for `size_histogram`. Each entry is the
/// bucket's exclusive upper bound and label; the last bound is unreachable
/// (`i64::MAX`) so every size lands somewhere.
const SIZE_BUCKETS: &[(i64, &str)] = &[
    (1024, "<1KB"),
    (5 * 1024, "1-5KB"),
    (10 * 1024, "5-10KB"),
    (25 * 1024, "10-25KB"),
    (50 * 1024, "25-50KB"),
    (100 * 1024, "50-100KB"),
    (250 * 1024, "100-250KB"),
    (1024 * 1024, "250KB-1MB"),
    (i64::MAX, ">1MB"),
];

/// Ascending checkout-age buckets (in days) for `checkout_staleness_histogram`.
const STALENESS_BUCKETS_DAYS: &[(f64, &str)] = &[
    (7.0, "0-7d"),
    (30.0, "7-30d"),
    (90.0, "30-90d"),
    (180.0, "90-180d"),
    (365.0, "180-365d"),
    (f64::MAX, ">365d"),
];

/// Bucket `values` into `bounds` (each an exclusive upper bound + label),
/// returning one [`HistogramBucket`] per bound in ascending order —
/// including buckets nothing landed in, so the histogram's shape is
/// accurate rather than just its non-empty bars.
fn bucket_counts<T: PartialOrd + Copy>(values: &[T], bounds: &[(T, &str)]) -> Vec<HistogramBucket> {
    let mut counts = vec![0i64; bounds.len()];
    for &value in values {
        if let Some(idx) = bounds.iter().position(|&(bound, _)| value < bound) {
            counts[idx] += 1;
        }
    }
    bounds
        .iter()
        .zip(counts)
        .map(|(&(_, label), count)| HistogramBucket {
            label: label.to_string(),
            count,
        })
        .collect()
}

impl SearchApi {
    // ------------------------------------------------------------------
    // Stats
    // ------------------------------------------------------------------

    /// Return catalog statistics grouped by language and owner.
    pub fn stats(&self) -> Result<StatsResult> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM scripts", [], |r| r.get(0))?;

        let by_language: Vec<LangCount> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT COALESCE(language,'unknown') AS language, COUNT(*) AS count
                 FROM scripts GROUP BY language ORDER BY count DESC",
            )?;
            let rows: Result<Vec<LangCount>> = stmt
                .query_map([], |row| {
                    Ok(LangCount {
                        language: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        let by_owner: Vec<OwnerCount> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT COALESCE(owner,'unknown') AS owner, COUNT(*) AS count
                 FROM scripts GROUP BY owner ORDER BY count DESC",
            )?;
            let rows: Result<Vec<OwnerCount>> = stmt
                .query_map([], |row| {
                    Ok(OwnerCount {
                        owner: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        let most_depended_upon: Vec<DependentCount> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT s.logical_path AS logical_path, COUNT(d.id) AS count
                 FROM scripts s
                 JOIN dependencies d ON d.resolved_script_id = s.id
                 GROUP BY s.id
                 ORDER BY count DESC, logical_path
                 LIMIT ?1",
            )?;
            let rows: Result<Vec<DependentCount>> = stmt
                .query_map([STATS_RANKING_LIMIT as i64], |row| {
                    Ok(DependentCount {
                        logical_path: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        let top_tags: Vec<TagCount> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT je.value AS tag, COUNT(*) AS count
                 FROM scripts, json_each(scripts.tags) AS je
                 GROUP BY je.value
                 ORDER BY count DESC, tag
                 LIMIT ?1",
            )?;
            let rows: Result<Vec<TagCount>> = stmt
                .query_map([STATS_RANKING_LIMIT as i64], |row| {
                    Ok(TagCount {
                        tag: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        let most_functions: Vec<DependentCount> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT s.logical_path AS logical_path, COUNT(f.id) AS count
                 FROM scripts s
                 JOIN function_definitions f ON f.script_id = s.id
                 GROUP BY s.id
                 ORDER BY count DESC, logical_path
                 LIMIT ?1",
            )?;
            let rows: Result<Vec<DependentCount>> = stmt
                .query_map([STATS_RANKING_LIMIT as i64], |row| {
                    Ok(DependentCount {
                        logical_path: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        let checkout_by_os: Vec<OsFlavorCount> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT os_flavor, COUNT(*) AS count
                 FROM revisions
                 WHERE revision_type = ?1
                 GROUP BY os_flavor
                 ORDER BY count DESC, os_flavor
                 LIMIT ?2",
            )?;
            let rows: Result<Vec<OsFlavorCount>> = stmt
                .query_map(
                    rusqlite::params![REVISION_TYPE_DEVELOP, STATS_RANKING_LIMIT as i64],
                    |row| {
                        Ok(OsFlavorCount {
                            os_flavor: row.get(0)?,
                            count: row.get(1)?,
                        })
                    },
                )?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        let most_active_checkout_users: Vec<CheckoutUserCount> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT user, COUNT(*) AS count
                 FROM revisions
                 WHERE revision_type = ?1 AND user != ''
                 GROUP BY user
                 ORDER BY count DESC, user
                 LIMIT ?2",
            )?;
            let rows: Result<Vec<CheckoutUserCount>> = stmt
                .query_map(
                    rusqlite::params![REVISION_TYPE_DEVELOP, STATS_RANKING_LIMIT as i64],
                    |row| {
                        Ok(CheckoutUserCount {
                            user: row.get(0)?,
                            count: row.get(1)?,
                        })
                    },
                )?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        // Bucketed client-side (rather than a SQL GROUP BY on a CASE
        // expression) so buckets stay in ascending range order — including
        // empty ones — instead of SQL's GROUP BY reordering them by
        // whichever count sorts where.
        let size_histogram = {
            let mut stmt = self
                .conn
                .prepare_cached("SELECT size FROM scripts WHERE size IS NOT NULL")?;
            let sizes: Result<Vec<i64>> = stmt
                .query_map([], |row| row.get(0))?
                .map(|r| r.map_err(Error::from))
                .collect();
            bucket_counts(&sizes?, SIZE_BUCKETS)
        };

        let checkout_staleness_histogram = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT age_seconds FROM revisions
                 WHERE revision_type = ?1 AND age_seconds IS NOT NULL",
            )?;
            let ages_days: Result<Vec<f64>> = stmt
                .query_map([REVISION_TYPE_DEVELOP], |row| {
                    row.get::<_, f64>(0).map(|secs| secs / 86_400.0)
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            bucket_counts(&ages_days?, STALENESS_BUCKETS_DAYS)
        };

        let findings_by_check: Vec<CheckCount> = {
            let audit = self.audit(None, STATS_AUDIT_STALE_DAYS, AuditOptions::default())?;
            let mut counts: std::collections::BTreeMap<String, i64> =
                std::collections::BTreeMap::new();
            for finding in audit.findings {
                *counts.entry(finding.check).or_insert(0) += 1;
            }
            let mut checks: Vec<CheckCount> = counts
                .into_iter()
                .map(|(check, count)| CheckCount { check, count })
                .collect();
            checks.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.check.cmp(&b.check)));
            checks
        };

        let checkout_activity_by_day: Vec<DailyCount> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT SUBSTR(timestamp, 1, 8) AS day, COUNT(*) AS count
                 FROM revisions
                 WHERE revision_type = ?1
                 GROUP BY day
                 ORDER BY day",
            )?;
            let rows: Result<Vec<DailyCount>> = stmt
                .query_map([REVISION_TYPE_DEVELOP], |row| {
                    Ok(DailyCount {
                        date: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .map(|r| r.map_err(Error::from))
                .collect();
            rows?
        };

        Ok(StatsResult {
            total_scripts: total,
            by_language,
            by_owner,
            most_depended_upon,
            top_tags,
            most_functions,
            checkout_by_os,
            most_active_checkout_users,
            size_histogram,
            checkout_staleness_histogram,
            findings_by_check,
            checkout_activity_by_day,
            revisions: self.revision_stats()?,
        })
    }

    // ------------------------------------------------------------------
    // Index metadata
    // ------------------------------------------------------------------

    /// Return build/index metadata row and current schema version.
    pub fn index_metadata(&self) -> Result<IndexMetadata> {
        let row = self
            .conn
            .query_row(
                "SELECT build_timestamp, schema_version FROM index_metadata WHERE id=1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                    ))
                },
            )
            .optional()?;

        let (ts, sv) = row.unwrap_or((None, None));
        Ok(IndexMetadata {
            build_timestamp: ts.map_or(Value::Null, Value::String),
            schema_version: sv.map_or(Value::Null, |n| Value::Number(n.into())),
            current_schema_version: SCHEMA_VERSION,
        })
    }

    /// Run selected audit checks and return findings with summary counts.
    pub fn audit(
        &self,
        checks: Option<&[String]>,
        stale_days: i64,
        options: AuditOptions<'_>,
    ) -> Result<AuditResult> {
        const ALL_CHECKS: &[&str] = &[
            "unowned",
            "no-purpose",
            "broken-deps",
            "orphan-checkouts",
            "stale-checkouts",
            "dead-scripts",
            "no-description",
            "near-duplicates",
            "outliers",
        ];

        let selected = checks.map(|values| {
            values
                .iter()
                .map(|v| v.trim().to_string())
                .collect::<std::collections::HashSet<_>>()
        });

        if let Some(set) = &selected {
            for check in set {
                if !ALL_CHECKS.contains(&check.as_str()) {
                    return Err(Error::Validation(format!("unknown audit check: {check}")));
                }
            }
        }

        let stale_seconds = stale_days.max(0) as f64 * 86_400.0;

        let mut findings = Vec::new();

        if should_run(selected.as_ref(), "unowned") {
            let rows = query_rows(
                &self.conn,
                "SELECT logical_path
                 FROM scripts
                 WHERE COALESCE(TRIM(json_extract(metadata_json, '$.techowner')), '') = ''
                   AND COALESCE(TRIM(json_extract(metadata_json, '$.funcowner')), '') = ''
                 ORDER BY logical_path",
                &[],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "unowned".to_string(),
                severity: "warn".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "no techowner or funcowner".to_string(),
            }));
        }

        if should_run(selected.as_ref(), "no-purpose") {
            let rows = query_rows(
                &self.conn,
                "SELECT logical_path
                 FROM scripts
                 WHERE COALESCE(TRIM(purpose), '') = ''
                 ORDER BY logical_path",
                &[],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "no-purpose".to_string(),
                severity: "warn".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "purpose/brief is missing".to_string(),
            }));
        }

        if should_run(selected.as_ref(), "broken-deps") {
            let rows = query_rows(
                &self.conn,
                "SELECT src.logical_path AS logical_path, d.depends_on_path AS dependency
                 FROM dependencies d
                 JOIN scripts src ON src.id = d.script_id
                 WHERE d.resolved_script_id IS NULL
                 ORDER BY src.logical_path, d.depends_on_path",
                &[],
            )?;
            findings.extend(rows.into_iter().map(|row| {
                let dep = row_string(&row, "dependency");
                AuditFinding {
                    check: "broken-deps".to_string(),
                    severity: "error".to_string(),
                    logical_path: row_string(&row, "logical_path"),
                    detail: format!("depends on {dep} (not indexed)"),
                }
            }));
        }

        if should_run(selected.as_ref(), "orphan-checkouts") {
            let rows = query_rows(
                &self.conn,
                "SELECT r.logical_path
                 FROM revisions r
                 LEFT JOIN scripts s ON s.logical_path = r.logical_path
                 WHERE s.logical_path IS NULL
                   AND r.revision_type = ?1
                 GROUP BY r.logical_path
                 ORDER BY r.logical_path",
                &[&REVISION_TYPE_DEVELOP],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "orphan-checkouts".to_string(),
                severity: "warn".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "vc checkout exists without catalog entry".to_string(),
            }));
        }

        if should_run(selected.as_ref(), "stale-checkouts") {
            let rows = query_rows(
                &self.conn,
                "SELECT logical_path, MAX(age_seconds) AS checkout_age_seconds
                 FROM revisions
                 WHERE revision_type = ?1
                   AND age_seconds IS NOT NULL
                 GROUP BY logical_path
                 HAVING MAX(age_seconds) >= ?2
                 ORDER BY logical_path",
                &[&REVISION_TYPE_DEVELOP, &stale_seconds],
            )?;
            findings.extend(rows.into_iter().map(|row| {
                let age = row
                    .get("checkout_age_seconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    / 86_400.0;
                AuditFinding {
                    check: "stale-checkouts".to_string(),
                    severity: "info".to_string(),
                    logical_path: row_string(&row, "logical_path"),
                    detail: format!("checkout is stale ({age:.0} days old)"),
                }
            }));
        }

        if should_run(selected.as_ref(), "dead-scripts") {
            let rows = query_rows(
                &self.conn,
                "SELECT s.logical_path
                 FROM scripts s
                 LEFT JOIN dependencies d ON d.resolved_script_id = s.id
                 LEFT JOIN revisions r ON r.logical_path = s.logical_path
                  AND r.revision_type = ?1
                 LEFT JOIN scripts sym ON sym.symlink_target = s.logical_path
                 GROUP BY s.id
                 HAVING COUNT(DISTINCT d.id) = 0
                    AND COUNT(DISTINCT r.id) = 0
                    AND COUNT(DISTINCT sym.id) = 0
                 ORDER BY s.logical_path",
                &[&REVISION_TYPE_DEVELOP],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "dead-scripts".to_string(),
                severity: "info".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "no dependents, never checked out".to_string(),
            }));
        }

        if should_run(selected.as_ref(), "no-description") {
            let rows = query_rows(
                &self.conn,
                "SELECT logical_path
                 FROM scripts
                 WHERE COALESCE(TRIM(json_extract(metadata_json, '$.brief')), '') = ''
                   AND COALESCE(TRIM(json_extract(metadata_json, '$.docstring')), '') = ''
                 ORDER BY logical_path",
                &[],
            )?;
            findings.extend(rows.into_iter().map(|row| AuditFinding {
                check: "no-description".to_string(),
                severity: "info".to_string(),
                logical_path: row_string(&row, "logical_path"),
                detail: "missing both docstring and @brief metadata".to_string(),
            }));
        }

        if should_run(selected.as_ref(), "near-duplicates") {
            match options.sidecar {
                Some(sidecar) => {
                    findings.extend(
                        sidecar
                            .near_duplicates(options.near_duplicate_threshold)
                            .into_iter()
                            .map(|pair| AuditFinding {
                                check: "near-duplicates".to_string(),
                                severity: "warn".to_string(),
                                logical_path: pair.a,
                                detail: format!(
                                    "near-duplicate of {} (cosine similarity {:.3})",
                                    pair.b, pair.score
                                ),
                            }),
                    );
                }
                None if selected
                    .as_ref()
                    .is_some_and(|s| s.contains("near-duplicates")) =>
                {
                    return Err(Error::Validation(
                        "near-duplicates requires an embeddings sidecar (run scat-embed and \
                         publish embeddings.sqlite; see crates/scat-embed)"
                            .to_string(),
                    ));
                }
                None => {}
            }
        }

        if should_run(selected.as_ref(), "outliers") {
            match options.sidecar {
                Some(sidecar) => {
                    findings.extend(sidecar.outliers(options.outlier_threshold).into_iter().map(
                        |s| AuditFinding {
                            check: "outliers".to_string(),
                            severity: "info".to_string(),
                            logical_path: s.logical_path,
                            detail: format!(
                                "no similar script found (best match cosine similarity {:.3})",
                                s.score
                            ),
                        },
                    ));
                }
                None if selected.as_ref().is_some_and(|s| s.contains("outliers")) => {
                    return Err(Error::Validation(
                        "outliers requires an embeddings sidecar (run scat-embed and publish \
                         embeddings.sqlite; see crates/scat-embed)"
                            .to_string(),
                    ));
                }
                None => {}
            }
        }

        findings.sort_by(|a, b| {
            a.check
                .cmp(&b.check)
                .then_with(|| a.logical_path.cmp(&b.logical_path))
                .then_with(|| a.detail.cmp(&b.detail))
        });

        let summary = findings.iter().fold(AuditSummary::default(), |mut acc, f| {
            match f.severity.as_str() {
                "error" => acc.error += 1,
                "warn" => acc.warn += 1,
                _ => acc.info += 1,
            }
            acc
        });

        Ok(AuditResult { findings, summary })
    }

    fn revision_stats(&self) -> Result<Option<RevisionStats>> {
        let has_revisions_table: bool = self.conn.query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM sqlite_master
                 WHERE type = 'table' AND name = 'revisions'
             )",
            [],
            |row| row.get(0),
        )?;
        if !has_revisions_table {
            return Ok(None);
        }

        let total_revision_rows: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM revisions", [], |row| row.get(0))?;
        if total_revision_rows == 0 {
            return Ok(None);
        }

        let (
            scripts_with_active_checkouts,
            scripts_with_archive_entries,
            scripts_with_working_versions,
            total_develop_revision_files,
            total_archive_revision_files,
            total_working_revision_files,
        ): (i64, i64, i64, i64, i64, i64) = self.conn.query_row(
            "SELECT
                 COUNT(DISTINCT CASE WHEN revision_type = ?1 THEN logical_path END),
                 COUNT(DISTINCT CASE WHEN revision_type = ?2 THEN logical_path END),
                 COUNT(DISTINCT CASE WHEN revision_type = ?3 THEN logical_path END),
                 COUNT(CASE WHEN revision_type = ?1 THEN 1 END),
                 COUNT(CASE WHEN revision_type = ?2 THEN 1 END),
                 COUNT(CASE WHEN revision_type = ?3 THEN 1 END)
             FROM revisions",
            [
                REVISION_TYPE_DEVELOP,
                REVISION_TYPE_ARCHIVE,
                REVISION_TYPE_WORKING,
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        let scripts_checked_out_by_multiple_users: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM (
                 SELECT logical_path
                 FROM revisions
                 WHERE revision_type = ?1
                 GROUP BY logical_path
                 HAVING COUNT(DISTINCT user) > 1
             )",
            [REVISION_TYPE_DEVELOP],
            |row| row.get(0),
        )?;

        Ok(Some(RevisionStats {
            scripts_with_active_checkouts,
            scripts_with_archive_entries,
            total_develop_revision_files,
            total_archive_revision_files,
            scripts_with_working_versions,
            total_working_revision_files,
            scripts_checked_out_by_multiple_users,
        }))
    }
}

fn should_run(selected: Option<&std::collections::HashSet<String>>, check: &str) -> bool {
    selected.is_none_or(|checks| checks.contains(check))
}
