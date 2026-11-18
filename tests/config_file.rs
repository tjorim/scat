//! Config file handling at the command line: an explicit path must exist and
//! mistakes in it must not be ignored silently.
use std::process::{Command, Output};

fn scat(args: &[&str], env: &[(&str, &std::path::Path)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_scat"));
    cmd.env_remove("SCAT_DB")
        .env_remove("SCAT_CONFIG")
        .env_remove("SCAT_SCAN_ROOTS")
        .args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().unwrap()
}

#[test]
fn missing_explicit_config_fails_instead_of_using_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.yaml");
    let missing_arg = missing.to_str().unwrap();

    let flag = scat(&["--config", missing_arg, "catalog", "stats"], &[]);
    assert!(!flag.status.success());
    let stderr = String::from_utf8_lossy(&flag.stderr);
    assert!(stderr.contains("config file not found"), "{stderr}");
    assert!(stderr.contains("nope.yaml"), "{stderr}");

    let env = scat(&["catalog", "stats"], &[("SCAT_CONFIG", &missing)]);
    assert!(!env.status.success());
    assert!(String::from_utf8_lossy(&env.stderr).contains("config file not found"));
}

#[test]
fn unknown_config_key_is_reported_with_the_file_and_key() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("scat.yaml");
    std::fs::write(&config, "scan_rootz:\n  - /a\n").unwrap();

    let output = scat(
        &["--config", config.to_str().unwrap(), "catalog", "stats"],
        &[],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("scan_rootz"), "{stderr}");
    assert!(stderr.contains("scat.yaml"), "{stderr}");
}

#[test]
fn commands_that_need_no_config_ignore_a_broken_one() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.yaml");
    for args in [&["completions", "bash"][..], &["man"][..]] {
        let output = scat(args, &[("SCAT_CONFIG", &missing)]);
        assert!(output.status.success(), "{args:?}");
    }
}
