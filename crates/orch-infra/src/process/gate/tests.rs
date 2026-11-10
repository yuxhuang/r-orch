//! Proof S: real builders, parked before std can create its pipes.
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::Duration;

use orch_core::process::{ProcessManager, SpawnOptions};

use crate::models::{DiscoveryRuntime, SnapshotDiscoveryRuntime};
use crate::process::{EnvSnapshot, SnapshotEnvProcessManager};

const DEADLINE: Duration = Duration::from_secs(20);
static SCENARIOS: Mutex<()> = Mutex::new(());

thread_local! {
    static WAITER: RefCell<Option<mpsc::Sender<Event>>> = const { RefCell::new(None) };
    static PARK: RefCell<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = const { RefCell::new(None) };
}

// This probe is test-only and does not replace acquisition: production's
// blocking lock immediately follows it. Only confirmed WouldBlock sends a
// contention event; a poisoned but available mutex is not contention.
pub(super) fn before_lock(gate: &tokio::sync::Semaphore) {
    WAITER.with(|slot| {
        if let Some(tx) = slot.borrow_mut().take()
            && gate.available_permits() == 0
        {
            tx.send(Event::Blocked)
                .expect("notify confirmed lock contention");
        }
    });
}

pub(crate) fn at_spawn() {
    PARK.with(|slot| {
        if let Some((entered, release)) = slot.borrow_mut().take() {
            entered.send(()).expect("notify parked spawn");
            release
                .recv_timeout(DEADLINE)
                .expect("release parked spawn");
        }
    });
}

pub(crate) fn take_park() -> Option<(mpsc::Sender<()>, mpsc::Receiver<()>)> {
    PARK.with(|slot| slot.borrow_mut().take())
}
pub(crate) fn install_park(park: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>) {
    PARK.with(|slot| *slot.borrow_mut() = park);
}

#[derive(Clone, Copy, Debug)]
enum Event {
    Blocked,
    Started,
}

#[derive(Clone, Copy, Debug)]
enum Builder {
    Manager,
    Discovery,
    Hostname,
}

fn snapshot(root: &Path) -> EnvSnapshot {
    EnvSnapshot {
        base: vec![
            ("HOME".into(), root.as_os_str().to_owned()),
            ("PATH".into(), root.as_os_str().to_owned()),
        ],
        forced: vec![],
    }
}

fn launch(builder: Builder, root: &Path, marker: PathBuf, handle: &tokio::runtime::Handle) {
    match builder {
        Builder::Manager => {
            let _entered = handle.enter();
            let child = SnapshotEnvProcessManager::new(snapshot(root))
                .spawn(
                    "fake-spawn",
                    &[marker.to_str().expect("UTF-8 fixture path").to_owned()],
                    SpawnOptions::default(),
                )
                .expect("spawn fake executable");
            assert!(handle.block_on(child.exit.wait()).success());
        }
        Builder::Discovery => {
            let output = handle
                .block_on(SnapshotDiscoveryRuntime::new(snapshot(root)).run_command(
                    "fake-spawn",
                    &[marker.to_str().expect("UTF-8 fixture path")],
                ))
                .expect("capture fake executable");
            assert!(output.stderr.is_empty());
        }
        Builder::Hostname => {
            assert!(
                crate::storage::remote::hostname_output()
                    .expect("hostname fallback")
                    .status
                    .success()
            );
        }
    }
}

fn exclusion(first: Builder, second: Builder) {
    use std::os::unix::fs::PermissionsExt;
    let _serial = SCENARIOS.lock().unwrap_or_else(PoisonError::into_inner);
    let root = tempfile::tempdir().expect("fixture directory");
    let script = root.path().join("fake-spawn");
    std::fs::write(&script, "#!/bin/sh\nprintf started > \"$1\"\n").expect("write fake executable");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
        .expect("make fixture executable");
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let marker = root.path().join("second-started");
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let escaped = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            PARK.with(|slot| *slot.borrow_mut() = Some((entered_tx, release_rx)));
            launch(
                first,
                root.path(),
                root.path().join("first-started"),
                runtime.handle(),
            );
        });
        entered_rx
            .recv_timeout(DEADLINE)
            .expect("first builder entered its spawn section");
        let b = scope.spawn(|| {
            WAITER.with(|slot| *slot.borrow_mut() = Some(event_tx.clone()));
            launch(second, root.path(), marker.clone(), runtime.handle());
            let _ = event_tx.send(Event::Started);
        });
        // The gated path reports confirmed contention before taking the real
        // lock. A bypass instead has to run the fake executable and report its
        // completion. There is no "wait a little and hope it ran" control.
        let event = event_rx
            .recv_timeout(DEADLINE)
            .expect("contention or completed executable");
        let escaped = matches!(event, Event::Started);
        if escaped {
            assert!(
                marker.exists(),
                "red requires the real fake executable marker"
            );
        } else {
            assert!(!marker.exists(), "blocked executable must not have started");
        }
        release_tx.send(()).expect("notify attempted spawn");
        a.join().expect("first builder finished");
        b.join().expect("second builder finished");
        escaped
    });
    assert!(
        marker.exists(),
        "second fake executable must run after release"
    );
    assert!(
        !escaped,
        "second fake executable started during A's section: {first:?} -> {second:?}"
    );
}

#[test]
fn manager_excludes_discovery_spawn() {
    exclusion(Builder::Manager, Builder::Discovery);
}
#[test]
fn discovery_excludes_manager_spawn() {
    exclusion(Builder::Discovery, Builder::Manager);
}
#[test]
fn manager_excludes_manager_spawn() {
    exclusion(Builder::Manager, Builder::Manager);
}
#[test]
fn hostname_excludes_manager_spawn() {
    exclusion(Builder::Hostname, Builder::Manager);
}

#[test]
fn unwinding_spawn_releases_and_recovers_the_poisoned_gate() {
    let _serial = SCENARIOS.lock().unwrap_or_else(PoisonError::into_inner);
    let panic =
        std::panic::catch_unwind(|| super::with_spawn_gate(|| panic!("controlled spawn panic")));
    assert!(panic.is_err());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let child = SnapshotEnvProcessManager::new(EnvSnapshot::default())
        .spawn("/usr/bin/true", &[], SpawnOptions::default())
        .unwrap();
    assert!(runtime.block_on(child.exit.wait()).success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asynchronous_admission_yields_and_cancellation_releases_waiting_record() {
    let holder = super::reserve_spawn(super::SpawnSite::Discovery).await;
    let waiter = tokio::spawn(async { super::reserve_spawn(super::SpawnSite::Launch).await });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "queued launch must wait for complete spawn"
    );
    tokio::time::timeout(DEADLINE, tokio::spawn(async { 42 }))
        .await
        .unwrap()
        .unwrap();
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    drop(holder);
    let permit = tokio::time::timeout(DEADLINE, super::reserve_spawn(super::SpawnSite::Launch))
        .await
        .unwrap();
    permit.run(|| super::with_spawn_gate(|| ()));
}

#[test]
fn forwarded_opencode_waiters_cancel_before_diagnostic_release() {
    use orch_core::adapter::{Adapter, ExecuteParams};
    use orch_domain::{AgentConfig, ReasoningEffort};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    let _serial = SCENARIOS.lock().unwrap_or_else(PoisonError::into_inner);
    let root = tempfile::tempdir().unwrap();
    for name in ["fake-spawn", "opencode"] {
        let file = root.path().join(name);
        std::fs::write(
            &file,
            "#!/bin/sh\nprintf started >> \"$HOME/executed\"\nprintf 'opencode v2.1.2\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    struct Release(Option<mpsc::Sender<()>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }
    let mut release = Release(Some(release_tx));
    let (blocked_tx, blocked_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let tokens: Vec<_> = (0..2)
        .map(|_| tokio_util::sync::CancellationToken::new())
        .collect();
    let result = std::thread::scope(|scope| {
        let holder = scope.spawn(|| {
            PARK.with(|slot| *slot.borrow_mut() = Some((entered_tx, release_rx)));
            launch(
                Builder::Discovery,
                root.path(),
                root.path().join("holder"),
                runtime.handle(),
            );
        });
        let result = (|| -> Result<(), String> {
            entered_rx
                .recv_timeout(DEADLINE)
                .map_err(|e| e.to_string())?;
            let mut jobs = Vec::new();
            for (index, token) in tokens.iter().enumerate() {
                let token = token.clone();
                let send = blocked_tx.clone();
                let done = done_tx.clone();
                let manager = Arc::new(SnapshotEnvProcessManager::new(snapshot(root.path())));
                let workspace = root.path().to_owned();
                jobs.push(runtime.spawn(async move {
                    WAITER.with(|slot| *slot.borrow_mut() = Some(send));
                    let params = ExecuteParams {
                        prompt: "sensitive-prompt".into(),
                        system_prompt: None,
                        workspace,
                        env: Default::default(),
                        inherit_env: orch_core::process::EnvInheritance::WithoutSecrets,
                        config: AgentConfig {
                            model: Some("openrouter/anthropic/claude-opus-5.5".into()),
                            effort: Some(ReasoningEffort::Xhigh),
                            ..Default::default()
                        },
                        cancel: token.clone(),
                    };
                    let adapter = crate::adapters::OpenCodeAdapter::new(manager);
                    let cancelled = tokio::select! {
                        biased;
                        () = token.cancelled() => true,
                        _ = adapter.prepare_effort(&params) => token.is_cancelled(),
                    };
                    let _ = done.send((index, cancelled));
                }));
            }
            // These events come from the real semaphore's unavailable state.
            let waits = [
                blocked_rx.recv_timeout(DEADLINE),
                blocked_rx.recv_timeout(DEADLINE),
            ];
            let (control_tx, control_rx) = mpsc::channel();
            let control = runtime.spawn(async move {
                let _ = control_tx.send(());
            });
            for token in &tokens {
                token.cancel();
            }
            let progress = control_rx.recv_timeout(Duration::from_millis(250)).is_ok();
            let completed = [
                done_rx.recv_timeout(Duration::from_millis(250)),
                done_rx.recv_timeout(Duration::from_millis(250)),
            ];
            let no_launch = !root.path().join("executed").exists();
            eprintln!(
                "OPENCODE: confirmed={waits:?}; pre-release control={progress}; cancellations={completed:?}; no-executable={no_launch}"
            );
            // Always release and drain before asserting the saved before-release results.
            release.0.take().unwrap().send(()).unwrap();
            runtime.block_on(async {
                for job in jobs {
                    job.await.unwrap();
                }
                control.await.unwrap();
            });
            if !waits.iter().all(Result::is_ok)
                || !progress
                || !completed.iter().all(|r| matches!(r, Ok((_, true))))
                || !no_launch
            {
                return Err("OpenCode control/cancellation did not complete before diagnostic release without executable entry".into());
            }
            Ok(())
        })();
        drop(release);
        holder.join().unwrap();
        result
    });
    result.unwrap();
    // Only the released diagnostic executes; queued preparation must never spawn late.
    assert_eq!(
        std::fs::read_to_string(root.path().join("executed")).unwrap(),
        "started"
    );
}
