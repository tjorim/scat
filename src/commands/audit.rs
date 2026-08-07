use std::path::Path;
use std::process;

use anyhow::Result;
use scat_core::core::embeddings::EmbeddingsSidecar;
use scat_core::core::search::AuditOptions;

use crate::output::print_json;
use crate::runtime::audit_exit_code;

#[allow(clippy::too_many_arguments)]
pub fn cmd_audit(
    api: &scat_core::core::search::SearchApi,
    sidecar_path: &Path,
    checks: &[String],
    strict: bool,
    stale_days: i64,
    near_duplicate_threshold: f32,
    outlier_threshold: f32,
    json: bool,
) -> Result<()> {
    let selected = (!checks.is_empty()).then_some(checks);
    let sidecar = EmbeddingsSidecar::open(sidecar_path);
    let options = AuditOptions {
        sidecar: sidecar.as_ref(),
        near_duplicate_threshold,
        outlier_threshold,
    };
    let result = api.audit(selected, stale_days, options)?;

    if json {
        print_json(&result);
    } else {
        println!("scat audit — {} findings", result.findings.len());
        println!();
        for finding in &result.findings {
            println!(
                "{:<5} {:<16} {} — {}",
                finding.severity.to_uppercase(),
                finding.check,
                finding.logical_path,
                finding.detail
            );
        }
        if !result.findings.is_empty() {
            println!();
        }
        println!(
            "Summary: {} error, {} warn, {} info",
            result.summary.error, result.summary.warn, result.summary.info
        );
    }

    let exit_code = audit_exit_code(&result.summary, strict);
    if exit_code != 0 {
        process::exit(exit_code);
    }
    Ok(())
}
