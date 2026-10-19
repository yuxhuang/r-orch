//! Process endurance and `process.kill` failure tests.
//!
//! These drive **real** processes. They run everywhere,
//! including `CI`, because they cover the one part of the layer where a
//! mistake is invisible until production: reaping. A tokio child that is
//! never waited on stays a zombie, and `kill(pid, 0)` keeps answering
//! "alive" — so a dropped `is_alive` contract would show up here and nowhere
//! else.

use std::time::Duration;

use orch_core::process::{DEFAULT_KILL_GRACE, ProcessManager, Signal, SpawnOptions};
use orch_core::test_support::{describe, wait_for_no_live_members};
use orch_infra::process::SystemProcessManager;

/// Pids well past `/proc/sys/kernel/pid_max`, so they cannot exist.
const FREE_PIDS: [u32; 5] = [
    999_000_001,
    999_000_002,
    999_000_003,
    999_000_004,
    999_000_005,
];

fn manager() -> SystemProcessManager {
    SystemProcessManager::new()
}

fn spawn_sleep(pm: &SystemProcessManager, seconds: u32) -> u32 {
    pm.spawn("sleep", &[seconds.to_string()], SpawnOptions::default())
        .expect("spawn sleep")
        .pid
}

/// Reaping is asynchronous: the background task has to run before the kernel
/// releases the pid.
async fn wait_until_dead(pm: &SystemProcessManager, pid: u32) -> bool {
    for _ in 0..1_000 {
        if !pm.is_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test]
async fn zombie_detection_tells_live_processes_from_dead_ones() {
    let pm = manager();
    let pids: Vec<u32> = (0..20).map(|_| spawn_sleep(&pm, 30)).collect();

    for pid in &pids {
        assert!(pm.is_alive(*pid), "pid {pid} should be alive after spawn");
    }

    let (killed, alive) = pids.split_at(10);
    for pid in killed {
        pm.kill(*pid, Signal::Kill);
    }

    for pid in killed {
        assert!(
            wait_until_dead(&pm, *pid).await,
            "pid {pid} should be dead after SIGKILL, not a zombie"
        );
    }
    for pid in alive {
        assert!(pm.is_alive(*pid), "pid {pid} should still be alive");
    }

    for pid in alive {
        pm.kill(*pid, Signal::Kill);
    }
}

#[tokio::test]
async fn kill_with_grace_guarantees_death_for_both_shapes_of_child() {
    let pm = manager();

    // A child that honours SIGTERM dies inside the grace period.
    let cooperative = spawn_sleep(&pm, 60);
    assert!(pm.is_alive(cooperative));
    pm.kill_with_grace(cooperative, Duration::from_secs(1))
        .await;
    assert!(wait_until_dead(&pm, cooperative).await);

    // One that ignores it is SIGKILLed once the grace expires.
    let stubborn = pm
        .spawn(
            "sh",
            &[
                "-c".to_owned(),
                "trap '' TERM; printf 'up\\n'; while :; do sleep 0.05; done".to_owned(),
            ],
            SpawnOptions::default(),
        )
        .expect("spawn");
    let mut stdout = stubborn.stdout.expect("stdout");
    let mut up = [0u8; 3];
    tokio::io::AsyncReadExt::read_exact(&mut stdout, &mut up)
        .await
        .expect("the trap is installed");

    pm.kill_with_grace(stubborn.pid, Duration::from_millis(100))
        .await;
    assert!(wait_until_dead(&pm, stubborn.pid).await);
}

#[tokio::test]
async fn repeated_kill_with_grace_calls_all_terminate_their_process() {
    let pm = manager();
    for run in 0..5 {
        let pid = spawn_sleep(&pm, 60);
        assert!(pm.is_alive(pid));
        pm.kill_with_grace(pid, Duration::from_secs(1)).await;
        assert!(
            wait_until_dead(&pm, pid).await,
            "run {run} (pid {pid}) must be dead"
        );
    }
}

#[tokio::test]
async fn the_group_kill_cleans_up_the_children_of_the_child() {
    let pm = manager();
    let child = pm
        .spawn(
            "sh",
            &[
                "-c".to_owned(),
                // All four forked before `up`, then `wait` (a builtin). See
                // the twin in `process::manager`'s unit tests for why the
                // trailing foreground `sleep` was a race: a child forked
                // between the SIGTERM and the shell's death joins the group
                // unsignalled, and `kill_with_grace` returns without ever
                // sending SIGKILL.
                "sleep 30 & sleep 30 & sleep 30 & sleep 30 & printf 'up\\n'; wait".to_owned(),
            ],
            SpawnOptions::default(),
        )
        .expect("spawn");

    let mut stdout = child.stdout.expect("stdout");
    let mut up = [0u8; 3];
    tokio::io::AsyncReadExt::read_exact(&mut stdout, &mut up)
        .await
        .expect("the children are up");

    pm.kill_with_grace(child.pid, Duration::from_secs(2)).await;
    assert!(wait_until_dead(&pm, child.pid).await);

    // Nothing *live* is left in the group. See the twin of this test in
    // `process::manager`'s unit tests for why this counts live members rather
    // than polling `kill(-pgid, 0)` until the group is empty: that call also
    // succeeds for a zombie, so the old form waited on init's reaping and
    // flaked without being able to say which half had failed.
    if let Err(survivors) = wait_for_no_live_members(child.pid, Duration::from_secs(10)).await {
        panic!(
            "the group kill left {} live process(es) behind, so it did not reach the whole \
             group: {}",
            survivors.len(),
            describe(&survivors)
        );
    }
}

#[tokio::test]
async fn fifty_rapid_spawn_kill_cycles_leave_no_zombies() {
    let pm = manager();
    let mut pids = Vec::new();
    for _ in 0..50 {
        let pid = spawn_sleep(&pm, 60);
        assert!(pm.is_alive(pid));
        pm.kill(pid, Signal::Kill);
        pids.push(pid);
    }

    for pid in pids {
        assert!(
            wait_until_dead(&pm, pid).await,
            "pid {pid} was never reaped"
        );
    }
}

#[tokio::test]
async fn a_hundred_concurrent_liveness_checks_agree() {
    let pm = manager();
    let live: Vec<u32> = (0..5).map(|_| spawn_sleep(&pm, 30)).collect();

    let targets: Vec<(u32, bool)> = live
        .iter()
        .map(|pid| (*pid, true))
        .chain(FREE_PIDS.iter().map(|pid| (*pid, false)))
        .collect();

    let checks: Vec<_> = (0..100)
        .map(|index| {
            let (pid, expected) = targets[index % targets.len()];
            let pm = manager();
            tokio::spawn(async move { (pid, pm.is_alive(pid), expected) })
        })
        .collect();

    for check in checks {
        let (pid, actual, expected) = check.await.expect("join");
        assert_eq!(actual, expected, "pid {pid}");
    }

    for pid in live {
        pm.kill(pid, Signal::Kill);
    }
}

#[tokio::test]
async fn dropping_the_handle_does_not_kill_the_child() {
    // The Rust equivalent of node's `proc.unref()`: the parent may let go of a
    // child without reaping it. `kill_on_drop` must stay off, or cancelling a
    // collector would take the agent with it.
    let pm = manager();
    let pid = {
        let child = pm
            .spawn("sleep", &["5".to_owned()], SpawnOptions::default())
            .expect("spawn");
        child.pid
    };

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        pm.is_alive(pid),
        "the child was killed when we let go of it"
    );
    pm.kill(pid, Signal::Kill);
    assert!(wait_until_dead(&pm, pid).await);
}

#[tokio::test]
async fn is_alive_is_false_for_a_pid_that_cannot_exist() {
    let pm = manager();
    assert!(!pm.is_alive(FREE_PIDS[0]));
}

#[test]
fn is_alive_is_true_for_our_own_pid() {
    assert!(manager().is_alive(std::process::id()));
}

#[test]
fn killing_a_dead_pid_does_not_panic() {
    let pm = manager();
    pm.kill(FREE_PIDS[1], Signal::Term);
    pm.kill(FREE_PIDS[1], Signal::Kill);
}

#[tokio::test]
async fn kill_with_grace_returns_immediately_for_a_dead_pid() {
    let pm = manager();
    let started = std::time::Instant::now();
    pm.kill_with_grace(FREE_PIDS[2], DEFAULT_KILL_GRACE).await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "it waited {:?} for a process that does not exist",
        started.elapsed()
    );
}
