use super::super::adapter::EventAdapter;
use super::*;
use std::sync::Arc;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn typed_adapter_binds_all_operations_to_caller_and_keeps_one_snapshot() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let p = EventAdapter {
            blobs: &blobs,
            writer: w,
            readers: &pool,
            caller: owner("p"),
        };
        let other = EventAdapter {
            blobs: &blobs,
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
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let p = EventAdapter {
            blobs: &blobs,
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
            blobs: &blobs,
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
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        submit(w, Lane::Entities, |tx| {
            tx.execute_batch("CREATE TRIGGER fail_second BEFORE INSERT ON run_events WHEN NEW.seq = 2 BEGIN SELECT RAISE(ABORT, 'synthetic mid-batch'); END;")?;
            Ok(())
        }).unwrap();
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let p = EventAdapter {
            blobs: &blobs,
            writer: w,
            readers: &pool,
            caller: owner("p"),
        };
        let lineage = fx.lineage.clone();
        let error = p
            .write(None, move |unit| {
                unit.gap("run", 10, 2)?;
                assert!(matches!(
                    unit.append(
                        "run",
                        &lineage,
                        2,
                        &[sized_event(1, 65_537), sized_event(2, 65_538), event(3)]
                    ),
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
        snapshot(&pool, |tx| {
            assert_eq!(
                tx.query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                0
            );
        });
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
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let adapter = EventAdapter {
            blobs: &blobs,
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
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = ReadPool::open(&data, Synchronous::Normal, 1).unwrap();
        let adapter = EventAdapter {
            blobs: &blobs,
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

fn sized_event(seq: u64, bytes: usize) -> SequencedEvent {
    let mut row = event(seq);
    row.event.data = serde_json::json!({"text":"", "marker":[BLOB_TAG, "ordinary data"]});
    let overhead = serde_json::to_vec(&row.event).unwrap().len();
    let count = bytes - overhead;
    row.event.data["text"] = serde_json::Value::String(format!(
        "{}{}",
        "é".repeat(count / 2),
        "a".repeat(count % 2)
    ));
    assert_eq!(serde_json::to_vec(&row.event).unwrap().len(), bytes);
    row
}

#[test]
fn serialized_byte_boundary_places_only_large_events_in_blobs_and_reopens() {
    let (fx, data) = fixture();
    let expected = vec![sized_event(1, 65_536), sized_event(2, 65_537)];
    {
        let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
        with_writer(&data, |w| {
            let pool = pool(&data);
            let adapter = EventAdapter {
                writer: w,
                readers: &pool,
                caller: owner("p"),
                blobs: &blobs,
            };
            let batch = expected.clone();
            let lineage = fx.lineage.clone();
            adapter
                .write(None, move |unit| {
                    assert_eq!(unit.append("run", &lineage, 2, &batch)?.stored, 2);
                    assert_eq!(
                        unit.append("run", &lineage, 2, &[sized_event(2, 65_539)])?
                            .skipped,
                        1
                    );
                    Ok(())
                })
                .unwrap();
            snapshot(&pool, |tx| {
                let (inline, reference): (String, String) = tx.query_row("SELECT a.payload,b.payload FROM run_events a JOIN run_events b ON a.project_id=b.project_id AND a.run_id=b.run_id WHERE a.seq=1 AND b.seq=2", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
                assert_eq!(inline.len(), 65_536);
                assert!(
                    serde_json::from_str::<serde_json::Value>(&inline)
                        .unwrap()
                        .is_object()
                );
                let marker: Vec<String> = serde_json::from_str(&reference).unwrap();
                assert_eq!(marker[0], BLOB_TAG);
                let hash = marker[1].parse::<BlobHash>().unwrap();
                assert_eq!(
                    blobs.get(&hash).unwrap(),
                    serde_json::to_vec(&expected[1].event).unwrap()
                );
                let (size, location, count): (i64, String, i64) = tx.query_row("SELECT size,storage_location,(SELECT count(*) FROM blobs) FROM blobs WHERE project_id='p'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
                assert_eq!((size, location.as_str(), count), (65_537, "disk", 1));
            });
            adapter
                .snapshot(deadline(), |read| {
                    assert_eq!(read.tail("run", 2)?, expected);
                    Ok(())
                })
                .unwrap();
        });
    }
    drop(data);
    let data = acquire(&fx.path);
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let adapter = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
            blobs: &blobs,
        };
        adapter
            .snapshot(deadline(), |read| {
                assert_eq!(read.events("run", 0, 10)?, expected);
                Ok(())
            })
            .unwrap();
        // A second run references the same hash. Pruning one run must not remove shared bytes.
        let lineage = fx.lineage.clone();
        let same = expected[1].clone();
        adapter
            .write(None, move |unit| {
                unit.append("another", &lineage, 3, &[same])
            })
            .unwrap();
        assert!(adapter.prune("run".into(), 100).unwrap().complete);
        adapter
            .snapshot(deadline(), |read| {
                assert_eq!(read.tail("another", 1)?, vec![expected[1].clone()]);
                Ok(())
            })
            .unwrap();
    });
}

#[test]
fn legacy_oversized_objects_stay_readable_and_blob_failures_are_never_skipped() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    let original = sized_event(1, 65_537);
    with_writer(&data, |w| {
        let pool = pool(&data);
        let adapter = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
            blobs: &blobs,
        };
        let payload = serde_json::to_string(&original.event).unwrap();
        submit(w, Lane::Entities, move |tx| {
            tx.execute(
                "INSERT INTO run_events VALUES ('p','run',1,0,'agent_output',?1)",
                [payload],
            )?;
            Ok(())
        })
        .unwrap();
        adapter
            .snapshot(deadline(), |read| {
                assert_eq!(read.tail("run", 1)?, vec![original]);
                Ok(())
            })
            .unwrap();
        for bad in [
            "[]".to_owned(),
            format!("[\"{BLOB_TAG}\",\"../bad\"]"),
            "[\"unknown\",\"hash\"]".into(),
            format!("[\"{BLOB_TAG}\",\"{}\"]", "0".repeat(64)),
            format!("[\"{BLOB_TAG}\",\"{}\",\"extra\"]", "0".repeat(64)),
        ] {
            submit(w, Lane::Entities, move |tx| {
                tx.execute("UPDATE run_events SET payload=?1", [bad])?;
                Ok(())
            })
            .unwrap();
            assert!(matches!(
                adapter.snapshot(deadline(), |read| read.tail("run", 1)),
                Err(StoreError::Backend(_))
            ));
        }
        // Restore via the real typed write, then make metadata visible only in another project.
        submit(w, Lane::Entities, |tx| {
            tx.execute("DELETE FROM run_events", [])?;
            Ok(())
        })
        .unwrap();
        let lineage = fx.lineage.clone();
        adapter
            .write(None, move |unit| {
                unit.append("run", &lineage, 2, &[sized_event(1, 65_537)])
            })
            .unwrap();
        submit(w, Lane::Entities, |tx| {
            tx.execute("UPDATE blobs SET project_id='other'", [])?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            adapter.snapshot(deadline(), |read| read.tail("run", 1)),
            Err(StoreError::Backend(_))
        ));
        submit(w, Lane::Entities, |tx| {
            tx.execute("UPDATE blobs SET project_id='p',size=size+1", [])?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            adapter.snapshot(deadline(), |read| read.tail("run", 1)),
            Err(StoreError::Backend(_))
        ));
        submit(w, Lane::Entities, |tx| {
            tx.execute(
                "UPDATE blobs SET size=size-1,storage_location='unknown'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            adapter.snapshot(deadline(), |read| read.tail("run", 1)),
            Err(StoreError::Backend(_))
        ));
    });
}

#[test]
fn signed_endpoint_and_successor_are_explicit_and_crossing_batches_are_atomic() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let adapter = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
            blobs: &blobs,
        };
        let top = i64::MAX as u64;
        let lineage = fx.lineage.clone();
        adapter
            .write(None, move |unit| {
                unit.append("run", &lineage, 2, &[event(top)])?;
                unit.gap("run", top - 1, 1)
            })
            .unwrap();
        let lineage = fx.lineage.clone();
        assert!(matches!(
            adapter.write(None, move |unit| unit.append(
                "run",
                &lineage,
                2,
                &[event(1), event(top + 1)]
            )),
            Err(StoreError::InvalidEvents(_))
        ));
        assert!(matches!(
            adapter.write(None, move |unit| unit.gap("run", top, 1)),
            Err(StoreError::InvalidEvents(_))
        ));
        adapter
            .snapshot(deadline(), |read| {
                assert_eq!(read.events("run", top - 1, 100)?, vec![event(top)]);
                assert!(read.events("run", top, 100)?.is_empty());
                assert!(read.events("run", top + 1, 100)?.is_empty());
                assert_eq!(read.events("run", 0, 100)?, vec![event(top)]);
                assert_eq!(
                    read.gaps("run")?,
                    vec![EventGap {
                        after_seq: top - 1,
                        dropped: 1
                    }]
                );
                Ok(())
            })
            .unwrap();
    });
}

#[test]
fn blob_read_verifies_bytes_event_type_and_resolves_only_one_reference() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let adapter = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
            blobs: &blobs,
        };
        let row = sized_event(1, 65_537);
        let original_bytes = serde_json::to_vec(&row.event).unwrap();
        let hash = BlobHash::of(&original_bytes).to_string();
        let lineage = fx.lineage.clone();
        adapter
            .write(None, move |unit| unit.append("run", &lineage, 2, &[row]))
            .unwrap();
        submit(w, Lane::Entities, |tx| {
            tx.execute("UPDATE run_events SET type='agent_error'", [])?;
            Ok(())
        })
        .unwrap();
        assert!(
            adapter
                .snapshot(deadline(), |read| read.tail("run", 1))
                .is_err()
        );
        submit(w, Lane::Entities, |tx| {
            tx.execute("UPDATE run_events SET type='agent_output'", [])?;
            Ok(())
        })
        .unwrap();
        let path = fx
            .path
            .join("blobs/sha256")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        let moved = path.with_extension("synthetic-moved");
        std::fs::rename(&path, &moved).unwrap();
        assert!(
            adapter
                .snapshot(deadline(), |read| read.tail("run", 1))
                .is_err()
        );
        std::fs::write(&path, b"damaged bytes").unwrap();
        assert!(
            adapter
                .snapshot(deadline(), |read| read.tail("run", 1))
                .is_err()
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&moved, &path).unwrap();
        adapter
            .snapshot(deadline(), |read| {
                assert_eq!(read.tail("run", 1)?.len(), 1);
                Ok(())
            })
            .unwrap();
        // A valid hash may contain invalid UTF-8 or another reference. Neither is an event.
        for bytes in [vec![0xff], serde_json::to_vec(&[BLOB_TAG, &hash]).unwrap()] {
            let reference = blobs.put(&bytes).unwrap().to_string();
            let payload = serde_json::to_string(&[BLOB_TAG, &reference]).unwrap();
            submit(w, Lane::Entities, move |tx| {
                tx.execute("INSERT INTO blobs(project_id,sha256,size,storage_location,created_at) VALUES('p',?1,?2,'disk',0)", params![reference, bytes.len() as i64])?;
                tx.execute("UPDATE run_events SET payload=?1", [payload])?;
                Ok(())
            }).unwrap();
            assert!(
                adapter
                    .snapshot(deadline(), |read| read.tail("run", 1))
                    .is_err()
            );
        }
    });
}

#[test]
fn disk_blob_size_refusal_rolls_back_prior_events_metadata_and_gaps() {
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    with_writer(&data, |w| {
        let pool = pool(&data);
        let adapter = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
            blobs: &blobs,
        };
        let lineage = fx.lineage.clone();
        let error = adapter
            .write(None, move |unit| {
                unit.append("run", &lineage, 2, &[sized_event(1, 65_537)])?;
                unit.gap("run", 10, 2)?;
                assert!(
                    unit.append(
                        "run",
                        &lineage,
                        2,
                        &[sized_event(2, crate::blob::MAX_BLOB_BYTES as usize + 1)]
                    )
                    .is_err()
                );
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(error, StoreError::Backend(_)));
        adapter
            .snapshot(deadline(), |read| {
                assert!(read.tail("run", 100)?.is_empty());
                assert!(read.gaps("run")?.is_empty());
                Ok(())
            })
            .unwrap();
        snapshot(&pool, |tx| {
            assert_eq!(
                tx.query_row("SELECT count(*) FROM blobs", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                0
            );
        });
    });
}

#[test]
fn caught_event_whole_transaction_rollback_refuses_later_work_and_reopens_empty() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (fx, data) = fixture();
    let blobs = Arc::new(DiskBlobStore::open(data.root()).unwrap());
    let large = sized_event(1, 65_537);
    let bytes = serde_json::to_vec(&large.event).unwrap();
    let hash = BlobHash::of(&bytes);
    let reached = Arc::new(AtomicBool::new(false));
    let later = Arc::clone(&reached);
    with_writer(&data, |w| {
        submit(w, Lane::Entities, |tx| {
            tx.execute_batch("CREATE TRIGGER end_second BEFORE INSERT ON run_events WHEN NEW.seq = 2 BEGIN SELECT RAISE(ROLLBACK,'event whole rollback'); END;")?;
            Ok(())
        }).unwrap();
        let pool = pool(&data);
        let adapter = EventAdapter {
            writer: w,
            readers: &pool,
            caller: owner("p"),
            blobs: &blobs,
        };
        let lineage = fx.lineage.clone();
        let result = adapter.write(None, move |unit| {
            unit.gap("run", 10, 2)?;
            assert!(matches!(
                unit.append("run", &lineage, 2, &[large, event(2)]),
                Err(StoreError::Backend(_))
            ));
            later.store(true, Ordering::SeqCst);
            // Even a read-only empty append is refused by the poisoned batch.
            assert!(unit.append("another", &lineage, 3, &[]).is_err());
            assert!(unit.append("another", &lineage, 3, &[event(9)]).is_err());
            assert!(unit.gap("another", 20, 2).is_err());
            Ok(())
        });
        assert!(matches!(result, Err(StoreError::Backend(_))));
    });
    assert!(reached.load(Ordering::SeqCst));
    drop(data);
    let reopened = acquire(&fx.path);
    let pool = pool(&reopened);
    snapshot(&pool, |tx| {
        let state = ["run_events", "run_event_gaps", "blobs"].map(|table| {
            tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        });
        assert_eq!(state, [0, 0, 0]);
    });
    // F7 is a separate filesystem outcome: publication precedes the SQL row.
    // No sweep/retention acceptance is implied by this orphan characterization.
    let reopened_blobs = DiskBlobStore::open(reopened.root()).unwrap();
    assert_eq!(
        reopened_blobs
            .get(&hash)
            .expect("F7: unswept publication remains until the T15 orphan sweep"),
        bytes
    );
}

#[test]
fn caught_raw_event_rollback_blocks_an_attempted_later_entity_write() {
    let (fx, data) = fixture();
    with_writer(&data, |w| {
        submit(w, Lane::Entities, |tx| {
            tx.execute_batch("CREATE TRIGGER end_second BEFORE INSERT ON run_events WHEN NEW.seq = 2 BEGIN SELECT RAISE(ROLLBACK,'raw event rollback'); END;")?;
            Ok(())
        }).unwrap();
        let lineage = fx.lineage.clone();
        let receipt = submit(w, Lane::Events, move |tx| {
            record_event_gap(tx, "p", "run", 10, 2).unwrap();
            let error = append_events(tx, &owner("p"), "run", &lineage, 2, &[event(1), event(2)]);
            assert!(
                matches!(error, Err(EventsError::Sql(ref e)) if e.sqlite_error_code() == Some(rusqlite::ffi::ErrorCode::ConstraintViolation))
            );
            assert!(tx.is_autocommit(), "RAISE(ABORT) is not this failure mode");
            // Catch the original failure and actually issue a new SQL write.
            // Returning its error would also test only an early-return path.
            let _caught = tx.execute(
                "INSERT INTO daemon_config(key,value,revision) VALUES('later','x',1)",
                [],
            );
            Ok(())
        });
        assert!(
            matches!(receipt, Err(SqliteError::TransactionAborted)),
            "{receipt:?}"
        );
    });
    drop(data);
    let reopened = acquire(&fx.path);
    snapshot(&pool(&reopened), |tx| {
        let state = (
            tx.query_row("SELECT count(*) FROM run_events", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            tx.query_row("SELECT count(*) FROM run_event_gaps", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
            tx.query_row(
                "SELECT count(*) FROM daemon_config WHERE key='later'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap(),
        );
        assert_eq!(state, (0, 0, 0));
    });
}
