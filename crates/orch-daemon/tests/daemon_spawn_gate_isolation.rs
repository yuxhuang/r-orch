#![cfg(feature = "testing")]
//! D20: isolate a foreign, acquired-but-never-entered Launch reservation from
//! three fake-daemon wake paths. One coordinator gives global snapshots unique
//! ownership; it does not serialize the original wake target.

#[macro_use]
mod support;
#[path = "support/spawn_gate_isolation.rs"]
mod isolation;

use isolation::{Scripted, WAKE_BOUND, fixture};
use orch_core::process::SpawnAdmission;
use orch_infra::process::gate::{
    ProcessSpawnAdmission, SpawnObservation, SpawnSite, SpawnStage, pending_spawns,
};
use serde_json::json;
use std::future::{Future, poll_fn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::task::Poll;
use std::time::Duration;

/// The runtime holding A is independent of the fixture's paused Tokio clock.
/// Dropping this guard releases and joins even during a setup/assertion unwind.
struct ForeignReservation {
    release: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
    released: Arc<AtomicBool>,
    id: u64,
}
impl ForeignReservation {
    fn acquire() -> Self {
        let setup = pending_spawns();
        println!("setup snapshot: {setup:?}");
        assert!(
            setup.is_empty(),
            "unexplained setup reservations: {setup:?}"
        );
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let released = Arc::new(AtomicBool::new(false));
        let flag = released.clone();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let reservation = ProcessSpawnAdmission.reserve().await;
                // Acquisition acknowledged only after reserve has returned. Never enter.
                ready_tx.send(()).unwrap();
                let explicit = release_rx.recv_timeout(Duration::from_secs(60));
                flag.store(true, Ordering::SeqCst);
                drop(reservation);
                assert!(explicit.is_ok(), "foreign-holder cleanup watchdog expired");
            });
        });
        let mut guard = Self {
            release: Some(release_tx),
            worker: Some(worker),
            released,
            id: 0,
        };
        ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("foreign Launch reservation was not acquired");
        let acquired = pending_spawns();
        println!("acknowledged A snapshot: {acquired:?}");
        assert_eq!(acquired.len(), 1, "A must be the only new reservation");
        guard.id = acquired[0].id;
        guard.assert_held(&acquired);
        guard
    }

    fn assert_held(&self, snapshot: &[SpawnObservation]) {
        assert!(!self.released.load(Ordering::SeqCst), "A already released");
        let a: Vec<_> = snapshot.iter().filter(|r| r.id == self.id).collect();
        assert_eq!(a.len(), 1, "A missing from snapshot: {snapshot:?}");
        assert_eq!(a[0].site, SpawnSite::Launch);
        assert_eq!(a[0].stage, SpawnStage::Waiting, "A must never enter");
    }

    fn release_and_join(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}
impl Drop for ForeignReservation {
    fn drop(&mut self) {
        self.release_and_join();
    }
}

#[derive(Clone, Copy, Debug)]
enum Case {
    Collector,
    Retry,
    Pause,
}

async fn scenario(case: Case) -> bool {
    let mut fx = fixture(Scripted::held(matches!(case, Case::Retry))).await;
    let agent = fx
        .agent(
            "isolated",
            if matches!(case, Case::Retry) {
                "worktree"
            } else {
                "shared"
            },
        )
        .await;
    let goal = if matches!(case, Case::Pause) {
        Some(
            fx.call("goal.create", json!({"title": "paused"})).await["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    } else {
        None
    };
    fx.settle().await;
    let mut original = None;
    if matches!(case, Case::Retry) {
        let id = fx
            .task("original failure", json!({"assignee": agent}))
            .await;
        assert!(
            fx.next_tick(WAKE_BOUND).await.is_some(),
            "original failure did not tick"
        );
        let record = fx.call("task.get", json!({"id": id})).await;
        assert_eq!(record["attempts"], 1, "initial workspace failure: {record}");
        assert_eq!(fx.pm.launches_of(&id), 0);
        original = Some(id);
        fx.settle().await;
    }
    let mut task = None;
    if let Some(goal) = &goal {
        let id = fx
            .task("paused task", json!({"assignee": agent, "goal_id": goal}))
            .await;
        fx.call("goal.status", json!({"id": goal, "status": "paused"}))
            .await;
        assert!(
            fx.next_tick(WAKE_BOUND).await.is_some(),
            "pause did not tick"
        );
        assert_eq!(fx.pm.launches_of(&id), 0, "paused task launched");
        fx.settle().await;
        task = Some(id);
    }

    let mut holder = ForeignReservation::acquire();
    let before = fx.pm.log();
    let mut dependent = None;
    match case {
        Case::Collector => {
            let first = fx.task("first", json!({"assignee": agent})).await;
            dependent = Some(
                fx.task(
                    "dependent",
                    json!({"assignee": agent, "depends_on": [first]}),
                )
                .await,
            );
            task = Some(first);
        }
        Case::Retry => {
            fx.task("unrelated 0", json!({})).await;
        }
        Case::Pause => {
            fx.call(
                "goal.status",
                json!({"id": goal.as_ref().unwrap(), "status": "active"}),
            )
            .await;
        }
    }
    let completed = fx.next_tick(WAKE_BOUND).await.is_some();
    let snapshot = pending_spawns();
    let log = fx.pm.log();
    println!(
        "{case:?}: completed_before_release={completed}; released={}; before={before:?}; after={log:?}; pending={snapshot:?}",
        holder.released.load(Ordering::SeqCst)
    );
    holder.assert_held(&snapshot);
    let b: Vec<_> = snapshot.iter().filter(|r| r.id != holder.id).collect();
    let blocked_id = if completed {
        assert!(b.is_empty(), "completed wake left B pending: {snapshot:?}");
        if matches!(case, Case::Retry) {
            assert_eq!(
                &log[before.len()..],
                &[String::new()],
                "exactly the unrelated workspace probe must complete before release"
            );
        }
        if let Some(task) = &task {
            assert_eq!(
                fx.pm.launches_of(task),
                1,
                "wake must launch before A release"
            );
        }
        if let Some(original) = &original {
            let record = fx.call("task.get", json!({"id": original})).await;
            assert_eq!(
                record["attempts"], 1,
                "wake bypassed backoff before A release"
            );
        }
        holder.assert_held(&pending_spawns());
        None
    } else {
        assert_eq!(log, before, "B entered a fake body while A held admission");
        assert_eq!(b.len(), 1, "missing/extra B cause record: {snapshot:?}");
        assert_eq!(
            b[0].site,
            if matches!(case, Case::Retry) {
                SpawnSite::Workspace
            } else {
                SpawnSite::Launch
            }
        );
        assert_eq!(b[0].stage, SpawnStage::Waiting);
        Some(b[0].id)
    };

    // Poll next_tick once BEFORE release: its drain must not discard the tick
    // that releasing A permits. This receiver is a separate positive control.
    if !completed {
        let mut control = std::pin::pin!(fx.next_tick(WAKE_BOUND));
        poll_fn(|cx| {
            assert!(
                control.as_mut().poll(cx).is_pending(),
                "control tick completed before release"
            );
            Poll::Ready(())
        })
        .await;
        holder.release_and_join();
        assert!(control.await.is_some(), "post-release control tick missing");
    } else {
        holder.release_and_join();
    }
    let after = pending_spawns();
    println!(
        "{case:?}: post-release pending={after:?}; log={:?}",
        fx.pm.log()
    );
    assert!(
        !after
            .iter()
            .any(|r| r.id == holder.id || Some(r.id) == blocked_id),
        "matching A/B record survived control completion"
    );
    if let Some(original) = original {
        assert_eq!(
            &fx.pm.log()[before.len()..],
            &[String::new()],
            "baseline Retry probe must appear after release"
        );
        let record = fx.call("task.get", json!({"id": original})).await;
        assert_eq!(
            record["attempts"], 1,
            "wake bypassed original backoff: {record}"
        );
        assert_eq!(fx.pm.launches_of(&original), 0);
    }
    if let Some(task) = task {
        let pm = fx.pm.clone();
        eventually!("one eventual task launch", pm.launches_of(&task) == 1);
        if let Some(dependent) = dependent {
            assert_eq!(
                fx.pm.launches_of(&dependent),
                0,
                "dependent launched before collector"
            );
            fx.pm.release(&task);
            assert!(
                fx.next_tick(WAKE_BOUND).await.is_some(),
                "collector completion did not wake"
            );
            let pm = fx.pm.clone();
            eventually!(
                "dependent launch after collector",
                pm.launches_of(&dependent) == 1
            );
            fx.pm.release(&dependent);
        } else {
            assert_eq!(fx.pm.launches(), 1, "resume must launch exactly one task");
            fx.pm.release(&task);
        }
    }
    fx.finish().await;
    assert!(
        pending_spawns().is_empty(),
        "teardown left unexplained reservations"
    );
    completed
}

async fn coordinate() {
    let mut results = Vec::new();
    for case in [Case::Collector, Case::Retry, Case::Pause] {
        results.push((case, scenario(case).await));
    }
    println!("all measured outcomes (cleanup completed): {results:?}");
    assert!(
        results.iter().all(|(_, completed)| *completed),
        "fake daemon completed its wake while the foreign pre-spawn reservation remained held"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test(start_paused = true)]
async fn collector_retry_and_pause_complete_while_foreign_launch_admission_is_held() {
    coordinate().await;
}

#[cfg(target_os = "linux")]
#[tokio::test(start_paused = true)]
async fn linux_foreign_launch_reservation_does_not_serialize_fake_daemon_wakes() {
    coordinate().await;
}
