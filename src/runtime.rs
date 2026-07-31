use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::{env, process};

use anyhow::Result;
use tracing_subscriber::EnvFilter;

use crate::output::print_json;

pub fn cmd_vc(args: &[String], json: bool, vc_executable: Option<PathBuf>) -> Result<()> {
    let vc_exe = vc_executable.or_else(which_vc);

    let Some(vc_exe) = vc_exe else {
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
    };

    let mut cmd = process::Command::new(vc_exe);
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

pub fn init_tracing(verbose: u8) {
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

pub fn effective_log_spec(verbose: u8, rust_log: Option<&OsStr>) -> Cow<'_, str> {
    match rust_log {
        Some(value) => value.to_string_lossy(),
        None => Cow::Borrowed(verbosity_directive(verbose)),
    }
}

pub fn verbosity_directive(verbose: u8) -> &'static str {
    match verbose {
        0 => "warn",
        1 => "debug",
        _ => "trace",
    }
}

fn which_vc() -> Option<PathBuf> {
    find_vc_on_path(env::var_os("PATH").as_deref())
}

fn find_vc_on_path(path_env: Option<&OsStr>) -> Option<PathBuf> {
    let path_env = path_env?;
    for dir in env::split_paths(path_env) {
        for name in ["vc", "vc.py"] {
            let path = dir.join(name);
            if is_executable_file(&path) {
                return Some(path);
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = path.metadata() else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

pub fn audit_exit_code(summary: &scat_core::core::search::AuditSummary, strict: bool) -> i32 {
    if summary.error > 0 {
        2
    } else {
        i32::from(strict && summary.warn > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, "").unwrap();
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

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

    #[test]
    fn find_vc_on_path_finds_vc_py_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("vc.py");
        make_executable(&executable);

        assert_eq!(
            find_vc_on_path(Some(dir.path().as_os_str())),
            Some(executable)
        );
    }

    #[test]
    fn find_vc_on_path_ignores_a_non_executable_candidate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("vc"), "").unwrap();

        assert_eq!(find_vc_on_path(Some(dir.path().as_os_str())), None);
    }
}
