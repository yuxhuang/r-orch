#![cfg(feature = "testing")]
//! Ownership and lifecycle vectors through the in-process harness (T1; design
//! D3, D12 `daemon_lifecycle::*`; spec R13–R15, R37). Process-level races,
//! signals and crashes need a real child and live in
//! `orch-cli/tests/daemon_lifecycle*.rs` (T1b).

#[macro_use]
mod support;

use nix::fcntl::{Flock, FlockArg};
use orch_daemon::DaemonError;
use orch_daemon::testing::DaemonHarness;
use serde_json::json;
use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use support::{Raw, Reaper, err_code, ok, shell_agent, task};

const GUARD: &str = ".orchestry/.daemon-owner.lock";
const PID_LOCK: &str = ".orchestry/orchestry.lock";

fn lock_pid(root: &Path) -> Option<u32> {
    std::fs::read_to_string(root.join(PID_LOCK))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Backdate a file's mtime by an hour: the legacy staleness rule (60 s) would
/// call a PID lock this old abandoned.
fn age(path: &Path) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
}

// -- the guard: one winner, a loser that names it -------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guard_has_one_winner_and_the_loser_names_it_and_changes_nothing() {
    let winner = DaemonHarness::start().await.unwrap();
    let root = winner.project_root().to_path_buf();
    let me = std::process::id();
    assert_eq!(lock_pid(&root), Some(me));
    let guard_before = std::fs::read_to_string(root.join(GUARD)).unwrap();

    let loser = DaemonHarness::builder().start_in(&root).await.err();
    let Some(DaemonError::AlreadyRunning(pid)) = loser else {
        panic!("expected AlreadyRunning, got {loser:?}");
    };
    assert_eq!(
        pid,
        Some(me),
        "the loser must name the winner, not invent a PID"
    );
    assert_eq!(DaemonError::AlreadyRunning(pid).exit_code(), 4);
    assert!(
        DaemonError::AlreadyRunning(pid)
            .to_string()
            .contains(&format!("(PID {me})"))
    );

    // The loser touched nothing the winner owns.
    assert_eq!(lock_pid(&root), Some(me));
    assert_eq!(
        std::fs::read_to_string(root.join(GUARD)).unwrap(),
        guard_before
    );
    let mut raw = Raw::session(&winner, "cli", "normal").await;
    ok(&raw.request(1, "daemon.status", json!({})).await);
    let project = winner.quiesce().await.unwrap();

    // Once released, a successor wins the same project.
    let successor = DaemonHarness::builder().start_in(&root).await.unwrap();
    successor.shutdown().await.unwrap();
    drop(project);
}

/// Many contenders at once: exactly one wins, every loser reports exit 4.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_contenders_produce_exactly_one_owner() {
    let first = DaemonHarness::start().await.unwrap();
    let root = first.project_root().to_path_buf();
    let _project = first.quiesce().await.unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(6));
    let mut contenders = Vec::new();
    for _ in 0..6 {
        let (root, barrier) = (root.clone(), barrier.clone());
        contenders.push(tokio::spawn(async move {
            barrier.wait().await;
            DaemonHarness::builder().start_in(&root).await
        }));
    }
    let mut winners = Vec::new();
    let mut losers = 0;
    for contender in contenders {
        match contender.await.unwrap() {
            Ok(daemon) => winners.push(daemon),
            Err(DaemonError::AlreadyRunning(_)) => losers += 1,
            Err(other) => panic!("a loser failed for another reason: {other:?}"),
        }
    }
    assert_eq!((winners.len(), losers), (1, 5));
    winners.pop().unwrap().shutdown().await.unwrap();
    drop(_project);
}

// -- legacy lock: never evicted by age ------------------------------------

/// A live owner whose PID file looks hours old is still the owner: a second
/// start is refused and the file is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_owner_old_mtime_not_evicted() {
    let owner = DaemonHarness::start().await.unwrap();
    let root = owner.project_root().to_path_buf();
    age(&root.join(PID_LOCK));
    age(&root.join(GUARD));
    let before = std::fs::read(root.join(PID_LOCK)).unwrap();

    let contender = DaemonHarness::builder().start_in(&root).await.err();
    assert!(
        matches!(contender, Some(DaemonError::AlreadyRunning(_))),
        "{contender:?}"
    );
    assert_eq!(std::fs::read(root.join(PID_LOCK)).unwrap(), before);
    // Still serving.
    let mut raw = Raw::session(&owner, "cli", "normal").await;
    ok(&raw.request(1, "daemon.status", json!({})).await);
    owner.shutdown().await.unwrap();
}

/// A v0.13 `orch run --watch` takes the lock with the age rule. The daemon's
/// own refresh keeps an idle owner (dispatch OFF) from ever looking stale, so
/// the legacy acquire is refused. Paused time stands in for the 10 s refresh
/// period, so no real waiting.
#[tokio::test(start_paused = true)]
async fn legacy_acquire_cannot_steal_idle_owner() {
    use orch_infra::storage::lock::acquire_lock;

    // Control: the legacy rule really does steal an hour-old lock, so the
    // assertion below is about the heartbeat and not about a lenient rule.
    let control = tempfile::tempdir().unwrap();
    let control_lock = control.path().join("orchestry.lock");
    std::fs::write(&control_lock, std::process::id().to_string()).unwrap();
    age(&control_lock);
    assert!(
        acquire_lock(&control_lock).await.unwrap().acquired,
        "control: legacy rule changed"
    );

    let daemon = DaemonHarness::start().await.unwrap();
    let root = daemon.project_root().to_path_buf();
    let lock = root.join(PID_LOCK);
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let status = raw.request(1, "daemon.status", json!({})).await;
    assert_eq!(ok(&status)["dispatch"]["dispatch_on"], false);

    age(&lock);
    let ancient = std::fs::metadata(&lock).unwrap().modified().unwrap();
    // The owner is idle; only the heartbeat can freshen the file.
    tokio::time::advance(Duration::from_secs(11)).await;
    eventually!("the heartbeat to refresh the PID lock", {
        std::fs::metadata(&lock).unwrap().modified().unwrap() > ancient
    });
    let result = acquire_lock(&lock).await.unwrap();
    assert!(
        !result.acquired,
        "a legacy acquire stole an idle live owner"
    );
    assert_eq!(result.pid, Some(std::process::id()));
    assert_eq!(lock_pid(&root), Some(std::process::id()));
    daemon.shutdown().await.unwrap();
}

/// A missing or foreign PID lock is surfaced and disables dispatch; the
/// daemon never silently recreates it (R37).
#[tokio::test(start_paused = true)]
async fn ownership_lost_disables_dispatch_and_is_never_silently_recreated() {
    let daemon = DaemonHarness::start().await.unwrap();
    let root = daemon.project_root().to_path_buf();
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let agent = shell_agent(&mut raw, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let id = task(&mut raw, "queued", json!({"assignee": agent})).await;
    let status = raw.request(2, "daemon.status", json!({})).await;
    assert_eq!(ok(&status)["ownership"], "held");

    // Another orch takes the lock file over.
    std::fs::write(root.join(PID_LOCK), "1").unwrap();
    tokio::time::advance(Duration::from_secs(11)).await;
    // A request in flight at the instant of the transition is cancelled with
    // `request cancelled before execution`; poll until a reply says `lost`.
    let mut poll = 100;
    eventually!("ownership to be reported lost", {
        poll += 1;
        let status = raw.request(poll, "daemon.status", json!({})).await;
        status.get("ok").is_some_and(|ok| ok["ownership"] == "lost")
    });
    // Visible, and dispatch refuses to run on lost ownership.
    for (n, op) in ["dispatch.tick", "dispatch.run_all"]
        .into_iter()
        .enumerate()
    {
        let reply = raw.request(10 + n as u64, op, json!({})).await;
        assert_eq!(err_code(&reply), "conflict", "{op}: {reply}");
        assert_eq!(reply["err"]["execution"], "not_executed");
    }
    let reply = raw
        .request(20, "dispatch.run_task", json!({"task_id": id}))
        .await;
    assert_eq!(err_code(&reply), "conflict", "{reply}");
    assert_eq!(
        support::task_status(&mut raw, &id).await,
        "todo",
        "work launched without ownership"
    );
    // Not recreated: still the foreign content after further heartbeats.
    tokio::time::advance(Duration::from_secs(30)).await;
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert_eq!(std::fs::read_to_string(root.join(PID_LOCK)).unwrap(), "1");
    // Reads and status stay alive for the owner to diagnose.
    ok(&raw.request(30, "task.list", json!({})).await);
    daemon.shutdown().await.unwrap();
}

// -- agents never inherit ownership ---------------------------------------

/// The guard descriptor is close-on-exec: an agent child (and anything it
/// forks) holds no descriptor on the guard file, so it can never keep the
/// project locked after the daemon is gone. A control child that opens the
/// file itself must be detected, so the scan is not vacuous.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guard_not_inherited_by_agent_children() {
    let daemon = DaemonHarness::start().await.unwrap();
    let root = daemon.project_root().to_path_buf();
    let guard = root.join(GUARD);
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    // `ls -iL` resolves /dev/fd/N to the inode of what the descriptor points
    // at on both macOS and Linux; `[ -ef ]` does not work on macOS' /bin/sh.
    let scan = |preamble: &str, out: &Path| {
        format!(
            "{preamble} g=$(ls -i '{guard}'); g=${{g%% *}}; for f in /dev/fd/*; do i=$(ls -iL \"$f\" 2>/dev/null); i=${{i%% *}}; if [ \"$i\" = \"$g\" ]; then echo HELD >> '{out}'; fi; done; echo SCANNED >> '{out}'",
            guard = guard.display(),
            out = out.display(),
        )
    };
    let clean_out = root.join("fd-clean.txt");
    let control_out = root.join("fd-control.txt");
    let clean = shell_agent(&mut raw, &daemon, "scanner", &scan("", &clean_out)).await;
    let control = shell_agent(
        &mut raw,
        &daemon,
        "control",
        &scan(&format!("exec 9< '{}';", guard.display()), &control_out),
    )
    .await;
    for (n, agent) in [clean, control].into_iter().enumerate() {
        let id = task(&mut raw, &format!("scan {n}"), json!({"assignee": agent})).await;
        let reply = raw
            .request(100 + n as u64, "dispatch.run_task", json!({"task_id": id}))
            .await;
        assert_eq!(ok(&reply)["exit_code"], 0, "{reply}");
    }
    assert_eq!(
        std::fs::read_to_string(&clean_out).unwrap(),
        "SCANNED\n",
        "an agent child inherited the owner guard"
    );
    assert_eq!(
        std::fs::read_to_string(&control_out).unwrap(),
        "HELD\nSCANNED\n",
        "the scan cannot see a held descriptor, so the clean result proves nothing"
    );
    daemon.shutdown().await.unwrap();
}

// -- capacity is not absence ----------------------------------------------

/// With every session slot taken a new client is told `busy` with the counts
/// (never "no daemon", so no start hint); established sessions keep working,
/// their control operations are not starved, and the frozen stop intent still
/// gets through without a slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_cap_reports_busy_not_absent() {
    use orch_proto::frame::limits::MAX_SESSIONS;
    let daemon = DaemonHarness::start().await.unwrap();
    let reaper = Reaper::new(&daemon);
    let mut sessions = Vec::new();
    for _ in 0..MAX_SESSIONS {
        sessions.push(Raw::session(&daemon, "cli", "normal").await);
    }

    let mut extra = Raw::open(&daemon).await;
    extra
        .send(&support::session_hello(
            daemon.project_root(),
            "cli",
            "normal",
        ))
        .await;
    let reply = extra.recv().await.expect("a busy reject, not silence");
    assert_eq!(reply["reject"]["code"], "busy", "{reply}");
    assert_eq!(reply["reject"]["sessions"], MAX_SESSIONS);
    assert_eq!(reply["reject"]["cap"], MAX_SESSIONS);
    assert!(extra.until_closed().await.is_empty());

    // The text a client prints for it is capacity, not absence.
    let text = orch_proto::error::ConnectFailure::Busy {
        project_root: daemon.project_root().display().to_string(),
        pid: Some(std::process::id()),
        sessions: MAX_SESSIONS as u32,
        cap: MAX_SESSIONS as u32,
    }
    .render(false);
    assert!(
        text.contains("daemon_busy") && !text.contains("No orch daemon"),
        "{text}"
    );
    assert!(!text.contains("orch daemon start"), "{text}");

    // Established sessions are unaffected, including a control operation.
    let first = &mut sessions[0];
    let status = first.request(1, "daemon.status", json!({})).await;
    assert_eq!(ok(&status)["sessions"], MAX_SESSIONS);
    assert!(ok(&status)["busy_rejections"].as_u64().unwrap() >= 1);
    let agent = shell_agent(first, &daemon, "sleeper", "exec /bin/sleep 120").await;
    let id = task(first, "long", json!({"assignee": agent})).await;
    ok(&first.request(2, "dispatch.tick", json!({})).await);
    reaper.watch(support::running_pid(&daemon, &id).await.unwrap());
    let cancelled = first.request(3, "task.cancel", json!({"id": id})).await;
    assert_eq!(ok(&cancelled)["status"], "cancelled", "{cancelled}");

    // Closing one frees exactly one slot.
    drop(sessions.pop());
    eventually!("a slot to free", {
        let status = sessions[0].request(4, "daemon.status", json!({})).await;
        ok(&status)["sessions"] == MAX_SESSIONS - 1
    });
    let mut again = Raw::session(&daemon, "cli", "normal").await;
    ok(&again.request(1, "daemon.status", json!({})).await);
    sessions.push(again);

    // Full again: the stop intent needs no ordinary slot.
    let mut stop = Raw::open(&daemon).await;
    stop.send(&support::hello(
        daemon.project_root(),
        orch_proto::LOCAL_PROTOCOL,
        orch_proto::BINARY_VERSION,
        json!({"intent": "stop"}),
    ))
    .await;
    assert_eq!(
        stop.recv().await.unwrap()["welcome"]["stop_state"],
        "accepted"
    );
    assert_eq!(
        stop.recv().await.unwrap()["welcome"]["stop_state"],
        "completed"
    );
    drop(sessions);
    daemon.shutdown().await.unwrap();
}

/// Idle half-open connections hold only pre-handshake slots: when those are
/// exhausted the next connection hears `busy`, and the slots come back when
/// the 2 s handshake deadline closes the idlers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_handshakes_use_their_own_budget_and_are_reclaimed() {
    use orch_proto::frame::limits::PRE_HANDSHAKE_SLOTS;
    let daemon = DaemonHarness::start().await.unwrap();
    let mut live = Raw::session(&daemon, "cli", "normal").await;
    let mut idlers = Vec::new();
    for _ in 0..PRE_HANDSHAKE_SLOTS {
        idlers.push(Raw::open(&daemon).await);
    }
    // One more: no slot left, so it is told it is busy rather than hanging.
    let mut overflow = Raw::open(&daemon).await;
    let reply = overflow.recv().await.expect("a busy reject");
    assert_eq!(reply["reject"]["code"], "busy", "{reply}");
    // The established session never needed a pre-handshake slot again.
    ok(&live.request(1, "daemon.status", json!({})).await);

    // The handshake deadline closes the idlers (we only wait on the daemon's
    // own close, bounded by the hang guard).
    for idler in &mut idlers {
        let rest = idler.until_closed().await;
        assert!(
            rest.iter()
                .all(|f| f["reject"]["code"] == "handshake_failed"),
            "{rest:?}"
        );
    }
    let mut fresh = Raw::session(&daemon, "cli", "normal").await;
    ok(&fresh.request(1, "daemon.status", json!({})).await);
    daemon.shutdown().await.unwrap();
}

// -- stop ordering --------------------------------------------------------

fn guard_is_free(root: &Path) -> bool {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(GUARD))
    else {
        return false;
    };
    // Dropping the Flock releases our probe at once.
    Flock::lock(file, FlockArg::LockExclusiveNonblock).is_ok()
}

/// Shutdown removes its own endpoint and discovery record first and drops the
/// owner locks last. A probe polling the whole time must never see the guard
/// free while the socket, the discovery record or the PID lock still exist
/// (if a successor could take the guard early, two owners would overlap).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_releases_files_then_locks_last() {
    let daemon = DaemonHarness::builder().on_socket().start().await.unwrap();
    let root = daemon.project_root().to_path_buf();
    let socket = daemon.socket().unwrap().to_path_buf();
    let discovery = root.join(".orchestry/daemon.json");
    assert!(socket.exists() && discovery.exists() && root.join(PID_LOCK).exists());
    assert!(
        !guard_is_free(&root),
        "the guard must be held while running"
    );

    let running = Arc::new(AtomicBool::new(true));
    let polls = Arc::new(AtomicU64::new(0));
    let violation = Arc::new(std::sync::Mutex::new(None::<String>));
    let probe = {
        let (root, socket, discovery) = (root.clone(), socket.clone(), discovery.clone());
        let (running, polls, violation) = (running.clone(), polls.clone(), violation.clone());
        std::thread::spawn(move || {
            while running.load(Ordering::SeqCst) {
                // Order matters: observe the locks first, the files after.
                let free = guard_is_free(&root);
                let pid_lock = root.join(PID_LOCK).exists();
                let (sock, disc) = (socket.exists(), discovery.exists());
                polls.fetch_add(1, Ordering::SeqCst);
                if free && (pid_lock || sock || disc) {
                    *violation.lock().unwrap() = Some(format!(
                        "guard free while pid_lock={pid_lock} socket={sock} discovery={disc}"
                    ));
                    return;
                }
            }
        })
    };
    // Make sure the probe is really sampling before the stop starts.
    let sampled_before = polls.load(Ordering::SeqCst);
    eventually!(
        "the probe to sample",
        polls.load(Ordering::SeqCst) > sampled_before + 20
    );

    let _project = daemon.quiesce().await.unwrap();
    let after_stop = polls.load(Ordering::SeqCst);
    eventually!(
        "the probe to sample after the stop",
        polls.load(Ordering::SeqCst) > after_stop + 5
    );
    running.store(false, Ordering::SeqCst);
    probe.join().unwrap();
    assert_eq!(violation.lock().unwrap().as_deref(), None);

    // Final state: everything the daemon created is gone except the stable
    // guard inode, which is never unlinked.
    assert!(!socket.exists(), "socket left behind");
    assert!(!discovery.exists(), "daemon.json left behind");
    assert!(!root.join(PID_LOCK).exists(), "PID lock left behind");
    assert!(
        root.join(GUARD).exists(),
        "the guard inode must never be unlinked"
    );
    assert!(guard_is_free(&root));
}
