use serde_json::Value;
use std::{fs, process::Command};

fn doctor(args: &[&str], path: &std::path::Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_scat"))
        .env_remove("SCAT_DB")
        .env_remove("SCAT_CONFIG")
        .env("PATH", path)
        .env("SCAT_EDITOR", "missing-viewer --secret=do-not-print")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("do-not-print"));
    serde_json::from_str(&text).unwrap()
}
#[test]
fn offline_without_tools_catalog_or_rpm_is_useful() {
    let dir = tempfile::tempdir().unwrap();
    let r = doctor(
        &["doctor", "--json", "/no-such-script", "--rpm"],
        dir.path(),
    );
    assert_eq!(r["schema_version"], 1);
    assert_eq!(r["catalog"]["state"], "not_configured");
    assert_eq!(r["policy"]["selinux"]["state"], "unavailable");
    assert_eq!(r["script"]["rpm"]["state"], "unverifiable");
    assert_eq!(r["viewers"][0]["path"], Value::Null);
}
#[test]
fn malformed_config_and_unresolved_mapping_do_not_abort_or_leak() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "bad: [secret-config-value").unwrap();
    let r = doctor(
        &[
            "--config",
            config.to_str().unwrap(),
            "doctor",
            "/logical/file",
            "--mapping",
            "/no-such-mapping",
            "--json",
        ],
        dir.path(),
    );
    assert_eq!(r["catalog"]["state"], "unknown");
    assert_eq!(r["script"]["state"], "unknown");
    assert!(!r.to_string().contains("secret-config-value"));
}

#[test]
fn policy_fixtures_keep_enforcement_and_evidence_separate() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let getenforce = dir.path().join("getenforce");
    let audit = dir.path().join("ausearch");
    fs::write(&audit, "#!/bin/sh\nprintf 'type=AVC msg=audit(1): avc: denied { read } path=secret-audit-detail\\n'\n").unwrap();
    fs::set_permissions(&audit, fs::Permissions::from_mode(0o700)).unwrap();
    for mode in ["Enforcing", "Permissive", "Disabled"] {
        fs::write(&getenforce, format!("#!/bin/sh\nprintf '{mode}\\n'\n")).unwrap();
        fs::set_permissions(&getenforce, fs::Permissions::from_mode(0o700)).unwrap();
        let r = doctor(&["doctor", "/selected", "--json"], dir.path());
        assert_eq!(r["policy"]["selinux"]["stdout"], format!("{mode}\n"));
        assert_eq!(r["audit"]["selinux"]["state"], "observed");
        assert_eq!(r["audit"]["selinux"]["denial_marker_count"], 1);
        assert!(!r.to_string().contains("secret-audit-detail"));
    }
    fs::write(
        &audit,
        "#!/bin/sh\necho 'Cannot open audit log' >&2\nexit 1\n",
    )
    .unwrap();
    let r = doctor(&["doctor", "/selected", "--json"], dir.path());
    assert_eq!(r["audit"]["selinux"]["state"], "unknown");
}
