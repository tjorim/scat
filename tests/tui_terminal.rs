//! Wire-level regression checks use a real PTY, without cursor-query replies.
#[test]
fn terminal_protocol_regressions() {
    let status = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/tui_pty.py"
        ))
        .arg(env!("CARGO_BIN_EXE_scat"))
        .status()
        .expect("python3 is required for Unix PTY regression checks");
    assert!(status.success(), "PTY regression checks failed");
}
