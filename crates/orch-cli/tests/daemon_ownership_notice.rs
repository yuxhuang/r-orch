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
