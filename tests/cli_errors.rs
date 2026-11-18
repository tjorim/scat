//! Error reporting, exit codes and output polish of the command line.
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn scat(args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_scat"));
    cmd.env_remove("SCAT_CONFIG")
        .env_remove("SCAT_DB")
        .env_remove("SCAT_SCAN_ROOTS")
        .env_remove("RUST_LOG")
        .args(args);
    cmd
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn failures_are_reported_even_when_logging_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.sqlite");
    let output = scat(&[
        "--db",
        missing.to_str().unwrap(),
        "--no-cache",
        "catalog",
        "stats",
    ])
    .env("RUST_LOG", "off")
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let (_, stderr) = text(&output);
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("none.sqlite"), "{stderr}");
    assert!(!stderr.contains("command failed"), "log-shaped: {stderr}");
}

#[test]
fn vc_missing_exits_non_zero_in_both_forms() {
    let empty = tempfile::tempdir().unwrap();
    let plain = scat(&["vc", "--", "list"])
        .env("PATH", empty.path())
        .output()
        .unwrap();
    assert_eq!(plain.status.code(), Some(1));
    assert!(text(&plain).1.contains("vc is unavailable"));

    let json = scat(&["vc", "--json", "--", "list"])
        .env("PATH", empty.path())
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_str(&text(&json).0).unwrap();
    assert_eq!(value["available"], false);
    assert_eq!(value["returncode"], 1);
}

fn fake_vc(dir: &Path, body: &str) {
    let path = dir.join("vc");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn vc_exit_status_is_passed_through_including_signals() {
    let dir = tempfile::tempdir().unwrap();

    fake_vc(dir.path(), "exit 7");
    let exited = scat(&["vc", "--", "x"])
        .env("PATH", dir.path())
        .output()
        .unwrap();
    assert_eq!(exited.status.code(), Some(7));

    // Killed by SIGKILL: reported as 128 + 9, not as a generic 1.
    fake_vc(dir.path(), "kill -9 $$");
    let killed = scat(&["vc", "--", "x"])
        .env("PATH", dir.path())
        .output()
        .unwrap();
    assert_eq!(killed.status.code(), Some(137));

    let json = scat(&["vc", "--json", "--", "x"])
        .env("PATH", dir.path())
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&text(&json).0).unwrap();
    assert_eq!(value["returncode"], 137);
}

fn build_small_catalog(dir: &Path) -> std::path::PathBuf {
    let root = dir.join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.py"), "import requests\nimport yaml\n").unwrap();
    std::fs::write(root.join("b.py"), "import json\n").unwrap();
    let db = dir.join("cat.sqlite");
    let output = scat(&[
        "--db",
        db.to_str().unwrap(),
        "catalog",
        "build",
        "--scan-root",
        root.to_str().unwrap(),
        "-q",
    ])
    .output()
    .unwrap();
    assert!(output.status.success(), "{:?}", text(&output));
    db
}

#[test]
fn catalog_info_prints_the_timestamp_unquoted() {
    let dir = tempfile::tempdir().unwrap();
    let db = build_small_catalog(dir.path());
    let output = scat(&[
        "--db",
        db.to_str().unwrap(),
        "--no-cache",
        "catalog",
        "info",
    ])
    .output()
    .unwrap();
    let (stdout, _) = text(&output);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("Last indexed"))
        .unwrap();
    assert!(!line.contains('"'), "{line}");
    assert!(line.contains("20"), "{line}");
}

#[test]
fn help_does_not_leak_internal_notes() {
    let output = scat(&["--help"]).output().unwrap();
    let (stdout, _) = text(&output);
    assert!(stdout.contains("--no-cache"));
    assert!(!stdout.contains("resolve_no_cache"), "{stdout}");
    assert!(stdout.contains("SCAT_NO_CACHE"));
}

#[test]
fn incremental_summary_reports_the_catalog_total_of_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let db = build_small_catalog(dir.path());
    let root = dir.path().join("root");
    let run = |extra: &[&str]| {
        let mut args = vec![
            "--db",
            db.to_str().unwrap(),
            "catalog",
            "build",
            "--scan-root",
            root.to_str().unwrap(),
            "--force",
        ];
        args.extend_from_slice(extra);
        let output = scat(&args).output().unwrap();
        assert!(output.status.success(), "{:?}", text(&output));
        text(&output).0
    };
    // Every script is reused, so none were newly extracted: the summary must
    // still describe the catalog, not "(0 dependencies)".
    let summary = run(&[]);
    assert!(summary.contains("reused unchanged"), "{summary}");
    assert!(summary.contains("(3 dependencies)"), "{summary}");

    let json: serde_json::Value = serde_json::from_str(&run(&["--json"])).unwrap();
    assert_eq!(json["dependencies_indexed"], 0);
    assert_eq!(json["dependencies_total"], 3);
}
