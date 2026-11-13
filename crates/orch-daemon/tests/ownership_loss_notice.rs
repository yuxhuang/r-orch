#![cfg(feature = "testing")]
//! The notice is independent of tracing, repeat refreshes and stderr errors.

#[macro_use]
mod support;

use orch_daemon::testing::DaemonHarness;
use serde_json::json;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use support::{Raw, ok};

const LINE: &str = "  ⚠ ownership lost: the project PID lock is missing or names another process; dispatch is off and this daemon is still running";
const CASE: &str = "OWNERSHIP_NOTICE_TEST_CASE";

// Removing the heartbeat's write must fail the missing/foreign cases. A
// panicking write must fail the broken-pipe case before status can be read.
#[test]
fn missing_lock_writes_once_over_further_refreshes() {
    let stderr = run_child("missing", false);
    assert_eq!(stderr.lines().collect::<Vec<_>>(), [LINE]);
}

#[test]
fn foreign_lock_writes_once_over_further_refreshes() {
    let stderr = run_child("foreign", false);
    assert_eq!(stderr.lines().collect::<Vec<_>>(), [LINE]);
}

#[test]
fn retained_lock_has_no_notice_over_three_refreshes() {
    assert_eq!(run_child("held", false), "");
}

#[test]
fn broken_stderr_still_records_loss_and_serves_status() {
    run_child("missing", true);
}

fn run_child(case: &str, broken: bool) -> String {
    let environment = tempfile::tempdir().unwrap();
    let home = environment.path().join("home");
    let tmp = environment.path().join("tmp");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&tmp).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "notice_child", "--nocapture"])
        .env_clear()
        .env("HOME", home)
        .env("TMPDIR", tmp)
        .env("PATH", "/usr/bin:/bin")
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            environment.path().join("global.yml"),
        )
        .env(CASE, case)
        .stdout(Stdio::piped());
    if broken {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        command.stderr(Stdio::from(writer));
    } else {
        command.stderr(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("notice child did not finish");
        }
        std::thread::yield_now();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("status checked after refreshes"));
    String::from_utf8(output.stderr).unwrap()
}

#[tokio::test(start_paused = true)]
async fn notice_child() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .dispatch(if case == "held" {
            orch_daemon::DispatchMode::Off
        } else {
            orch_daemon::DispatchMode::On
        })
        .start()
        .await
        .unwrap();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let lock = daemon.project_root().join(".orchestry/orchestry.lock");
    let initial = raw.request(1, "daemon.status", json!({})).await;
    assert_eq!(ok(&initial)["ownership"], "held");
    assert_eq!(ok(&initial)["dispatch"]["dispatch_on"], case != "held");
    match case.as_str() {
        "missing" => std::fs::remove_file(&lock).unwrap(),
        "foreign" => std::fs::write(&lock, "1\n").unwrap(),
        "held" => (),
        _ => panic!("unknown case"),
    }
    tokio::time::advance(Duration::from_secs(11)).await;
    let expected = if case == "held" { "held" } else { "lost" };
    let mut id = 10;
    eventually!("ownership status", {
        id += 1;
        let reply = raw.request(id, "daemon.status", json!({})).await;
        reply
            .get("ok")
            .is_some_and(|status| status["ownership"] == expected)
    });
    // Advance each period separately so a heartbeat that wrongly continued
    // would execute three times, rather than skip directly to one late tick.
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(11)).await;
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        id += 1;
        let reply = raw.request(id, "daemon.status", json!({})).await;
        assert_eq!(ok(&reply)["ownership"], expected);
        assert_eq!(ok(&reply)["dispatch"]["dispatch_on"], false);
    }
    if case == "missing" {
        assert!(!lock.exists());
    } else if case == "foreign" {
        assert_eq!(std::fs::read_to_string(&lock).unwrap(), "1\n");
    }
    println!("status checked after refreshes");
    daemon.shutdown().await.unwrap();
}
