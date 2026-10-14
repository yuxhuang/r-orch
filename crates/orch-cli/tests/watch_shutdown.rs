//! `run --watch` and `serve` must stop when they are signalled.
//!
//! Both block in `Orchestrator::wait_for_stop`, which is released by `stop()`
//! — and `stop()` runs *inside the watch task* when the trigger is a signal or
//! the tick circuit breaker. A `stop()` that cannot finish there leaves the
//! process alive: `Ctrl+C` appears to do nothing, and a service manager's
//! `SIGTERM` is followed by `SIGKILL`, leaving `.orchestry/orchestry.lock`
//! behind and the agents orphaned.
//!
//! This is a binary-level test because that is the only level where it is
//! real: the in-memory fakes never make `stop()` wait for anything, so the
//! orchestrator's own tests cannot see it.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn orch_binary() -> std::path::PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("orch")
}

fn initialized_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let status = Command::new(orch_binary())
        .args(["init", "--name", "proj", "--adapter", "claude"])
        .current_dir(dir.path())
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .stdout(Stdio::null())
        .status()
        .expect("init runs");
    assert!(status.success(), "init failed");
    dir
}

/// Signal `pid` and wait for it to exit, returning how long that took.
fn signal_and_wait(
    child: &mut std::process::Child,
    signal: &str,
    within: Duration,
) -> Option<Duration> {
    let signalled = Instant::now();
    let _ = Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status();

    while signalled.elapsed() < within {
        match child.try_wait().expect("poll the child") {
            Some(_) => return Some(signalled.elapsed()),
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

#[test]
fn run_watch_exits_on_sigint() {
    let dir = initialized_project();
    let mut child = Command::new(orch_binary())
        .args(["run", "--watch"])
        .current_dir(dir.path())
        .env("HOME", dir.path().join("home"))
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn run --watch");

    // The first tick has to have happened, or `stop()` finds no loaded state
    // and the interesting path — a state save inside the watch task — is not
    // exercised at all.
    std::thread::sleep(Duration::from_millis(1_500));

    let elapsed = signal_and_wait(&mut child, "-INT", Duration::from_secs(15));
    let mut stdout = String::new();
    if let Some(pipe) = child.stdout.as_mut() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    assert!(
        elapsed.is_some(),
        "run --watch ignored SIGINT and had to be killed; stdout so far:\n{stdout}"
    );
    assert!(
        stdout.contains("Shutting down"),
        "the shutdown was never reported:\n{stdout}"
    );
    // The lock is released on the way out, so the next invocation is not
    // locked out by a corpse.
    assert!(
        !dir.path().join(".orchestry/orchestry.lock").exists(),
        "the lock file outlived the process"
    );
}

#[test]
fn serve_exits_on_sigterm() {
    // The daemon case, and the one that matters most: a service manager sends
    // `SIGTERM` and escalates to `SIGKILL` if the process does not go, which
    // would leave the lock file behind for every later invocation.
    let dir = initialized_project();
    let mut child = Command::new(orch_binary())
        .args(["serve", "--log-format", "json"])
        .current_dir(dir.path())
        .env("HOME", dir.path().join("home"))
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");

    std::thread::sleep(Duration::from_millis(1_500));

    let elapsed = signal_and_wait(&mut child, "-TERM", Duration::from_secs(15));
    let mut stdout = String::new();
    if let Some(pipe) = child.stdout.as_mut() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    assert!(
        elapsed.is_some(),
        "serve ignored SIGTERM and had to be killed; stdout so far:\n{stdout}"
    );
    assert!(
        stdout.contains("\"event\":\"orchestrator:shutdown\""),
        "the shutdown was never logged:\n{stdout}"
    );
    assert!(
        !dir.path().join(".orchestry/orchestry.lock").exists(),
        "the lock file outlived the process"
    );
}
