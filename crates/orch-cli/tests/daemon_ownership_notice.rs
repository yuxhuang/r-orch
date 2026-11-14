//! Real start modes and tracing setup need the CLI binary. Repeat periods
//! and stderr failures use the daemon harness's paused-time seam instead.

mod daemon_support;

use daemon_support::*;
use std::fs;
use std::path::Path;

const LINE: &str = "  ⚠ ownership lost: the project PID lock is missing or names another process; dispatch is off and this daemon is still running";

fn lost(sandbox: &Sandbox) {
    fs::remove_file(sandbox.state_dir().join("orchestry.lock")).unwrap();
    wait_for(READY, "lost ownership", || {
        sandbox.status().ok().filter(|status| {
            status.ownership == orch_proto::ops::results::OwnershipStatus::Lost
                && !status.dispatch.dispatch_on
        })
    });
}

fn one_line(path: &Path) {
    wait_for(READY, "the fixed stderr notice", || {
        let text = fs::read_to_string(path).unwrap_or_default();
        text.lines().any(|line| line == LINE).then_some(())
    });
    let text = fs::read_to_string(path).unwrap();
    assert_eq!(
        text.lines().filter(|line| *line == LINE).count(),
        1,
        "{text}"
    );
}

#[test]
fn foreground_default_notice() {
    foreground(false);
}

#[test]
fn foreground_explicit_tracing_notice() {
    foreground(true);
}

fn foreground(logging: bool) {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let mut command = sandbox.start_cmd(&[]);
    // Sandbox clears the environment, so the default case has neither setting.
    if logging {
        command.env("RUST_LOG", "error");
    }
    let stderr = sandbox.short_dir().join("notice.err");
    let mut child = DaemonChild::spawn(command, stderr.clone());
    child.expect_ready(&sandbox.socket());
    lost(&sandbox);
    one_line(&stderr);
    assert!(sandbox.status().is_ok());
    let stop = sandbox.run(&["daemon", "stop"]);
    assert!(stop.status.success(), "{stop:?}");
    assert_eq!(child.wait_exit(READY).code(), Some(0));
    one_line(&stderr);
    let text = fs::read_to_string(stderr).unwrap();
    if logging {
        assert!(
            text.contains("ownership_lost: the project PID lock"),
            "{text}"
        );
    } else {
        assert_eq!(text, format!("{LINE}\n"));
    }
}

#[test]
fn detached_default_notice_in_daemon_log() {
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let output = sandbox.run(&["daemon", "start", "--detach"]);
    assert!(output.status.success(), "{output:?}");
    let status = sandbox.status().unwrap();
    let _owned = KillOnDrop(status.pid);
    lost(&sandbox);
    let log = sandbox.state_dir().join("daemon.log");
    one_line(&log);
    assert!(sandbox.status().is_ok());
    let stop = sandbox.run(&["daemon", "stop"]);
    assert!(stop.status.success(), "{stop:?}");
    wait_dead(status.pid, "detached daemon to exit");
    one_line(&log);
    assert_eq!(fs::read_to_string(log).unwrap(), format!("{LINE}\n"));
}

const PROGRESS: std::time::Duration = std::time::Duration::from_secs(3);

// Each child is ours, including on an assertion's unwind. Unlike DaemonChild,
// this fixture leaves stderr unread until the test explicitly drains it.
struct PipeDaemon(std::process::Child);

impl Drop for PipeDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn full_stderr_one_worker_status_request_and_stop() {
    full_stderr(1, false);
}

#[test]
fn full_stderr_two_workers_status_request_and_stop() {
    full_stderr(2, false);
}

#[test]
fn full_stderr_drained_later_delivers_the_notice() {
    full_stderr(1, true);
}

fn full_stderr(workers: usize, drain_later: bool) {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Instant;

    let started = Instant::now();
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let (reader, mut filler) = std::io::pipe().unwrap();
    let mut command = sandbox.start_cmd(&["--dispatch", "on"]);
    command
        .env("TOKIO_WORKER_THREADS", workers.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(filler.try_clone().unwrap());
    let mut daemon = PipeDaemon(command.spawn().unwrap());
    drop(command);
    let stdout = daemon.0.stdout.take().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.starts_with("listening ") {
                let _ = ready_tx.send(line);
                return;
            }
        }
    });
    assert_eq!(
        ready_rx.recv_timeout(READY).unwrap(),
        format!("listening {}", sandbox.socket().display())
    );
    let call = |args: &[&str]| {
        let mut command = sandbox.orch();
        command.args(args);
        let started = Instant::now();
        let output = output_with_deadline(command, PROGRESS);
        assert!(
            started.elapsed() < PROGRESS,
            "{args:?} exceeded {PROGRESS:?}"
        );
        output
    };
    let status = call(&["--json", "daemon", "status"]);
    assert!(status.status.success(), "{status:?}");
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["ownership"], "held");
    assert_eq!(status["dispatch"]["dispatch_on"], true);

    // Saturate AFTER readiness. The temporary nonblocking flag is restored
    // before the loss; the daemon therefore receives a genuinely blocking fd.
    let flags = OFlag::from_bits_retain(fcntl(&filler, FcntlArg::F_GETFL).unwrap());
    fcntl(&filler, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
    let mut filled = 0;
    for size in [4096, 1] {
        let bytes = vec![b'x'; size];
        loop {
            match filler.write(&bytes) {
                Ok(n) => {
                    assert!(n > 0);
                    filled += n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("fill stderr: {e}"),
            }
        }
    }
    fcntl(&filler, FcntlArg::F_SETFL(flags)).unwrap();
    assert!(filled > 0);
    assert!(
        !OFlag::from_bits_retain(fcntl(&filler, FcntlArg::F_GETFL).unwrap())
            .contains(OFlag::O_NONBLOCK)
    );
    drop(filler);
    fs::remove_file(sandbox.state_dir().join("orchestry.lock")).unwrap();

    let deadline = Instant::now() + READY;
    let mut tolerated_cancellation = false;
    loop {
        let status = call(&["--json", "daemon", "status"]);
        if ownership_loss_probe(&mut tolerated_cancellation, status) {
            break;
        }
        assert!(Instant::now() < deadline, "ownership did not become lost");
        std::thread::yield_now();
    }
    eprintln!("workers={workers}: LOST/OFF; context request next");
    let request = call(&["context", "set", "notice-probe", "accepted"]);
    assert!(request.status.success(), "{request:?}");

    // Keep ownership of the reader explicit across the two drain choices.
    let mut reader = Some(reader);
    let capture = if drain_later {
        let mut reader = reader.take().unwrap();
        // Do not wait for EOF: a live daemon still owns stderr. Observe the
        // complete line by an event with a deadline, then drain through exit.
        let (line_tx, line_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut captured = vec![0; filled];
            reader.read_exact(&mut captured).unwrap();
            assert!(captured.iter().all(|b| *b == b'x'));
            let mut buffered = BufReader::new(reader);
            let mut line = String::new();
            buffered.read_line(&mut line).unwrap();
            let _ = line_tx.send(line.clone());
            buffered.read_to_string(&mut line).unwrap();
            let _ = done_tx.send(line);
        });
        assert_eq!(line_rx.recv_timeout(PROGRESS).unwrap(), format!("{LINE}\n"));
        Some(done_rx)
    } else {
        None
    };
    eprintln!("workers={workers}: request succeeded; stop next");
    let stop_started = Instant::now();
    let stop = call(&["daemon", "stop"]);
    assert!(stop.status.success(), "{stop:?}");
    let deadline = stop_started + PROGRESS;
    loop {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            assert_eq!(status.code(), Some(0));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "clean stop exceeded {PROGRESS:?}"
        );
        std::thread::yield_now();
    }
    assert!(
        stop_started.elapsed() < PROGRESS,
        "stop exceeded {PROGRESS:?}"
    );
    eprintln!(
        "workers={workers} drain_later={drain_later}: stop={:?}, total={:?}, filled={filled}",
        stop_started.elapsed(),
        started.elapsed()
    );
    if let Some(capture) = capture {
        assert_eq!(capture.recv_timeout(PROGRESS).unwrap(), format!("{LINE}\n"));
    } else {
        // The reader stayed open and unread until clean exit. Only filler
        // remains: the pending notice disappeared with the process.
        let (tx, rx) = mpsc::channel();
        let mut reader = reader.take().unwrap();
        std::thread::spawn(move || {
            let mut captured = Vec::new();
            reader.read_to_end(&mut captured).unwrap();
            let _ = tx.send(captured);
        });
        assert_eq!(rx.recv_timeout(PROGRESS).unwrap(), vec![b'x'; filled]);
    }
}

// A registered status request can be cancelled by the ownership-loss sweep.
// Sequential probes permit that exact response once, then require Lost/OFF.
fn ownership_loss_probe(tolerated_cancellation: &mut bool, output: std::process::Output) -> bool {
    if !*tolerated_cancellation
        && output.status.code() == Some(1)
        && output.stdout.is_empty()
        && output.stderr == "  ✕ request cancelled before execution\n".as_bytes()
    {
        *tolerated_cancellation = true;
        eprintln!(
            "{:?}: tolerated transition-time cancellation: {output:?}",
            std::thread::current().name()
        );
        return false;
    }
    assert!(
        output.status.success(),
        "unexpected ownership-loss probe: {output:?}"
    );
    assert!(
        output.stderr.is_empty(),
        "unexpected status stderr: {output:?}"
    );
    let status: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("invalid ownership-loss status JSON");
    let dispatch_on = status["dispatch"]["dispatch_on"]
        .as_bool()
        .expect("missing or invalid dispatch_on");
    match status["ownership"].as_str() {
        Some("lost") => {
            assert!(!dispatch_on, "lost ownership with dispatch on: {status:?}");
            true
        }
        Some("held") => {
            assert!(
                !*tolerated_cancellation,
                "expected lost after transition-time cancellation: {status:?}"
            );
            false
        }
        _ => panic!("unexpected ownership-loss status: {status:?}"),
    }
}

mod ownership_loss_poll_tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::process::{ExitStatus, Output};

    fn reply(code: i32, stdout: &[u8], stderr: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    fn status(ownership: &str, dispatch_on: bool) -> Output {
        reply(
            0,
            serde_json::json!({
                "ownership": ownership,
                "dispatch": {"dispatch_on": dispatch_on},
            })
            .to_string()
            .as_bytes(),
            b"",
        )
    }

    fn refusal() -> Output {
        reply(
            1,
            b"",
            "  ✕ request cancelled before execution\n".as_bytes(),
        )
    }

    fn decisions(replies: Vec<Output>) -> (Vec<bool>, bool) {
        let mut tolerated = false;
        let decisions = replies
            .into_iter()
            .map(|reply| ownership_loss_probe(&mut tolerated, reply))
            .collect();
        (decisions, tolerated)
    }

    #[test]
    fn held_then_lost_off_completes_without_tolerance() {
        assert_eq!(
            decisions(vec![status("held", true), status("lost", false)]),
            (vec![false, true], false)
        );
    }

    #[test]
    fn repeated_held_then_lost_off_completes_without_tolerance() {
        assert_eq!(
            decisions(vec![
                status("held", true),
                status("held", true),
                status("lost", false)
            ]),
            (vec![false, false, true], false)
        );
    }

    #[test]
    fn refusal_then_lost_off_completes() {
        assert_eq!(
            decisions(vec![refusal(), status("lost", false)]),
            (vec![false, true], true)
        );
    }

    #[test]
    fn held_then_refusal_then_lost_off_completes() {
        assert_eq!(
            decisions(vec![status("held", true), refusal(), status("lost", false)]),
            (vec![false, false, true], true)
        );
    }

    #[test]
    #[should_panic(expected = "unexpected ownership-loss probe:")]
    fn refusal_then_refusal_fails_immediately() {
        decisions(vec![refusal(), refusal()]);
    }

    #[test]
    #[should_panic(expected = "expected lost after transition-time cancellation:")]
    fn refusal_then_held_fails_immediately() {
        decisions(vec![refusal(), status("held", true)]);
    }

    #[test]
    #[should_panic(expected = "unexpected ownership-loss probe:")]
    fn refusal_then_unrelated_error_fails_immediately() {
        decisions(vec![refusal(), reply(7, b"", b"daemon unavailable\n")]);
    }

    #[test]
    fn wrong_exit_stdout_stderr_and_signals_fail_before_or_after_refusal() {
        let mut signal = refusal();
        signal.status = ExitStatus::from_raw(15);
        let mut stdout = refusal();
        stdout.stdout = b"unexpected".to_vec();
        let mut exit = refusal();
        exit.status = ExitStatus::from_raw(7 << 8);
        let mut success = refusal();
        success.status = ExitStatus::from_raw(0);
        let mut whitespace = refusal();
        whitespace.stderr.insert(0, b' ');
        let mut newline = refusal();
        newline.stderr.pop();
        let mut trailing = refusal();
        trailing.stderr.extend_from_slice(b"extra\n");
        let mut success_stderr = status("lost", false);
        success_stderr.stderr = b"unexpected\n".to_vec();
        let invalid = [
            ("signal", signal),
            ("stdout", stdout),
            ("exit", exit),
            ("success cancellation", success),
            ("whitespace", whitespace),
            ("newline", newline),
            ("trailing bytes", trailing),
            ("other stderr", reply(1, b"", b"another error\n")),
            ("success stderr", success_stderr),
        ];
        for (name, output) in invalid {
            for after_refusal in [false, true] {
                let mut tolerated = false;
                if after_refusal {
                    ownership_loss_probe(&mut tolerated, refusal());
                }
                assert!(
                    catch_unwind(AssertUnwindSafe(|| ownership_loss_probe(
                        &mut tolerated,
                        output.clone()
                    )))
                    .is_err(),
                    "accepted {name}, after_refusal={after_refusal}: {output:?}"
                );
            }
        }
    }

    #[test]
    fn malformed_or_invalid_status_and_lost_on_fail_before_or_after_refusal() {
        let mut invalid = vec![status("lost", true), status("unknown", false)];
        for json in [
            "",
            "{",
            "{}",
            "[]",
            r#"{"ownership":"held"}"#,
            r#"{"ownership":"lost","dispatch":{"dispatch_on":"false"}}"#,
        ] {
            invalid.push(reply(0, json.as_bytes(), b""));
        }
        for output in invalid {
            for after_refusal in [false, true] {
                let mut tolerated = false;
                if after_refusal {
                    ownership_loss_probe(&mut tolerated, refusal());
                }
                assert!(
                    catch_unwind(AssertUnwindSafe(|| ownership_loss_probe(
                        &mut tolerated,
                        output.clone()
                    )))
                    .is_err(),
                    "accepted invalid status, after_refusal={after_refusal}: {output:?}"
                );
            }
        }
    }
}
