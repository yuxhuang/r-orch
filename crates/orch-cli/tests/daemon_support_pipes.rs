//! The harness's own contract for reading a command's output (F-S1): a
//! finished command's output is returned even when another process still
//! holds its pipe open.
//!
//! On macOS a pipe created for a `Command` is not close-on-exec until a
//! second system call, so a process spawned by another thread in that window
//! (an `orch mcp` bridge or a tmux server in the stress test) inherits the
//! write end and keeps it for its whole life. Waiting for EOF forever made a
//! finished `orch task add` look like a client that hung for a minute.

mod daemon_support;

use daemon_support::*;
use std::process::Command;
use std::time::{Duration, Instant};

/// `sh` prints, starts a background `sleep` that inherits stdout/stderr, and
/// exits: the same shape as a pipe whose write end leaked into a long-lived
/// process. The sleeper's pid goes to `pid_file` so the test can reap it.
fn command_leaving_a_pipe_holder(pid_file: &std::path::Path) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(format!(
        "sleep 8 & echo $! > '{}'; echo out-text; echo err-text >&2; exit 3",
        pid_file.display()
    ));
    cmd
}

fn reap(pid_file: &std::path::Path) {
    if let Some(pid) = std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

#[test]
fn output_of_a_finished_command_is_returned_while_another_process_holds_its_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("sleeper.pid");
    let started = Instant::now();
    let (out, _) = output_and_pid(command_leaving_a_pipe_holder(&pid_file), CALL);
    let took = started.elapsed();
    reap(&pid_file);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(text(&out.stdout), "out-text\n");
    assert_eq!(text(&out.stderr), "err-text\n");
    assert!(
        took < Duration::from_secs(6),
        "waited {took:?} for a pipe a finished command no longer writes to"
    );
}

#[test]
fn output_of_a_command_that_closes_its_pipes_is_returned_without_waiting() {
    let started = Instant::now();
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg("echo out-text; echo err-text >&2");
    let out = output_with_deadline(cmd, CALL);
    assert_eq!(text(&out.stdout), "out-text\n");
    assert_eq!(text(&out.stderr), "err-text\n");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a command that closed its pipes was held for the EOF grace"
    );
}
