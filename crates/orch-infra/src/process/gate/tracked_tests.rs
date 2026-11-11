//! Tracker admission is owned, single-use, and specific to its manager.
use super::*;
use crate::process::{EnvSnapshot, SnapshotEnvProcessManager};
use orch_core::process::{ProcessManager, SpawnAdmission, SpawnOptions};
use std::sync::Arc;

#[tokio::test]
async fn dropping_admitted_reservation_or_scope_releases_owned_work() {
    let work = Arc::new(SpawnWork::default());
    let admission = TrackedSpawnAdmission::new(SpawnDomain::default(), work.clone());
    let mut reservation = admission.reserve().await;
    assert!(reservation.try_admit());
    assert!(!work.is_idle());
    drop(reservation);
    assert!(work.is_idle());
    let mut reservation = admission.reserve().await;
    assert!(reservation.try_admit());
    let scope = reservation.enter();
    work.seal();
    assert!(!work.is_idle());
    drop(scope);
    assert!(work.is_idle());
    work.wait_idle().await;
    let mut refused = admission.reserve().await;
    assert!(!refused.try_admit());
    assert!(work.is_idle());
}

#[tokio::test]
async fn admitted_launch_after_seal_consumes_its_own_lease_once() {
    let work = Arc::new(SpawnWork::default());
    let other = Arc::new(SpawnWork::default());
    let admission = TrackedSpawnAdmission::new(SpawnDomain::default(), work.clone());
    let manager =
        SnapshotEnvProcessManager::new(EnvSnapshot::default()).with_spawn_work(work.clone());
    let other_manager =
        SnapshotEnvProcessManager::new(EnvSnapshot::default()).with_spawn_work(other.clone());
    let mut reservation = admission.reserve().await;
    assert!(reservation.try_admit());
    work.seal();
    other.seal();
    let child = {
        let _scope = reservation.enter();
        // A foreign manager cannot steal the admission, nor can a second call
        // reuse it after the first child has consumed it.
        assert!(
            other_manager
                .spawn("/usr/bin/true", &[], SpawnOptions::default())
                .is_err()
        );
        let child = manager
            .spawn("/usr/bin/true", &[], SpawnOptions::default())
            .unwrap();
        assert!(
            manager
                .spawn("/usr/bin/true", &[], SpawnOptions::default())
                .is_err()
        );
        child
    };
    assert!(child.exit.wait().await.success());
    work.wait_idle().await;
    assert!(work.is_idle());
    assert!(other.is_idle());
}
