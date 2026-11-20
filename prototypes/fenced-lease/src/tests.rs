use super::*;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

fn fixture() -> (TempDir, Store) {
    let dir = TempDir::new().unwrap();
    let s = Store::create(&dir.path().join("db")).unwrap();
    s.task("p", "t").unwrap();
    (dir, s)
}
fn integer(s: &Store, sql: &str) -> i64 {
    s.db.query_row(sql, [], |r| r.get(0)).unwrap()
}
fn text(s: &Store, sql: &str) -> String {
    s.db.query_row(sql, [], |r| r.get(0)).unwrap()
}
fn refused<T: std::fmt::Debug>(result: Result<T>, expected: Refusal) {
    match result {
        Err(Error::Refused(actual)) => assert_eq!(actual, expected),
        other => panic!("expected {expected:?}, got {other:?}"),
    }
}
fn token(s: &mut Store) -> Token {
    s.claim("p", "t", "a", "r1", 0, None).unwrap()
}

#[test]
fn claim_commits_all_effects() {
    let (_dir, mut s) = fixture();
    let t = token(&mut s);
    assert_eq!(t.epoch, 1);
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM leases"), 1);
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM runs"), 1);
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM retries"), 0);
    assert_eq!(
        integer(&s, "SELECT COUNT(*) FROM intents WHERE state='pending'"),
        1
    );
    assert_eq!(text(&s, "SELECT status FROM tasks"), "in_progress");
    assert_eq!(text(&s, "PRAGMA journal_mode"), "wal");
    assert_eq!(integer(&s, "PRAGMA foreign_keys"), 1);
    assert_eq!(integer(&s, "PRAGMA synchronous"), 1);
    assert_eq!(integer(&s, "PRAGMA busy_timeout"), 5000);
    assert_eq!(
        integer(
            &s,
            "SELECT COUNT(*) FROM pragma_table_list WHERE name IN ('tasks','task_epochs','runs','leases','retries','intents','daemon') AND strict=1"
        ),
        7
    );
}

#[test]
fn claim_is_atomic_at_every_step() {
    for step in 1..=6 {
        let (_dir, mut s) = fixture();
        refused(
            s.claim("p", "t", "a", "r1", 0, Some(step)),
            Refusal::Injected,
        );
        for table in ["leases", "runs", "intents", "task_epochs"] {
            assert_eq!(
                integer(&s, &format!("SELECT COUNT(*) FROM {table}")),
                0,
                "step {step}: {table}"
            );
        }
        assert_eq!(integer(&s, "SELECT COUNT(*) FROM retries"), 1);
        assert_eq!(text(&s, "SELECT status FROM tasks"), "ready");
        assert_eq!(token(&mut s).epoch, 1, "rollback must not consume an epoch");
    }
}

#[test]
fn sqlite_statement_failure_rolls_back_the_claim() {
    let (_dir, mut s) = fixture();
    s.db.execute_batch("CREATE TRIGGER fail_intent BEFORE INSERT ON intents BEGIN SELECT RAISE(ABORT,'simulated storage failure'); END;").unwrap();
    assert!(matches!(
        s.claim("p", "t", "a", "r1", 0, None),
        Err(Error::Sql(_))
    ));
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM leases"), 0);
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM runs"), 0);
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM task_epochs"), 0);
    assert_eq!(text(&s, "SELECT status FROM tasks"), "ready");
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM retries"), 1);
}

fn race(same_task: bool) {
    for _ in 0..32 {
        let (dir, s) = fixture();
        s.task("p", "u").unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|n| {
                let path = dir.path().join("db");
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut s = Store::connect(&path).unwrap();
                    barrier.wait();
                    let task = if same_task || n == 0 { "t" } else { "u" };
                    let agent = if !same_task || n == 0 { "a" } else { "b" };
                    s.claim("p", task, agent, &format!("r{n}"), 0, None)
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        for result in results.into_iter().filter(|r| r.is_err()) {
            refused(
                result,
                if same_task {
                    Refusal::TaskBusy
                } else {
                    Refusal::AgentBusy
                },
            );
        }
        assert_eq!(integer(&s, "SELECT COUNT(*) FROM runs"), 1);
        assert_eq!(integer(&s, "SELECT SUM(epoch) FROM task_epochs"), 1);
        assert_eq!(integer(&s, "SELECT COUNT(*) FROM intents"), 1);
        assert_eq!(integer(&s, "SELECT COUNT(*) FROM retries"), 1);
    }
}
#[test]
fn task_race() {
    race(true);
}
#[test]
fn agent_race() {
    race(false);
}

#[test]
fn projects_are_independent() {
    let (_dir, mut s) = fixture();
    s.task("q", "t").unwrap();
    token(&mut s);
    assert_eq!(s.claim("q", "t", "a", "r1", 0, None).unwrap().epoch, 1);
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM leases"), 2);
}

#[test]
fn wake_after_50_100_200_seconds() {
    for wake in [50, 100, 200] {
        let (_dir, mut s) = fixture();
        let first = token(&mut s);
        let mut holder = Holder::new(first.clone(), 0);
        if wake < LEASE {
            refused(s.claim("p", "t", "b", "r2", wake, None), Refusal::TaskBusy);
            assert_eq!(holder.may_work(wake), wake < GIVE_UP);
            if holder.may_work(wake) {
                // Wake requires an authority round-trip before resuming work.
                s.renew(&first, wake).unwrap();
                assert!(holder.acknowledge(wake, wake));
            }
        }
        let takeover = if wake == 50 { 170 } else { 120 };
        let second = s.claim("p", "t", "b", "r2", takeover, None).unwrap();
        assert_eq!(second.epoch, first.epoch + 1);
        assert!(!holder.may_work(wake.max(takeover)));
        refused(s.renew(&first, wake.max(takeover)), Refusal::Superseded);
        refused(
            s.finish(&first, wake.max(takeover), true),
            Refusal::Superseded,
        );
        assert_eq!(integer(&s, "SELECT attempts FROM tasks"), 0);
        assert_eq!(text(&s, "SELECT status FROM runs WHERE run='r1'"), "lost");
        assert_eq!(
            text(&s, "SELECT state FROM intents WHERE run='r1'"),
            "cancelled"
        );
        assert_eq!(
            text(&s, "SELECT status FROM runs WHERE run='r2'"),
            "running"
        );
        s.finish(&second, wake.max(takeover), true).unwrap();
    }
}

#[test]
fn exact_deadlines_and_renewal_cadence() {
    let (_dir, mut s) = fixture();
    let t = token(&mut s);
    let mut h = Holder::new(t.clone(), 0);
    assert!(!h.renewal_due(14));
    assert!(h.renewal_due(15));
    s.renew(&t, 15).unwrap();
    assert!(h.acknowledge(15, 15));
    assert!(!h.renewal_due(29));
    assert!(h.renewal_due(30));
    assert!(h.may_work(104));
    assert!(!h.may_work(105));
    assert_eq!(s.reap(134).unwrap(), 0);
    refused(s.renew(&t, 135), Refusal::Expired);
    refused(s.finish(&t, 135, true), Refusal::Expired);
    assert_eq!(s.reap(135).unwrap(), 1);
    assert_eq!(integer(&s, "SELECT attempts FROM tasks"), 0);
}

#[test]
fn renewal_send_time_bounds_authority() {
    let (_dir, mut s) = fixture();
    let t = token(&mut s);
    let mut h = Holder::new(t.clone(), 0);
    // Request sent 15, server commits 16, response arrives 80.
    s.renew(&t, 16).unwrap();
    assert!(h.acknowledge(15, 80));
    assert!(h.may_work(104));
    assert!(!h.may_work(105));
    assert_eq!(integer(&s, "SELECT deadline FROM leases"), 136);
    assert!(!h.acknowledge(100, 106), "a stopped run cannot revive");
    let mut late = Holder::new(t, 0);
    assert!(
        !late.acknowledge(15, 100),
        "late ACK must not extend the old deadline"
    );
    assert!(!late.may_work(101));
}

#[test]
fn wall_jumps_do_not_change_lease() {
    let (_dir, mut s) = fixture();
    let t = token(&mut s);
    let mut h = Holder::new(t.clone(), 0);
    // Model UTC is separate from elapsed time; it is never supplied as authority.
    for (elapsed, wall) in [(15, 86_400_i64), (30, -86_400), (45, 0)] {
        assert_ne!(wall, elapsed);
        s.renew(&t, elapsed).unwrap();
        assert!(h.acknowledge(elapsed, elapsed));
        assert!(h.may_work(elapsed));
        refused(
            s.claim("p", "t", "b", "r2", elapsed, None),
            Refusal::TaskBusy,
        );
    }
    assert!(h.may_work(134));
    assert!(!h.may_work(135));
    assert_eq!(s.reap(164).unwrap(), 0);
    assert_eq!(s.reap(165).unwrap(), 1);
}

#[test]
fn wall_clock_and_paused_clock_counterexamples() {
    let (_dir, mut s) = fixture();
    let first = token(&mut s);
    // Naive UTC expiry: a forward jump reassigns while only 50 seconds elapsed.
    let second = s.claim("p", "t", "b", "r2", 86_450, None).unwrap();
    let mut holder = Holder::new(first.clone(), 0);
    assert!(
        holder.may_work(50),
        "counterexample: UTC and elapsed authority disagree"
    );
    refused(s.finish(&first, 86_450, true), Refusal::Superseded);
    assert!(second.epoch > first.epoch);
    let (_back_dir, mut backward) = fixture();
    token(&mut backward);
    refused(
        backward.claim("p", "t", "b", "r2", -86_200, None),
        Refusal::TaskBusy,
    );
    assert_eq!(
        backward.reap(-86_200).unwrap(),
        0,
        "counterexample: UTC jump hides 200 elapsed seconds"
    );
    assert!(backward.claim("p", "t", "b", "r2", 200, None).is_ok());
    let mut paused_clock_holder = Holder::new(first.clone(), 0);
    assert!(
        paused_clock_holder.may_work(0),
        "counterexample: clock excluding 200 seconds of sleep permits work"
    );
    let mut continuous_clock_holder = Holder::new(first, 0);
    assert!(!continuous_clock_holder.may_work(200));
}

#[test]
fn restart_grace_and_reattach() {
    let (dir, mut s) = fixture();
    s.task("p", "u").unwrap();
    let first = s.claim("p", "t", "a", "r1", 1000, None).unwrap();
    let mut stale_connection = Store::connect(&dir.path().join("db")).unwrap();
    drop(s);
    let mut s = Store::connect(&dir.path().join("db")).unwrap();
    s.restart("session-2", 0).unwrap();
    refused(
        stale_connection.claim("p", "u", "b", "r2", 0, None),
        Refusal::Session,
    );
    refused(s.renew(&first, 0), Refusal::Reattach);
    refused(s.finish(&first, 0, true), Refusal::Reattach);
    assert_eq!(s.reap(119).unwrap(), 0);
    refused(s.claim("p", "t", "b", "r2", 119, None), Refusal::TaskBusy);
    refused(s.claim("p", "u", "a", "r2", 119, None), Refusal::AgentBusy);
    let attached = s.reattach(&first, 119).unwrap();
    assert_eq!(attached.epoch, first.epoch);
    assert_eq!(attached.run, first.run);
    assert_ne!(attached.session, first.session);
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM runs"), 1);
    assert_eq!(s.reap(120).unwrap(), 0);
    refused(s.finish(&first, 120, true), Refusal::Session);
    s.finish(&attached, 120, true).unwrap();
}

#[test]
fn restart_after_grace_loses_without_attempt() {
    let (dir, mut s) = fixture();
    let first = token(&mut s);
    drop(s);
    let mut s = Store::connect(&dir.path().join("db")).unwrap();
    s.restart("session-2", 0).unwrap();
    refused(s.reattach(&first, 120), Refusal::Expired);
    let second = s.claim("p", "t", "b", "r2", 120, None).unwrap();
    refused(s.reattach(&first, 120), Refusal::Superseded);
    assert_eq!(second.epoch, 2);
    assert_eq!(integer(&s, "SELECT attempts FROM tasks"), 0);
    s.finish(&second, 121, false).unwrap();
    assert_eq!(integer(&s, "SELECT attempts FROM tasks"), 1);
}

#[test]
fn reattach_does_not_revive_a_stopped_holder() {
    let (_dir, mut s) = fixture();
    let first = token(&mut s);
    let mut h = Holder::new(first.clone(), 0);
    assert!(!h.may_work(100));
    s.restart("session-2", 100).unwrap();
    // Storage may recover an otherwise valid token. The stopped holder still must not work.
    let recovered = s.reattach(&first, 101).unwrap();
    h.token = recovered;
    assert!(!h.acknowledge(101, 101));
    assert!(!h.may_work(102));
}

#[test]
fn snapshot_maximum_repeats_a_discarded_epoch() {
    let (dir, mut live) = fixture();
    let first = token(&mut live);
    let snapshot = dir.path().join("snapshot");
    live.backup(&snapshot).unwrap();
    let discarded = live.claim("p", "t", "b", "r2", 120, None).unwrap();
    let mut restored = Store::connect(&snapshot).unwrap();
    // Reaping and max(restored counter/runs/leases)+1 is precisely the proposed numeric rule.
    restored.reap(120).unwrap();
    let next = restored.claim("p", "t", "c", "r3", 120, None).unwrap();
    assert!(next.epoch > first.epoch);
    assert_eq!(
        next.epoch, discarded.epoch,
        "snapshot lost evidence of epoch 2: rule is false"
    );
    assert_eq!(next.lineage, discarded.lineage);
}

#[test]
fn restore_lineage_fences_discarded_future() {
    let (dir, mut live) = fixture();
    let old_snapshot_token = token(&mut live);
    let snapshot = dir.path().join("snapshot");
    live.backup(&snapshot).unwrap();
    let discarded = live.claim("p", "t", "b", "r2", 120, None).unwrap();
    let mut restored = Store::connect(&snapshot).unwrap();
    restored
        .restore("fresh-lineage-2", "restore-session")
        .unwrap();
    let next = restored.claim("p", "t", "b", "r2", 0, None).unwrap();
    // Same task/run/agent/numeric epoch, but old lineage and session are never accepted.
    assert_eq!(next.epoch, discarded.epoch);
    assert_ne!(next.lineage, discarded.lineage);
    for stale in [old_snapshot_token, discarded] {
        refused(restored.renew(&stale, 1), Refusal::Superseded);
        refused(restored.finish(&stale, 1, true), Refusal::Superseded);
        refused(restored.reattach(&stale, 1), Refusal::Superseded);
    }
    assert_eq!(integer(&restored, "SELECT attempts FROM tasks"), 0);
    restored.finish(&next, 1, true).unwrap();
}

#[test]
fn restored_counters_include_runs_leases_and_tombstones() {
    let (_dir, mut s) = fixture();
    token(&mut s);
    s.task("p", "u").unwrap();
    s.db.execute_batch(
        "UPDATE runs SET epoch=7; UPDATE leases SET epoch=9;
        INSERT INTO task_epochs VALUES ('p','u',20);
        INSERT INTO task_epochs VALUES ('p','deleted-task',40);",
    )
    .unwrap();
    s.restore("fresh-lineage-2", "session-2").unwrap();
    assert_eq!(s.claim("p", "t", "a", "r2", 0, None).unwrap().epoch, 10);
    assert_eq!(s.claim("p", "u", "b", "u1", 0, None).unwrap().epoch, 21);
    s.task("p", "deleted-task").unwrap();
    assert_eq!(
        s.claim("p", "deleted-task", "c", "d1", 0, None)
            .unwrap()
            .epoch,
        41
    );
}

#[test]
fn epoch_survives_task_deletion() {
    let (_dir, mut s) = fixture();
    let first = token(&mut s);
    s.finish(&first, 1, true).unwrap();
    s.db.execute_batch("DELETE FROM intents; DELETE FROM runs; DELETE FROM tasks;")
        .unwrap();
    s.task("p", "t").unwrap();
    assert_eq!(s.claim("p", "t", "a", "r2", 2, None).unwrap().epoch, 2);
    assert!(s.db.execute("DELETE FROM task_epochs", []).is_err());
    assert!(s.db.execute("UPDATE task_epochs SET epoch=0", []).is_err());
}

#[test]
fn exhausted_counter_and_clock_overflow_refuse_cleanly() {
    let (_dir, mut s) = fixture();
    s.db.execute("INSERT INTO task_epochs VALUES ('p','t',?1)", [i64::MAX])
        .unwrap();
    refused(s.claim("p", "t", "a", "r1", 0, None), Refusal::Overflow);
    s.task("p", "u").unwrap();
    refused(
        s.claim("p", "u", "a", "r1", i64::MAX, None),
        Refusal::Overflow,
    );
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM leases"), 0);
    assert_eq!(integer(&s, "SELECT COUNT(*) FROM runs"), 0);
    assert_eq!(
        integer(&s, "SELECT COUNT(*) FROM task_epochs WHERE task='u'"),
        0
    );
}
