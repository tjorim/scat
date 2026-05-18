use std::borrow::Cow;
use std::ffi::OsStr;
use std::process;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use crate::output::print_json;

pub(crate) fn cmd_vc(
    args: &[String],
    json: bool,
    vc_executable: Option<std::path::PathBuf>,
) -> Result<()> {
    let vc_exe = vc_executable
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(which_vc);

    if vc_exe.is_none() {
        if json {
            print_json(&serde_json::json!({
                "available": false,
                "returncode": 1,
                "stdout": "",
                "stderr": "vc not found on PATH"
            }));
        } else {
            println!("vc is unavailable; scat remains in read-only catalog mode.");
        }
        return Ok(());
    }

    let mut cmd = process::Command::new(vc_exe.unwrap());
    cmd.args(args);
    let output = cmd.output()?;
    let returncode = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if json {
        print_json(&serde_json::json!({
            "available": true,
            "returncode": returncode,
            "stdout": stdout,
            "stderr": stderr,
        }));
    } else {
        if !stdout.is_empty() {
            print!("{stdout}");
        }
        if !stderr.is_empty() {
            eprint!("{stderr}");
        }
    }

    if returncode != 0 {
        process::exit(returncode);
    }
    Ok(())
}

pub(crate) fn init_tracing(verbose: u8) {
    let filter = EnvFilter::new(effective_log_spec(
        verbose,
        std::env::var_os(EnvFilter::DEFAULT_ENV).as_deref(),
    ));

    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .compact()
        .try_init();
}

pub(crate) fn effective_log_spec<'a>(verbose: u8, rust_log: Option<&'a OsStr>) -> Cow<'a, str> {
    match rust_log {
        Some(value) => value.to_string_lossy(),
        None => Cow::Borrowed(verbosity_directive(verbose)),
    }
}

pub(crate) fn verbosity_directive(verbose: u8) -> &'static str {
    match verbose {
        0 => "warn",
        1 => "debug",
        _ => "trace",
    }
}

fn which_vc() -> Option<String> {
    ["vc", "vc.py"]
        .iter()
        .find(|name| {
            std::process::Command::new(if cfg!(windows) { "where" } else { "which" })
                .arg(name)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(|s| (*s).to_string())
}

pub(crate) fn audit_exit_code(
    summary: &scat_core::core::search::AuditSummary,
    strict: bool,
) -> i32 {
    if summary.error > 0 {
        2
    } else if strict && summary.warn > 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbosity_directive_maps_expected_levels() {
        assert_eq!(verbosity_directive(0), "warn");
        assert_eq!(verbosity_directive(1), "debug");
        assert_eq!(verbosity_directive(2), "trace");
        assert_eq!(verbosity_directive(3), "trace");
    }

    #[test]
    fn rust_log_overrides_verbose_flag() {
        assert_eq!(
            effective_log_spec(2, Some(OsStr::new("info,scat_core::core::db=trace"))),
            "info,scat_core::core::db=trace"
        );
    }

    #[test]
    fn audit_exit_code_prefers_errors() {
        let summary = scat_core::core::search::AuditSummary {
            error: 1,
            warn: 10,
            info: 10,
        };
        assert_eq!(audit_exit_code(&summary, false), 2);
        assert_eq!(audit_exit_code(&summary, true), 2);
    }

    #[test]
    fn audit_exit_code_warns_only_with_strict() {
        let summary = scat_core::core::search::AuditSummary {
            error: 0,
            warn: 1,
            info: 5,
        };
        assert_eq!(audit_exit_code(&summary, false), 0);
        assert_eq!(audit_exit_code(&summary, true), 1);
    }
}
