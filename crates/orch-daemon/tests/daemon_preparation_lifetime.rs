#![cfg(feature = "testing")]
//! SRE 8.22: synchronous adapter preparation cannot outlive project ownership.

mod support;

use async_trait::async_trait;
use orch_core::process::{ExitOutcome, ExitSignal, SpawnOptions, SpawnedChild};
use orch_core::{ProcessManager, Result, Signal};
use orch_daemon::testing::DaemonHarness;
use orch_domain::RunStatus;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use support::Raw;
use tokio::sync::Notify;

#[derive(Default)]
struct DelayedProbe {
    entered: Notify,
    gate: (Mutex<bool>, Condvar),
    fail: bool,
    calls: Mutex<Vec<Vec<String>>>,
}

impl DelayedProbe {
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

// A failed assertion must also unblock the runtime worker before test teardown.
struct Release(Arc<DelayedProbe>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[async_trait]
impl ProcessManager for DelayedProbe {
    fn is_alive(&self, _: u32) -> bool {
        false
    }
    fn kill(&self, _: u32, _: Signal) {}
    async fn kill_with_grace(&self, _: u32, _: Duration) {}
    fn spawn(&self, command: &str, args: &[String], _: SpawnOptions) -> Result<SpawnedChild> {
        assert_eq!(command, "opencode");
        self.calls.lock().unwrap().push(args.to_vec());
        let output: &[u8] = if args == ["--version"] {
            self.entered.notify_one();
            let mut open = self.gate.0.lock().unwrap();
            while !*open {
                open = self.gate.1.wait(open).unwrap();
            }
            if self.fail {
                return Err(orch_core::Error::other("fake probe spawn failed"));
            }
            b"opencode v2.1.2"
        } else {
            assert_eq!(args, ["run", "--help"], "no agent launch allowed");
            b"--model provider/model#variant"
        };
        Ok(SpawnedChild {
            pid: 987654,
            stdout: Some(Box::new(std::io::Cursor::new(output))),
            stderr: None,
            stdin: None,
            exit: ExitSignal::resolved(ExitOutcome {
                code: Some(0),
                signal: None,
            }),
        })
    }
}

fn state_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, out);
            } else if matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("json" | "yaml" | "yml")
            ) {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, &root.join(".orchestry"), &mut out);
    out
}

#[derive(Clone, Copy)]
enum Stop {
    Shutdown,
    Drop,
    CancelShutdown,
}

async fn delayed_preparation(fail: bool, disconnect: bool, stop: Stop) {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    std::fs::create_dir(root.join(".orchestry")).unwrap();
    let processes = Arc::new(DelayedProbe {
        fail,
        ..Default::default()
    });
    let _release = Release(processes.clone());
    let old = DaemonHarness::builder()
        .processes(processes.clone())
        .maintenance_timer(false)
        .start_in(&root)
        .await
        .unwrap();
    let mut owner = Raw::session(&old, "cli", "normal").await;
    let created = owner
        .request(
            100,
            "agent.create",
            serde_json::json!({
                "name": "delayed", "adapter": "opencode", "workspace_mode": "shared",
                "effort": "high", "model": "openrouter/anthropic/claude-opus-5.5"
            }),
        )
        .await;
    let agent = support::ok(&created)["agent"]["id"].as_str().unwrap();
    support::task(
        &mut owner,
        "delayed preparation",
        serde_json::json!({"assignee": agent, "workspace_mode": "shared"}),
    )
    .await;
    drop(owner);
    let mut raw = Some(Raw::session(&old, "cli", "normal").await);
    raw.as_mut()
        .unwrap()
        .send_text(r#"{"id":1,"req":{"op":"dispatch.tick","args":{}}}"#)
        .await;
    tokio::time::timeout(Duration::from_secs(3), processes.entered.notified())
        .await
        .unwrap();
    let run = old.engine().runs.list_all().await.unwrap().pop().unwrap();
    assert_eq!(run.status, RunStatus::Preparing);
    if disconnect {
        drop(raw.take());
    }
    let shutdown = match stop {
        Stop::Shutdown => {
            let mut shutdown = tokio::spawn(old.shutdown());
            // The original probe reported CLEAN after ten seconds while spawn
            // was still blocked. Deadlines cannot revoke a write/spawn lease.
            assert!(
                tokio::time::timeout(Duration::from_secs(11), &mut shutdown)
                    .await
                    .is_err(),
                "shutdown released ownership while accepted preparation could still spawn/write"
            );
            Some(shutdown)
        }
        Stop::Drop => {
            drop(old);
            tokio::time::sleep(Duration::from_millis(100)).await;
            None
        }
        Stop::CancelShutdown => {
            let shutdown = tokio::spawn(old.shutdown());
            // Wait until engine shutdown has run and lifecycle is draining.
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let state: serde_json::Value = serde_json::from_slice(
                        &std::fs::read(root.join(".orchestry/state.json")).unwrap(),
                    )
                    .unwrap();
                    if state["claimed"]
                        .as_array()
                        .is_some_and(|ids| ids.is_empty())
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            shutdown.abort();
            assert!(shutdown.await.unwrap_err().is_cancelled());
            tokio::time::sleep(Duration::from_millis(100)).await;
            None
        }
    };
    let busy = DaemonHarness::builder()
        .maintenance_timer(false)
        .start_in(&root)
        .await
        .err()
        .expect("old owner retained");
    assert!(
        matches!(busy, orch_daemon::DaemonError::AlreadyRunning(_)),
        "{busy:?}"
    );
    processes.release();
    if let Some(shutdown) = shutdown {
        tokio::time::timeout(Duration::from_secs(20), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    } else {
        tokio::time::timeout(Duration::from_secs(20), async {
            while root.join(".orchestry/orchestry.lock").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fallback drain releases ownership after preparation");
    }
    // PID-file removal precedes flock release. Test the actual ownership
    // boundary, rather than treating the file's absence as its acknowledgement.
    let successor = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match DaemonHarness::builder()
                .maintenance_timer(false)
                .start_in(&root)
                .await
            {
                Ok(successor) => break successor,
                Err(orch_daemon::DaemonError::AlreadyRunning(_)) => tokio::task::yield_now().await,
                Err(error) => panic!("successor start failed: {error}"),
            }
        }
    })
    .await
    .expect("preparation drain releases the owner guard");
    let mut owned = successor.engine().runs.get(&run.id).await.unwrap().unwrap();
    owned.error = Some("successor-owned sentinel".into());
    successor.engine().runs.save(&owned).await.unwrap();
    let before = state_bytes(&root);
    let calls = processes.calls.lock().unwrap().clone();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        state_bytes(&root),
        before,
        "run/task/state/index bytes must remain exact"
    );
    assert_eq!(
        *processes.calls.lock().unwrap(),
        calls,
        "no post-release spawn"
    );
    assert!(matches!(
        DaemonHarness::builder()
            .start_in(&root)
            .await
            .err()
            .unwrap(),
        orch_daemon::DaemonError::AlreadyRunning(_)
    ));
    drop(raw);
    successor.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn successful_probe_is_drained_before_owner_release() {
    delayed_preparation(false, false, Stop::Shutdown).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_probe_after_disconnect_is_drained_before_owner_release() {
    delayed_preparation(true, true, Stop::Shutdown).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_harness_retains_owner_until_preparation_ends() {
    delayed_preparation(false, true, Stop::Drop).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_shutdown_retains_owner_until_preparation_ends() {
    delayed_preparation(true, false, Stop::CancelShutdown).await;
}
