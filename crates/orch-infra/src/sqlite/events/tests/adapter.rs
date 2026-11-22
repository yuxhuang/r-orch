use super::super::adapter::EventAdapter;
use super::*;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn typed_adapter_binds_all_operations_to_caller_and_keeps_one_snapshot() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let p = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
        };
        let other = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("other"),
        };
        let lineage = fx.lineage.clone();
        p.write(None, move |unit| {
            assert_eq!(
                unit.append("run", &lineage, 2, &[event(1), event(1), event(2)])?,
                AppendResult {
                    stored: 2,
                    skipped: 1
                }
            );
            unit.gap("run", 2, 3)?;
            unit.gap("run", 2, 3)
        })
        .unwrap();
        other
            .snapshot(deadline(), |read| {
                assert!(read.events("run", 0, 100)?.is_empty());
                assert!(read.gaps("run")?.is_empty());
                Ok(())
            })
            .unwrap();
        assert_eq!(other.prune("run".into(), 100).unwrap().deleted, 0);
        p.snapshot(deadline(), |read| {
            let before = read.events("run", 0, 100)?;
            assert_eq!(before.len(), 2);
            let lineage = fx.lineage.clone();
            p.write(None, move |unit| {
                unit.append("run", &lineage, 2, &[event(3)])
            })
            .unwrap();
            assert_eq!(read.events("run", 0, 100)?, before);
            assert_eq!(read.tail("run", 1)?, vec![event(2)]);
            assert_eq!(
                read.gaps("run")?,
                vec![EventGap {
                    after_seq: 2,
                    dropped: 3
                }]
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(
            p.prune("run".into(), 1).unwrap(),
            PruneResult {
                deleted: 1,
                complete: false
            }
        );
        p.snapshot(deadline(), |read| {
            assert_eq!(read.tail("run", 100)?, vec![event(2), event(3)]);
            assert_eq!(read.gaps("run")?.len(), 1);
            assert!(matches!(
                read.events("run", 0, 0),
                Err(StoreError::InvalidEvents(_))
            ));
            Ok(())
        })
        .unwrap();
    });
}

#[test]
fn typed_refusals_abort_prior_work_and_check_duplicate_and_empty_fences() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let p = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
        };
        let lineage = fx.lineage.clone();
        let error = p
            .write(None, move |unit| {
                unit.append("run", &lineage, 2, &[event(1)])?;
                unit.gap("run", 1, 3)?;
                unit.append("run", &lineage, 99, &[])
            })
            .unwrap_err();
        assert!(matches!(error, StoreError::StaleEpoch));
        p.snapshot(deadline(), |read| {
            assert!(read.tail("run", 100)?.is_empty());
            assert!(read.gaps("run")?.is_empty());
            Ok(())
        })
        .unwrap();
        let lineage = fx.lineage.clone();
        p.write(None, move |unit| {
            unit.append("run", &lineage, 2, &[event(1)])
        })
        .unwrap();
        for batch in [vec![], vec![event(1)]] {
            let lineage = fx.lineage.clone();
            assert!(matches!(
                p.write(None, move |unit| unit.append("run", &lineage, 99, &batch)),
                Err(StoreError::StaleEpoch)
            ));
        }
        let lineage = fx.lineage.clone();
        let forged = EventAdapter {
            writer: w,
            readers: &pool,
            caller: task_caller("p", "wrong", "run", &lineage, 2),
        };
        assert!(matches!(
            forged.write(None, move |unit| unit.append("run", &lineage, 2, &[])),
            Err(StoreError::StaleEpoch)
        ));
    });
}

#[test]
fn mid_batch_sql_error_cannot_commit_even_when_the_callback_catches_it() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        submit(w, Lane::Entities, |tx| {
            tx.execute_batch("CREATE TRIGGER fail_second BEFORE INSERT ON run_events WHEN NEW.seq = 2 BEGIN SELECT RAISE(ABORT, 'synthetic mid-batch'); END;")?;
            Ok(())
        }).unwrap();
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let p = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
        };
        let lineage = fx.lineage.clone();
        let error = p
            .write(None, move |unit| {
                unit.gap("run", 10, 2)?;
                assert!(matches!(
                    unit.append("run", &lineage, 2, &[event(1), event(2), event(3)]),
                    Err(StoreError::Backend(_))
                ));
                assert!(unit.gap("run", 20, 2).is_err());
                // This success must not turn the partial INSERT into a commit.
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(error, StoreError::Backend(_)));
        p.snapshot(deadline(), |read| {
            assert!(read.tail("run", 100)?.is_empty());
            assert!(read.gaps("run")?.is_empty());
            Ok(())
        })
        .unwrap();
        let lineage = fx.lineage.clone();
        p.write(None, move |unit| {
            unit.append("run", &lineage, 2, &[event(3)])
        })
        .unwrap();
        // The writer remains usable after the rollback.
        p.snapshot(deadline(), |read| {
            assert_eq!(read.tail("run", 100)?, vec![event(3)]);
            Ok(())
        })
        .unwrap();
        submit(w, Lane::Entities, |tx| {
            tx.execute("UPDATE run_events SET payload='broken'", [])?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            p.snapshot(deadline(), |read| read.tail("run", 100)),
            Err(StoreError::Backend(_))
        ));
    });
}

#[test]
fn typed_writes_preserve_the_original_event_and_identity_after_reopen() {
    let (fx, data) = fixture();
    let original = SequencedEvent {
        seq: 7,
        event: RunEvent {
            timestamp: "original timestamp text +00:00".into(),
            event_type: RunEventType::AgentOutput,
            data: serde_json::json!({"id":"original-id", "run_id":"payload-is-data", "timestamp":"unchanged", "text":"é\n\\", "nested":[null,42]}),
        },
    };
    with_writer(&data, |w| {
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let adapter = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
        };
        let first = original.clone();
        let lineage = fx.lineage.clone();
        adapter
            .write(None, move |unit| {
                unit.append("run", &lineage, 2, &[first])?;
                let mut replacement = event(7);
                replacement.event.timestamp = "must not replace".into();
                assert_eq!(
                    unit.append("run", &lineage, 2, &[replacement])?,
                    AppendResult {
                        stored: 0,
                        skipped: 1
                    }
                );
                Ok(())
            })
            .unwrap();
    });
    drop(data);
    let data = acquire(&fx.path);
    with_writer(&data, |w| {
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let adapter = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
        };
        adapter
            .snapshot(deadline(), |read| {
                assert_eq!(read.events("run", 0, 100)?, vec![original]);
                Ok(())
            })
            .unwrap();
    });
}
