//! Reads after a latch that was set before them (issue 389; spec R128, R140).
//!
//! The policy under test: a latched state refuses a callback on the writer
//! connection and serves a pooled snapshot read, which fails only for a fatal
//! observation made during that read. The latch is set from the checkpoint
//! source, as in the incident of the issue: no connection of the reads below saw
//! damage, so nothing but the gate decides whether they are served.
use super::tests::{owner, payload, scenario, submit};
use super::*;
use crate::sqlite::{FatalCause, ReadPool, Synchronous};
use orch_core::store::attention::{AttentionFacts, AttentionFilter};
use orch_core::store::leases::{ClaimOutcome, NewLease};
use std::time::{Duration, Instant};

fn corrupt() -> SqliteError {
    SqliteError::Sql(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        None,
    ))
}

fn is_unavailable<T: std::fmt::Debug>(result: &Result<T>) -> bool {
    matches!(result, Err(SqliteError::StorageUnavailable(_)))
}

/// One agent, one lease and one attention item, so that every read below has a
/// value to return and an absent row cannot pass for a served read.
fn seed(handle: &crate::sqlite::WriterHandle, disk: &std::sync::Arc<DiskBlobStore>) {
    let state = handle.failure_state();
    let blobs = disk.clone();
    submit(handle, move |tx| {
        let caller = owner("p");
        let mut unit = EntityWrite::new(tx, &caller, &blobs, state, 0);
        unit.create(payload(Family::Agent, "a1", "agent"))?;
        let ClaimOutcome::Granted(_) = unit.claim_lease(
            &NewLease {
                task: "t".into(),
                agent: "a1".into(),
                run: "r".into(),
            },
            0,
        )?
        else {
            panic!("grant")
        };
        unit.open_attention(
            &AttentionFacts::DispatchOffWithWork {
                waiting_tasks: 1,
                oldest_waiting_at: 1,
            },
            1,
        )?;
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_prior_latch_still_serves_pooled_entity_and_operational_reads() {
    scenario(|data, handle, disk| {
        seed(handle, disk);
        handle
            .failure_state()
            .latch(FailureSource::Checkpoint, FatalCause::SqliteCorrupt);
        assert!(handle.failure_state().failure().is_some());
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            assert!(read.get(Family::Agent, "a1")?.is_some());
            assert!(read.get_lease("t")?.is_some());
            assert!(read.get_agent_lease("a1")?.is_some());
            assert_eq!(read.list_leases()?.len(), 1);
            assert_eq!(
                read.attention_page(AttentionFilter::Open, None, 10)?
                    .items
                    .len(),
                1
            );
            Ok(())
        })
        .expect("a read that saw no fresh fatal observation is served");
        assert_eq!(
            handle.failure_state().failure().unwrap().source,
            FailureSource::Checkpoint,
            "serving a read neither changes nor clears the latch"
        );
    });
}

#[test]
fn a_fresh_fatal_observation_during_a_read_after_a_prior_latch_discards_it() {
    scenario(|data, handle, disk| {
        seed(handle, disk);
        handle
            .failure_state()
            .latch(FailureSource::Checkpoint, FatalCause::SqliteCorrupt);
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        let result = pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            assert!(read.get(Family::Agent, "a1")?.is_some());
            // The read's own connection now sees damage, though the state is latched.
            read.port::<()>(Err(corrupt()))
                .expect_err("the fatal result is the read's answer");
            Ok(read.list_leases()?.len())
        });
        assert!(
            is_unavailable(&result),
            "the value of a read that straddled a fresh fatal observation is not returned: {result:?}"
        );
    });
}

#[test]
fn a_prior_latch_refuses_every_operation_of_a_writer_callback_and_its_commit() {
    scenario(|data, handle, disk| {
        seed(handle, disk);
        let state = handle.failure_state();
        let blobs = disk.clone();
        let result = submit(handle, move |tx| {
            let caller = owner("p");
            let mut unit = EntityWrite::new(tx, &caller, &blobs, state.clone(), 0);
            unit.create(payload(Family::Agent, "must-rollback", "agent"))?;
            // Another connection's fatal result reaches the shared state mid-callback.
            state.latch(FailureSource::Checkpoint, FatalCause::SqliteCorrupt);
            assert!(is_unavailable(&unit.get(Family::Agent, "a1")));
            assert!(is_unavailable(&unit.list_leases()));
            assert!(is_unavailable(&unit.get_lease("t")));
            assert!(is_unavailable(&unit.create(payload(
                Family::Agent,
                "refused",
                "agent"
            ))));
            assert!(is_unavailable(&unit.ensure_committable()));
            // Swallowing every refusal must not let the callback commit.
            Ok(())
        });
        assert!(is_unavailable(&result), "{result:?}");
        let pool = ReadPool::open_with_state(data, Synchronous::Normal, 1, handle.failure_state())
            .unwrap();
        pool.read(Instant::now() + Duration::from_secs(10), |tx| {
            let caller = owner("p");
            let read = EntityRead::reader(tx, &caller, disk, pool.failure_state().clone());
            assert!(read.get(Family::Agent, "a1")?.is_some());
            assert!(read.get(Family::Agent, "must-rollback")?.is_none());
            assert!(read.get(Family::Agent, "refused")?.is_none());
            Ok(())
        })
        .unwrap();
    });
}
