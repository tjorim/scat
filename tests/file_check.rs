use std::{os::unix::ffi::OsStringExt, process::Command};

#[test]
fn isolated_file_probe_needs_no_catalog_or_config_and_handles_native_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join(std::ffi::OsString::from_vec(b"native-\xff.py".to_vec()));
    std::fs::write(&path, "print(1)").unwrap();
    for exists in [true, false] {
        if !exists {
            std::fs::remove_file(&path).unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_scat"))
            .arg("file-check")
            .arg(&path)
            .env("SCAT_CONFIG", dir.path().join("missing-config"))
            .env("SCAT_DB", dir.path().join("missing-catalog"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, format!("{exists}\n").as_bytes());
    }
}

#[test]
fn cli_rejects_invalid_search_limits_before_catalog_access() {
    for limit in ["0", "18446744073709551615"] {
        let output = Command::new(env!("CARGO_BIN_EXE_scat"))
            .args(["search", "--limit", limit])
            .env_remove("SCAT_CONFIG")
            .env_remove("SCAT_DB")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Result limit"));
    }
}
