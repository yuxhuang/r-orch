#![cfg(feature = "testing")]
//! Detached diagnostic spawn work retains project ownership through a failed stop.

mod support;

use orch_daemon::{DaemonError, testing::DaemonHarness};
use orch_infra::process::gate::{SpawnObservation, SpawnObserver, SpawnSite, SpawnStage};
use serde_json::json;
use std::sync::{Arc, Condvar, Mutex};
use support::{DEADLINE, Raw};
use tokio::sync::Notify;

#[derive(Default)]
struct HeldDiscovery {
    entered: Notify,
    gate: (Mutex<bool>, Condvar),
}

impl HeldDiscovery {
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

impl SpawnObserver for HeldDiscovery {
    fn observe(&self, event: SpawnObservation) {
        if event.site == SpawnSite::Discovery && event.stage == SpawnStage::Entered {
            self.entered.notify_one();
            let mut released = self.gate.0.lock().unwrap();
            while !*released {
                released = self.gate.1.wait(released).unwrap();
            }
        }
    }
}

// Unblock the owned job on every assertion failure, before runtime teardown.
struct ReleaseOnDrop(Arc<HeldDiscovery>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incomplete_stop_retains_ownership_until_detached_spawn_work_finishes() {
    // The test, rather than the first daemon, owns the project so a successful
    // successor can use the same directory after the first drain completes.
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    std::fs::create_dir(root.join(".orchestry")).unwrap();
    let daemon = DaemonHarness::builder()
        .maintenance_timer(false)
        .start_in(&root)
        .await
        .unwrap();
    // Starting the rival must test ownership, without queuing hostname discovery
    // behind the diagnostic's process-wide spawn reservation.
    daemon
        .engine()
        .config_store
        .set("peer.name", json!("drain-spawn-test"))
        .await
        .unwrap();
    let held = Arc::new(HeldDiscovery::default());
    let _release = ReleaseOnDrop(held.clone());
    daemon.observe_spawns(held.clone());
    let mut probe = Raw::session(&daemon, "cli", "normal").await;
    let mut owner = Raw::session(&daemon, "cli", "normal").await;
    probe
        .send(&json!({"id":1,"req":{"op":"models.catalog","args":{"adapters":["claude"]}}}))
        .await;
    tokio::time::timeout(DEADLINE, held.entered.notified())
        .await
        .expect("diagnostic entered owned spawn work");

    probe
        .send(&json!({"id":3,"req":{"op":"request.cancel","args":{"id":1}}}))
        .await;
    // Cancel the diagnostic future while its synchronous spawn job is
    // still parked at the barrier. Its terminal reply fences request-lease
    // destruction: execute_request joins the task holding that lease.
    let cancelled = probe.terminal(1).await;
    assert_eq!(support::err_code(&cancelled), "conflict", "{cancelled}");
    assert_eq!(cancelled["err"]["message"], "diagnostic cancelled");
    drop(probe);
    owner
        .send(&json!({"id":2,"req":{"op":"daemon.stop","args":{}}}))
        .await;
    // This uses the production stop deadline in full, with the job held
    // throughout. No injected clock or shorter stop timeout is involved.
    let stopped = owner.terminal(2).await;
    assert_eq!(support::err_code(&stopped), "general", "{stopped}");
    assert_eq!(
        stopped["err"]["message"],
        "Daemon shutdown incomplete; diagnostic spawn work remains"
    );
    drop(owner);

    let mut drain = tokio::spawn(daemon.shutdown());
    // Observe the entire drain-completion window, rather than sampling task
    // readiness after one yield. If the spawn-work wait is removed, this
    // completes and the real rival start below exposes the ownership loss.
    // The timeout is the negative observation bound, never an ordering sleep.
    let early_exit = tokio::time::timeout(DEADLINE, &mut drain).await;
    let rival = tokio::time::timeout(
        DEADLINE,
        DaemonHarness::builder()
            .maintenance_timer(false)
            .start_in(&root),
    )
    .await
    .expect("rival explicit start returns while spawn work is held");
    let retained = matches!(&rival, Err(DaemonError::AlreadyRunning(_)));
    let rival_description = match &rival {
        Ok(_) => "rival explicit start succeeded".to_owned(),
        Err(error) => format!("rival explicit start returned {error:?}"),
    };

    // Always release and drain both owners before asserting the saved oracle,
    // including the intentionally broken production-code experiment.
    held.release();
    let exit = match early_exit {
        Ok(exit) => exit.unwrap(),
        Err(_) => tokio::time::timeout(DEADLINE, drain)
            .await
            .expect("released spawn work permits ownership drainage")
            .unwrap(),
    };
    if let Ok(rival) = rival {
        rival.shutdown().await.unwrap();
    }
    assert!(
        retained,
        "OWNERSHIP: project released after incomplete stop while detached spawn work was held; {rival_description}"
    );
    assert!(matches!(exit, Err(DaemonError::StopIncomplete)), "{exit:?}");

    // Positive control: completion of the held job and the ownership drain
    // permits the same explicit start that was refused before release.
    let successor = tokio::time::timeout(
        DEADLINE,
        DaemonHarness::builder()
            .maintenance_timer(false)
            .start_in(&root),
    )
    .await
    .expect("rival explicit start completes after spawn-work drainage")
    .expect("rival explicit start succeeds after spawn-work drainage");
    successor.shutdown().await.unwrap();
}
