use std::process::Command;

#[test]
fn man_dir_writes_a_page_per_command_including_nested_ones() {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_scat"))
        .args(["man", "--dir"])
        .arg(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    for page in [
        "scat.1",
        "scat-search.1",
        "scat-catalog-audit.1",
        "scat-doctor.1",
    ] {
        let text = std::fs::read_to_string(dir.path().join(page)).unwrap();
        assert!(text.contains(".TH"), "{page} is not a roff page");
    }
}

#[test]
fn man_without_dir_prints_the_top_level_page() {
    let output = Command::new(env!("CARGO_BIN_EXE_scat"))
        .arg("man")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains(".TH scat 1")
    );
}
