//! Closing the read end of a pipe (`scat … | head`) must not panic or abort.

use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

#[test]
fn closed_stdout_pipe_ends_quietly_without_a_panic() {
    // `completions` writes a few hundred KiB, far past the pipe buffer, so
    // the write is guaranteed to hit the closed pipe.
    let mut child = Command::new(env!("CARGO_BIN_EXE_scat"))
        .args(["completions", "bash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "unexpected stderr: {stderr}"
    );
    // Default SIGPIPE: the kernel ends the process; never SIGABRT.
    assert_ne!(output.status.signal(), Some(libc::SIGABRT));
    assert!(
        output.status.signal() == Some(libc::SIGPIPE) || output.status.success(),
        "unexpected status: {:?}",
        output.status
    );
}
