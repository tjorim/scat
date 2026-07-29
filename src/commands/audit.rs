use std::process;

use anyhow::Result;

use crate::output::print_json;
use crate::runtime::audit_exit_code;

pub(crate) fn cmd_audit(
    api: &scat_core::core::search::SearchApi,
    checks: &[String],
    strict: bool,
    stale_days: i64,
    json: bool,
) -> Result<()> {
    let selected = (!checks.is_empty()).then_some(checks);
    let result = api.audit(selected, stale_days)?;

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
