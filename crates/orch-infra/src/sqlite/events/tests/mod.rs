//! The event operations on the real 1.1 schema, through the real writer and read
//! pool. The first eight tests are the cases of `orch-engine`'s
//! `storage_contract/events_v2.rs` under the same names, with the same vectors,
//! so that the memory and file references and this adapter are held to one
//! contract. The rest are what only SQLite can get wrong: the stored form, a
//! damaged row, hostile ids, snapshots, a returned error and a reopen.
//!
//! Leases are seeded with plain SQL: lease CRUD belongs to another slice, and
//! these operations only read the row it will write.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orch_domain::{RunEvent, RunEventType};
use rusqlite::{Transaction, params};

use super::*;
use crate::data_dir::{DataDir, Holder, LockKind};
use crate::sqlite::migrations::migrate;
use crate::sqlite::{
    ConnectionRole, Lane, ReadPool, SqliteError, Synchronous, Writer, WriterHandle, open_connection,
};

type SqlResult<T> = crate::sqlite::Result<T>;

fn event(seq: u64) -> SequencedEvent {
    SequencedEvent {
        seq,
        event: RunEvent {
            timestamp: "2026-01-01T00:00:00Z".into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::json!({"seq": seq}),
        },
    }
}

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

fn task_caller(project: &str, task: &str, run: &str, lineage: &str, epoch: u64) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Task {
            identity: "synthetic".into(),
            task: task.into(),
            run: run.into(),
            lineage: lineage.into(),
            epoch,
        },
    }
}

/// The contract's refusals, which are all a test inspects; anything else fails it.
fn store<T>(result: Result<T>) -> std::result::Result<T, StoreError> {
    match result {
        Ok(value) => Ok(value),
        Err(EventsError::Store(refusal)) => Err(refusal),
        Err(other) => panic!("not a refusal of the contract: {other}"),
    }
}

fn acquire(path: &Path) -> DataDir {
    DataDir::acquire(
        path,
        &Holder {
            kind: LockKind::Daemon,
            pid: std::process::id(),
            started_at: "synthetic".into(),
            binary_version: "test".into(),
            executable: "/synthetic/orch".into(),
        },
    )
    .unwrap()
}

/// A migrated database with the leases of the shared fixture, in the lineage
/// the database made for itself: ("p", run -> task, epoch 2), ("other", run ->
/// task, epoch 2) and ("p", another -> task2, epoch 3).
struct Fixture {
    _temp: tempfile::TempDir,
    path: PathBuf,
    lineage: String,
}

fn fixture() -> (Fixture, DataDir) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data");
    let data = acquire(&path);
    {
        let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
        migrate(&data, &mut conn).unwrap();
    }
    let lineage = with_writer(&data, |w| {
        submit(w, Lane::Claims, |tx| {
            let lineage = current_lineage(tx);
            for (project, run, task, epoch) in [
                ("p", "run", "task", 2),
                ("other", "run", "task", 2),
                ("p", "another", "task2", 3),
            ] {
                lease(tx, project, task, run, &lineage, epoch, "active");
            }
            Ok(lineage)
        })
        .unwrap()
    });
    (
        Fixture {
            _temp: temp,
            path,
            lineage,
        },
        data,
    )
}

fn current_lineage(tx: &Transaction<'_>) -> String {
    tx.query_row(
        "SELECT value FROM daemon_config WHERE key = 'lineage'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn lease(
    tx: &Transaction<'_>,
    project: &str,
    task: &str,
    run: &str,
    lineage: &str,
    epoch: u64,
    state: &str,
) {
    tx.execute(
        "INSERT INTO leases (project_id, task_id, run_id, agent_id, worker_id, epoch, lineage,
                             state, granted_at, renewed_at, expires_at)
         VALUES (?1, ?2, ?3, ?2, 'local_worker', ?4, ?5, ?6, 0, 0, 1)",
        params![project, task, run, epoch as i64, lineage, state],
    )
    .unwrap();
}

/// Closes admission when dropped, so a test that panics ends the writer thread
/// instead of hanging the scope's join.
struct Close<'a>(&'a WriterHandle);

impl Drop for Close<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Runs `body` while a writer serves the handle it is given, then drains it.
fn with_writer<R>(data: &DataDir, body: impl FnOnce(&WriterHandle) -> R) -> R {
    let (writer, handle) = Writer::open(data, Synchronous::Normal, 8).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| writer.run());
        let close = Close(&handle);
        let out = body(&handle);
        drop(close);
        worker.join().unwrap();
        out
    })
}

fn submit<T: Send + 'static>(
    handle: &WriterHandle,
    lane: Lane,
    work: impl FnOnce(&Transaction<'_>) -> SqlResult<T> + Send + 'static,
) -> SqlResult<T> {
    handle.submit(lane, None, work).unwrap().wait()
}

fn snapshot<T>(pool: &ReadPool<'_>, read: impl FnOnce(&Transaction<'_>) -> T) -> T {
    pool.read(Instant::now() + Duration::from_secs(5), |tx| Ok(read(tx)))
        .unwrap()
}

/// One committed append of `batch` to run `run` of project `project`.
fn append(
    handle: &WriterHandle,
    fx: &Fixture,
    project: &str,
    batch: Vec<SequencedEvent>,
) -> std::result::Result<AppendResult, StoreError> {
    let (project, lineage) = (project.to_owned(), fx.lineage.clone());
    submit(handle, Lane::Events, move |tx| {
        Ok(store(append_events(
            tx,
            &owner(&project),
            "run",
            &lineage,
            2,
            &batch,
        )))
    })
    .unwrap()
}

fn all(pool: &ReadPool<'_>, project: &str, run: &str) -> Vec<SequencedEvent> {
    snapshot(pool, |tx| {
        read_events(tx, project, run, 0, MAX_ROWS).unwrap()
    })
}

fn gaps(pool: &ReadPool<'_>, project: &str, run: &str) -> Vec<EventGap> {
    snapshot(pool, |tx| read_event_gaps(tx, project, run).unwrap())
}

fn pool(data: &DataDir) -> ReadPool<'_> {
    ReadPool::open(data, Synchronous::Normal, 2).unwrap()
}

// ---- the shared cases of events_v2.rs ----

#[test]
fn duplicate_counts_first_bytes_and_range_tail_order() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        assert_eq!(
            append(w, &fx, "p", vec![event(3), event(1), event(2), event(2)]).unwrap(),
            AppendResult {
                stored: 3,
                skipped: 1
            }
        );
        let mut replacement = event(2);
        replacement.event.data = serde_json::json!("replacement");
        assert_eq!(
            append(w, &fx, "p", vec![replacement, event(4)]).unwrap(),
            AppendResult {
                stored: 1,
                skipped: 1
            }
        );
        let pool = pool(&data);
        snapshot(&pool, |tx| {
            assert_eq!(
                read_events(tx, "p", "run", 1, 2).unwrap(),
                vec![event(2), event(3)]
            );
            assert_eq!(
                read_events_tail(tx, "p", "run", 2).unwrap(),
                vec![event(3), event(4)]
            );
            assert_eq!(
                read_events_tail(tx, "p", "run", 100).unwrap(),
                (1..=4).map(event).collect::<Vec<_>>()
            );
            assert!(read_events(tx, "p", "run", u64::MAX, 1).unwrap().is_empty());
        });
    });
}

#[test]
fn projects_and_runs_are_separate_and_reads_are_snapshots() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        let pool = pool(&data);
        // The snapshot is pinned by its first read; a commit that lands while
        // it is open stays invisible to it and shows to the next one.
        snapshot(&pool, |tx| {
            assert!(read_events(tx, "p", "run", 0, 10).unwrap().is_empty());
            append(w, &fx, "p", vec![event(1)]).unwrap();
            assert!(read_events(tx, "p", "run", 0, 10).unwrap().is_empty());
        });
        assert!(all(&pool, "other", "run").is_empty());
        assert!(matches!(
            append(w, &fx, "unseeded", vec![event(1)]),
            Err(StoreError::StaleEpoch)
        ));
        append(w, &fx, "other", vec![event(8)]).unwrap();
        assert_eq!(all(&pool, "p", "run"), vec![event(1)]);
        assert_eq!(all(&pool, "other", "run"), vec![event(8)]);
        snapshot(&pool, |tx| {
            assert!(read_events_tail(tx, "p", "missing", 10).unwrap().is_empty());
        });
    });
}

#[test]
fn stale_lineage_epoch_missing_lease_and_task_mismatch_change_nothing() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        append(w, &fx, "p", vec![event(1)]).unwrap();
        let lineage = fx.lineage.clone();
        let refused = submit(w, Lane::Events, move |tx| {
            let before = tx.total_changes();
            for (run, given, epoch) in [
                ("run", "old", 2),
                ("run", lineage.as_str(), 1),
                ("missing", lineage.as_str(), 2),
            ] {
                for batch in [vec![], vec![event(1)], vec![event(2)]] {
                    assert!(matches!(
                        store(append_events(tx, &owner("p"), run, given, epoch, &batch)),
                        Err(StoreError::StaleEpoch)
                    ));
                }
            }
            for (task, run, held, epoch) in [
                ("wrong", "run", lineage.as_str(), 2),
                ("task", "wrong", lineage.as_str(), 2),
                ("task", "run", "old", 2),
                ("task", "run", lineage.as_str(), 1),
            ] {
                let caller = task_caller("p", task, run, held, epoch);
                assert!(matches!(
                    store(append_events(tx, &caller, "run", &lineage, 2, &[event(2)])),
                    Err(StoreError::StaleEpoch)
                ));
            }
            // Not a statement of any refusal reached a table.
            Ok(tx.total_changes() - before)
        })
        .unwrap();
        assert_eq!(refused, 0);
        assert_eq!(all(&pool(&data), "p", "run"), vec![event(1)]);
    });
}

#[test]
fn transaction_error_rolls_back_append_gap_prune_and_entities() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        append(w, &fx, "p", vec![event(1), event(2)]).unwrap();
        let lineage = fx.lineage.clone();
        let outcome = submit(w, Lane::Events, move |tx| {
            // The waiting batches of two runs share this one transaction (R26).
            append_events(tx, &owner("p"), "run", &lineage, 2, &[event(3)]).unwrap();
            append_events(tx, &owner("p"), "another", &lineage, 3, &[event(9)]).unwrap();
            record_event_gap(tx, "p", "run", 3, 2).unwrap();
            prune_events(tx, "p", "run", 1).unwrap();
            tx.execute(
                "INSERT INTO context_entries (project_id, key, value, set_by_kind, position, revision)
                 VALUES ('p', 'rollback', 'v', 'owner', 1, 1)",
                [],
            )?;
            Err::<(), _>(SqliteError::Migration("injected rollback".into()))
        });
        assert!(matches!(outcome, Err(SqliteError::Migration(_))));
        let pool = pool(&data);
        assert_eq!(all(&pool, "p", "run"), vec![event(1), event(2)]);
        assert!(all(&pool, "p", "another").is_empty());
        assert!(gaps(&pool, "p", "run").is_empty());
        let entities: i64 = snapshot(&pool, |tx| {
            tx.query_row("SELECT count(*) FROM context_entries", [], |r| r.get(0))
                .unwrap()
        });
        assert_eq!(entities, 0);
        // The writer carries on, and the same work committed keeps all of it.
        let lineage = fx.lineage.clone();
        submit(w, Lane::Events, move |tx| {
            append_events(tx, &owner("p"), "run", &lineage, 2, &[event(3)]).unwrap();
            append_events(tx, &owner("p"), "another", &lineage, 3, &[event(9)]).unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(all(&pool, "p", "another"), vec![event(9)]);
    });
}

#[test]
fn gaps_and_bounded_prune_preserve_other_runs_and_projects() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        append(w, &fx, "other", vec![event(8)]).unwrap();
        append(w, &fx, "p", vec![event(1), event(4), event(5)]).unwrap();
        let lineage = fx.lineage.clone();
        submit(w, Lane::Events, move |tx| {
            record_event_gap(tx, "p", "run", 1, 2).unwrap();
            append_events(tx, &owner("p"), "another", &lineage, 3, &[event(9)]).unwrap();
            Ok(())
        })
        .unwrap();
        let pool = pool(&data);
        let before = snapshot(&pool, |tx| read_events(tx, "p", "run", 0, 10).unwrap());
        assert_eq!(before.len(), 3);
        for (deleted, complete) in [(2, false), (1, true), (0, true)] {
            let result = submit(w, Lane::Maintenance, |tx| {
                Ok(prune_events(tx, "p", "run", 2).unwrap())
            })
            .unwrap();
            assert_eq!(result, PruneResult { deleted, complete });
            if !complete {
                assert_eq!(all(&pool, "p", "run"), vec![event(5)]);
            }
        }
        assert_eq!(all(&pool, "p", "another"), vec![event(9)]);
        assert_eq!(
            gaps(&pool, "p", "run"),
            vec![EventGap {
                after_seq: 1,
                dropped: 2
            }]
        );
        assert_eq!(all(&pool, "other", "run"), vec![event(8)]);
        assert!(gaps(&pool, "other", "run").is_empty());
    });
}

#[test]
fn empty_tails_batches_and_invalid_bounds_are_explicit() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        assert_eq!(
            append(w, &fx, "p", vec![]).unwrap(),
            AppendResult {
                stored: 0,
                skipped: 0
            }
        );
        let pool = pool(&data);
        snapshot(&pool, |tx| {
            assert!(read_events_tail(tx, "p", "run", 0).unwrap().is_empty());
            assert!(read_events_tail(tx, "p", "run", 10).unwrap().is_empty());
            assert!(read_events(tx, "p", "run", 0, 0).is_err());
            assert!(read_events(tx, "p", "run", 0, 1_001).is_err());
            assert!(read_events_tail(tx, "p", "run", 1_001).is_err());
        });
        for bound in [0, 1_001] {
            let refused = submit(w, Lane::Maintenance, move |tx| {
                Ok(store(prune_events(tx, "p", "run", bound)))
            })
            .unwrap();
            assert!(matches!(refused, Err(StoreError::InvalidEvents(_))));
        }
        let lineage = fx.lineage.clone();
        submit(w, Lane::Events, move |tx| {
            // A refusal caught by the caller, which then commits, stores nothing
            // of the batch: a valid row before the bad one is not kept.
            for bad in [0, i64::MAX as u64 + 1, u64::MAX] {
                assert!(matches!(
                    store(append_events(
                        tx,
                        &owner("p"),
                        "run",
                        &lineage,
                        2,
                        &[event(1), event(bad)]
                    )),
                    Err(StoreError::InvalidEvents(_))
                ));
            }
            for (after, dropped) in [(1, 0), (u64::MAX, 1), (i64::MAX as u64, 1), (1, u64::MAX)] {
                assert!(matches!(
                    store(record_event_gap(tx, "p", "run", after, dropped)),
                    Err(StoreError::InvalidEvents(_))
                ));
            }
            Ok(())
        })
        .unwrap();
        assert!(all(&pool, "p", "run").is_empty());
        assert!(gaps(&pool, "p", "run").is_empty());
    });
}

#[test]
fn matching_task_fence_is_accepted() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        let lineage = fx.lineage.clone();
        submit(w, Lane::Events, move |tx| {
            let caller = task_caller("p", "task", "run", &lineage, 2);
            assert_eq!(
                append_events(tx, &caller, "run", &lineage, 2, &[event(1), event(2)]).unwrap(),
                AppendResult {
                    stored: 2,
                    skipped: 0
                }
            );
            record_event_gap(tx, "p", "run", 2, 1).unwrap();
            Ok(())
        })
        .unwrap();
        let pool = pool(&data);
        snapshot(&pool, |tx| {
            assert!(read_events_tail(tx, "p", "run", 0).unwrap().is_empty());
            assert_eq!(
                read_events(tx, "p", "run", 1, 1_000).unwrap(),
                vec![event(2)]
            );
        });
        assert_eq!(
            gaps(&pool, "p", "run"),
            vec![EventGap {
                after_seq: 2,
                dropped: 1
            }]
        );
    });
}

#[test]
fn repeated_gap_is_idempotent_and_conflict_changes_nothing() {
    let (_fx, data) = fixture();
    with_writer(&data, |w| {
        for attempt in 0..3 {
            submit(w, Lane::Events, move |tx| {
                if attempt < 2 {
                    record_event_gap(tx, "p", "run", 5, 3).unwrap();
                } else {
                    // Catch the refusal and commit: the operation is still atomic.
                    assert!(matches!(
                        store(record_event_gap(tx, "p", "run", 5, 4)),
                        Err(StoreError::InvalidEvents(_))
                    ));
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(
                gaps(&pool(&data), "p", "run"),
                vec![EventGap {
                    after_seq: 5,
                    dropped: 3
                }]
            );
        }
        submit(w, Lane::Events, |tx| {
            record_event_gap(tx, "other", "run", 5, 9).unwrap();
            Ok(())
        })
        .unwrap();
        submit(w, Lane::Events, |tx| {
            record_event_gap(tx, "p", "another", 5, 8).unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(
            gaps(&pool(&data), "p", "run"),
            vec![EventGap {
                after_seq: 5,
                dropped: 3
            }]
        );
    });
}

// ---- what only this adapter can get wrong ----

#[test]
fn a_repeated_gap_writes_no_row_and_gaps_read_in_order_of_their_start() {
    let (_fx, data) = fixture();
    with_writer(&data, |w| {
        let writes = submit(w, Lane::Events, |tx| {
            record_event_gap(tx, "p", "run", 20, 2).unwrap();
            record_event_gap(tx, "p", "run", 5, 3).unwrap();
            let before = tx.total_changes();
            record_event_gap(tx, "p", "run", 5, 3).unwrap();
            assert!(record_event_gap(tx, "p", "run", 5, 4).is_err());
            Ok(tx.total_changes() - before)
        })
        .unwrap();
        assert_eq!(writes, 0);
        let order: Vec<u64> = gaps(&pool(&data), "p", "run")
            .iter()
            .map(|g| g.after_seq)
            .collect();
        assert_eq!(order, [5, 20]);
    });
}

#[test]
fn events_gaps_and_exact_bytes_survive_a_reopen() {
    let (fx, data) = fixture();
    let odd = SequencedEvent {
        seq: 2,
        event: RunEvent {
            timestamp: "not a time, as an imported line may carry".into(),
            event_type: RunEventType::ToolCall,
            data: serde_json::json!({
                "text": "quote \" backslash \\ nul \u{0} emoji \u{1F600} \u{202E}rtl",
                "nested": [1, 2.5, null, {"k": [true]}],
                "big": 18446744073709551615u64,
            }),
        },
    };
    let rfc3339 = SequencedEvent {
        seq: 3,
        event: RunEvent {
            timestamp: "2026-01-02T03:04:05.678+02:00".into(),
            event_type: RunEventType::Done,
            data: serde_json::Value::String("x".repeat(8_192)),
        },
    };
    let batch = vec![odd.clone(), event(1), rfc3339.clone()];
    with_writer(&data, |w| {
        append(w, &fx, "p", batch.clone()).unwrap();
        let writes = |tx: &Transaction<'_>| {
            record_event_gap(tx, "p", "run", 3, 7).unwrap();
            Ok(())
        };
        submit(w, Lane::Events, writes).unwrap();
    });
    let path = fx.path.clone();
    drop(data);

    let data = acquire(&path);
    let pool = pool(&data);
    assert_eq!(all(&pool, "p", "run"), vec![event(1), odd, rfc3339]);
    assert_eq!(
        gaps(&pool, "p", "run"),
        vec![EventGap {
            after_seq: 3,
            dropped: 7
        }]
    );
    let (at, kind): (Vec<i64>, Vec<String>) = snapshot(&pool, |tx| {
        let mut select = tx
            .prepare("SELECT at, type FROM run_events ORDER BY seq")
            .unwrap();
        select
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .unzip()
    });
    // 2026-01-02T01:04:05.678Z in milliseconds; 0 for text that is no instant.
    assert_eq!(at, [1_767_225_600_000, 0, 1_767_315_845_678]);
    assert_eq!(kind, ["agent_output", "tool_call", "done"]);
}

#[test]
fn a_damaged_row_is_reported_and_never_skipped() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        append(w, &fx, "p", vec![event(1), event(2), event(3)]).unwrap();
        let pool = pool(&data);
        for (damage, case) in [
            (
                "UPDATE run_events SET payload = 'not json' WHERE seq = 2",
                "payload",
            ),
            (
                "UPDATE run_events SET payload = '{\"timestamp\":\"t\",\"type\":\"done\",\"data\":1}' WHERE seq = 2",
                "type column",
            ),
            (
                "UPDATE run_events SET type = 'no_such_type' WHERE seq = 2",
                "unknown type",
            ),
        ] {
            submit(w, Lane::Maintenance, move |tx| {
                tx.execute_batch(damage)?;
                Ok(())
            })
            .unwrap();
            snapshot(&pool, |tx| {
                for read in [
                    read_events(tx, "p", "run", 0, 10),
                    read_events_tail(tx, "p", "run", 10),
                ] {
                    assert!(matches!(read, Err(EventsError::Corrupt(_))), "{case}");
                }
                // A range that does not reach the row is still readable.
                assert_eq!(read_events(tx, "p", "run", 2, 10).unwrap(), vec![event(3)]);
            });
            submit(w, Lane::Maintenance, |tx| {
                let good = event(2).event;
                tx.execute(
                    "UPDATE run_events SET type = 'agent_output', payload = ?1 WHERE seq = 2",
                    [serde_json::to_string(&good).unwrap()],
                )?;
                Ok(())
            })
            .unwrap();
            assert_eq!(all(&pool, "p", "run").len(), 3, "{case}");
        }
    });
}

#[test]
fn hostile_run_ids_are_data_and_never_match_each_other() {
    let (fx, data) = fixture();
    let runs = [
        "'; DROP TABLE run_events; --",
        "r%",
        "r_",
        "run",
        "",
        "caf\u{e9}",
        "CAF\u{c9}",
        "run\u{0}tail",
    ];
    with_writer(&data, |w| {
        let lineage = fx.lineage.clone();
        submit(w, Lane::Claims, move |tx| {
            for (i, run) in runs.iter().enumerate().filter(|(_, r)| **r != "run") {
                lease(tx, "p", &format!("hostile{i}"), run, &lineage, 1, "active");
            }
            Ok(())
        })
        .unwrap();
        let lineage = fx.lineage.clone();
        submit(w, Lane::Events, move |tx| {
            for (i, run) in runs.iter().enumerate() {
                let epoch = if *run == "run" { 2 } else { 1 };
                let at = i as u64 + 1;
                let stored =
                    append_events(tx, &owner("p"), run, &lineage, epoch, &[event(at)]).unwrap();
                assert_eq!(stored.stored, 1, "{run:?}");
            }
            Ok(())
        })
        .unwrap();
        let pool = pool(&data);
        for (i, run) in runs.iter().enumerate() {
            assert_eq!(all(&pool, "p", run), vec![event(i as u64 + 1)], "{run:?}");
        }
        let pruned = submit(w, Lane::Maintenance, |tx| {
            Ok(prune_events(tx, "p", "r%", 10).unwrap())
        })
        .unwrap();
        assert_eq!(
            pruned,
            PruneResult {
                deleted: 1,
                complete: true
            }
        );
        assert_eq!(all(&pool, "p", "r_").len(), 1);
    });
}

#[test]
fn a_lease_that_is_not_active_cannot_append() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        let lineage = fx.lineage.clone();
        submit(w, Lane::Claims, move |tx| {
            lease(
                tx,
                "p",
                "offered-task",
                "offered-run",
                &lineage,
                1,
                "offered",
            );
            Ok(())
        })
        .unwrap();
        let lineage = fx.lineage.clone();
        let refused = submit(w, Lane::Events, move |tx| {
            Ok(store(append_events(
                tx,
                &owner("p"),
                "offered-run",
                &lineage,
                1,
                &[event(1)],
            )))
        })
        .unwrap();
        assert!(matches!(refused, Err(StoreError::StaleEpoch)));
    });
}

#[test]
fn a_lineage_replaced_after_the_lease_was_granted_refuses_both_lineages() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        append(w, &fx, "p", vec![event(1)]).unwrap();
        // What a restore does to the lineage; the lease it left is of the old one.
        submit(w, Lane::Maintenance, |tx| {
            tx.execute(
                "UPDATE daemon_config SET value = 'after-restore' WHERE key = 'lineage'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        for given in [fx.lineage.clone(), "after-restore".to_owned()] {
            let refused = submit(w, Lane::Events, move |tx| {
                let before = tx.total_changes();
                let refused = [vec![], vec![event(1)], vec![event(2)]]
                    .map(|batch| store(append_events(tx, &owner("p"), "run", &given, 2, &batch)));
                assert_eq!(tx.total_changes(), before);
                Ok(refused)
            })
            .unwrap();
            assert!(
                refused
                    .iter()
                    .all(|r| matches!(r, Err(StoreError::StaleEpoch)))
            );
        }
        assert_eq!(all(&pool(&data), "p", "run"), vec![event(1)]);
    });
}

#[test]
fn a_missing_lineage_is_reported_as_damage_not_as_a_stale_lease() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        submit(w, Lane::Maintenance, |tx| {
            tx.execute("DELETE FROM daemon_config WHERE key = 'lineage'", [])?;
            Ok(())
        })
        .unwrap();
        let lineage = fx.lineage.clone();
        let outcome = submit(w, Lane::Events, move |tx| {
            Ok(append_events(
                tx,
                &owner("p"),
                "run",
                &lineage,
                2,
                &[event(1)],
            ))
        })
        .unwrap();
        assert!(matches!(outcome, Err(EventsError::Corrupt(_))));
    });
}

#[test]
fn a_large_batch_is_stored_whole_and_read_back_in_bounded_pages() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        let batch: Vec<_> = (1..=2_500).rev().map(event).collect();
        assert_eq!(
            append(w, &fx, "p", batch).unwrap(),
            AppendResult {
                stored: 2_500,
                skipped: 0
            }
        );
        let pool = pool(&data);
        let mut after = 0;
        let mut seen = 0;
        snapshot(&pool, |tx| {
            loop {
                let page = read_events(tx, "p", "run", after, MAX_ROWS).unwrap();
                let Some(last) = page.last() else { break };
                assert!(page.windows(2).all(|pair| pair[0].seq < pair[1].seq));
                after = last.seq;
                seen += page.len();
            }
        });
        assert_eq!(seen, 2_500);
        let tail = snapshot(&pool, |tx| read_events_tail(tx, "p", "run", 3).unwrap());
        assert_eq!(tail, vec![event(2_498), event(2_499), event(2_500)]);
        let mut left = 2_500;
        while left > 0 {
            let result = submit(w, Lane::Maintenance, |tx| {
                Ok(prune_events(tx, "p", "run", MAX_ROWS).unwrap())
            })
            .unwrap();
            assert_eq!(result.deleted, left.min(MAX_ROWS));
            left -= result.deleted;
            assert_eq!(result.complete, left == 0);
        }
    });
}

mod adapter;
mod async_waits;
