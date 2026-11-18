//! Cancellable capture for background work. Capture files avoid pipe deadlocks;
//! a separate process group lets cancellation stop descendants as well.
use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Seek, SeekFrom},
    os::unix::process::CommandExt,
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Send `SIGKILL` to the process group `process_group(0)` made for `child`.
///
/// Only sound while `child` has not been reaped: its pid is also the group id,
/// and an unreaped (even exited) leader keeps that id reserved. Once reaped, an
/// unrelated process could in principle be given the same id and, as a new
/// group leader, be hit by this signal.
fn kill_group(child: &Child) {
    // SAFETY: process_group(0) creates a group identified by this child PID.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}

/// Whether `child` has exited, *without* reaping it (`WNOWAIT`), so that its
/// pid — the group id — stays reserved until the caller has dealt with any
/// descendants and calls [`Child::wait`].
fn has_exited(child: &Child) -> std::io::Result<bool> {
    // SAFETY: `info` is plain data, valid when zeroed; `waitid` only writes it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            libc::id_t::from(child.id()),
            &raw mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // With WNOHANG and nothing to report, `si_pid` is left at 0.
    // SAFETY: reading the `si_pid` field of a siginfo that `waitid` filled in.
    Ok(unsafe { info.si_pid() } != 0)
}

struct Running(Option<Child>);
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            kill_group(&child);
            let _ = child.kill();
            // A process stuck in kernel filesystem I/O must not hold up the UI.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

pub fn capture(
    command: &mut Command,
    cancel: &AtomicBool,
    timeout: Option<Duration>,
) -> Result<Output> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Operation canceled");
    }
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut running = Running(Some(
        command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?)
            .spawn()
            .context("Could not start background process")?,
    ));
    let start = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("Operation canceled");
        }
        if timeout.is_some_and(|timeout| start.elapsed() >= timeout) {
            bail!("Operation timed out");
        }
        if has_exited(running.0.as_ref().unwrap())? {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Kill any descendants which outlived the direct child before reading
    // captures — while the child is still unreaped, so the group id can't have
    // been recycled — and only then reap it.
    let mut child = running.0.take().unwrap();
    kill_group(&child);
    let status = child.wait()?;
    stdout.seek(SeekFrom::Start(0))?;
    stderr.seek(SeekFrom::Start(0))?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    stdout.read_to_end(&mut out)?;
    stderr.read_to_end(&mut err)?;
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn captures_both_streams_and_nonzero_status() {
        let output = capture(
            Command::new("/bin/sh").args(["-c", "printf stdout; printf stderr >&2; exit 2"]),
            &AtomicBool::new(false),
            None,
        )
        .unwrap();
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
        assert_eq!(output.status.code(), Some(2));
    }

    #[test]
    fn timeout_returns_without_waiting_for_slow_process() {
        let start = Instant::now();
        let err = capture(
            Command::new("/bin/sh").args(["-c", "sleep 60"]),
            &AtomicBool::new(false),
            Some(Duration::from_millis(40)),
        )
        .unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn descendants_that_outlive_the_direct_child_are_killed() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let script = format!("sleep 60 & echo $! > '{}'; exit 0", pid_file.display());
        let output = capture(
            Command::new("/bin/sh").args(["-c", &script]),
            &AtomicBool::new(false),
            None,
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(0));

        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SIGKILL is delivered asynchronously and init still has to reap it.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: signal 0 only checks that the process exists.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(Instant::now() < deadline, "background process survived");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn cancellation_stops_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let late = dir.path().join("late");
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let marker = late.clone();
        let signal = ready.clone();
        let worker = std::thread::spawn(move || {
            capture(
                Command::new("/bin/sh")
                    .args([
                        "-c",
                        "(sleep 0.5; touch \"$2\") & touch \"$1\"; wait",
                        "probe",
                    ])
                    .arg(signal)
                    .arg(marker),
                &worker_cancel,
                None,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(
            worker
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("canceled")
        );
        std::thread::sleep(Duration::from_millis(650));
        assert!(!late.exists(), "descendant survived cancellation");
    }
}
