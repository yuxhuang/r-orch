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
pub(super) fn before_lock(gate: &Mutex<()>) {
    WAITER.with(|slot| {
        if let Some(tx) = slot.borrow_mut().take()
            && matches!(gate.try_lock(), Err(std::sync::TryLockError::WouldBlock))
        {
            tx.send(Event::Blocked)
                .expect("notify confirmed lock contention");
        }
    });
}

pub(super) fn at_spawn() {
    PARK.with(|slot| {
        if let Some((entered, release)) = slot.borrow_mut().take() {
            entered.send(()).expect("notify parked spawn");
            release
                .recv_timeout(DEADLINE)
                .expect("release parked spawn");
        }
    });
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
