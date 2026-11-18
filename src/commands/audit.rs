use std::path::Path;
use std::process;

use anyhow::Result;
use scat_core::core::embeddings::EmbeddingsSidecar;
use scat_core::core::search::AuditOptions;

use crate::output::print_json;
use crate::runtime::audit_exit_code;

/// The `scat catalog audit` flags.
#[derive(Debug, Clone, Copy)]
pub struct AuditOpts<'a> {
    /// `--check` (empty runs every default check).
    pub checks: &'a [String],
    /// `--strict`: warnings fail the run too.
    pub strict: bool,
    /// `--stale-days`.
    pub stale_days: i64,
    /// `--near-duplicate-threshold`.
    pub near_duplicate_threshold: f32,
    /// `--outlier-threshold`.
    pub outlier_threshold: f32,
    /// `--json`.
    pub json: bool,
}

pub fn cmd_audit(
    api: &scat_core::core::search::SearchApi,
    sidecar_path: &Path,
    opts: AuditOpts<'_>,
) -> Result<()> {
    let AuditOpts {
        checks,
        strict,
        stale_days,
        near_duplicate_threshold,
        outlier_threshold,
        json,
    } = opts;
    let selected = (!checks.is_empty()).then_some(checks);
    // Audit works without a sidecar, but an unusable one is said out loud
    // rather than quietly skipping the checks that need it.
    let sidecar = EmbeddingsSidecar::open_checked(sidecar_path).unwrap_or_else(|err| {
        eprintln!("warning: {err}; skipping embedding-based checks");
        None
    });
    let options = AuditOptions {
        sidecar: sidecar.as_ref(),
        near_duplicate_threshold,
        outlier_threshold,
    };
    let result = api.audit(selected, stale_days, options)?;

    if json {
        print_json(&result);
    } else {
        outln!("scat audit — {} findings", result.findings.len());
        outln!();
        for finding in &result.findings {
            outln!(
                "{:<5} {:<16} {} — {}",
                finding.severity.to_uppercase(),
                finding.check,
                finding.logical_path,
                finding.detail
            );
        }
        if !result.findings.is_empty() {
            outln!();
        }
        outln!(
            "Summary: {} error, {} warn, {} info",
            result.summary.error,
            result.summary.warn,
            result.summary.info
        );
    }

    let exit_code = audit_exit_code(&result.summary, strict);
    if exit_code != 0 {
        process::exit(exit_code);
    }
    Ok(())
}
