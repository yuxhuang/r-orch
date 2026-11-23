//! Shared attention contract scenarios; registered on both references.
#![allow(clippy::unwrap_used, reason = "contract assertion helpers")]

use crate::v2_operations::Fixture;
use orch_core::store::{attention::*, *};

fn caller(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "owner_1".into(),
        },
    }
}
fn dispatch() -> AttentionFacts {
    AttentionFacts::DispatchOffWithWork {
        waiting_tasks: 2,
        oldest_waiting_at: 10,
    }
}

pub async fn duplicate_open_ack_and_resolve_are_revisioned(fixture: Fixture) {
    let s = fixture.storage.clone();
    s.transaction(caller("p1"), Lane::Maintenance, |u| {
        let first = u.open_attention(&dispatch(), 20)?;
        assert_eq!(first.revision, Revision::ONE);
        assert_eq!(u.open_attention(&dispatch(), 30)?, first);
        let ack = u.ack_attention(first.id, first.revision, 40)?;
        assert_eq!(ack.acknowledgement.as_ref().unwrap().identity, "owner_1");
        assert_eq!(ack.resolution, None);
        assert!(
            u.resolve_attention(
                first.id,
                first.revision,
                ResolutionReason::DispatchEnabled,
                50
            )
            .is_err()
        );
        let done = u.resolve_attention(
            first.id,
            ack.revision,
            ResolutionReason::DispatchEnabled,
            50,
        )?;
        assert!(done.resolution.is_some());
        let next = u.open_attention(&dispatch(), 60)?;
        assert!(next.id > first.id);
        Ok(())
    })
    .await
    .unwrap();
}

async fn open(s: &dyn Storage, project: &str, facts: AttentionFacts, at: i64) -> Attention {
    s.transaction(caller(project), Lane::Maintenance, move |u| {
        u.open_attention(&facts, at)
    })
    .await
    .unwrap()
}
async fn get(s: &dyn Storage, project: &str, id: u64) -> Option<Attention> {
    s.read(caller(project))
        .await
        .unwrap()
        .read_attention(id)
        .await
        .unwrap()
}
fn disk() -> AttentionFacts {
    AttentionFacts::DiskFloor {
        subject: DiskSubject::DataDirectory,
        free_bytes: 1,
        floor_bytes: 100,
    }
}

pub async fn daemon_disk_and_project_dispatch_are_separate_scopes(f: Fixture) {
    let s = f.storage;
    let global = open(s.as_ref(), "", disk(), 20).await;
    assert_eq!(get(s.as_ref(), "p1", global.id).await, None);
    let local = open(s.as_ref(), "p1", dispatch(), 20).await;
    assert_eq!(get(s.as_ref(), "p2", local.id).await, None);
    assert_eq!(get(s.as_ref(), "", global.id).await, Some(global.clone()));
    s.transaction(caller("p2"), Lane::Maintenance, move |u| {
        assert!(matches!(
            u.ack_attention(local.id, local.revision, 30),
            Err(StoreError::StaleAttentionRevision { actual: None, .. })
        ));
        assert!(u.open_attention(&disk(), 30).is_err());
        assert_eq!(u.expire_attention(100, 1000)?, 0);
        Ok(())
    })
    .await
    .unwrap();
    s.transaction(caller(""), Lane::Maintenance, |u| {
        assert!(u.open_attention(&dispatch(), 30).is_err());
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(get(s.as_ref(), "", global.id).await, Some(global));
}

pub async fn caught_stale_or_invalid_transitions_change_nothing(f: Fixture) {
    let s = f.storage;
    let row = open(s.as_ref(), "p1", dispatch(), 20).await;
    let check = row.clone();
    s.transaction(caller("p1"), Lane::Maintenance, move |u| {
        assert!(u.ack_attention(row.id, row.revision.next()?, 30).is_err());
        assert!(
            u.resolve_attention(
                row.id,
                row.revision,
                ResolutionReason::DiskSpaceRecovered,
                30
            )
            .is_err()
        );
        assert!(u.ack_attention(row.id, row.revision, 19).is_err());
        assert!(
            u.open_attention(
                &AttentionFacts::DispatchOffWithWork {
                    waiting_tasks: 0,
                    oldest_waiting_at: 10
                },
                30
            )
            .is_err()
        );
        assert_eq!(u.get_attention(row.id)?, Some(row));
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(get(s.as_ref(), "p1", check.id).await, Some(check));
}

pub async fn first_ack_and_first_resolution_keep_their_provenance(f: Fixture) {
    let s = f.storage;
    s.transaction(caller("p1"), Lane::Maintenance, |u| {
        let row = u.open_attention(&dispatch(), 20)?;
        let ack = u.ack_attention(row.id, row.revision, 30)?;
        assert_eq!(ack.revision.get(), 2);
        assert_eq!(u.ack_attention(ack.id, ack.revision, 40)?, ack);
        let done =
            u.resolve_attention(ack.id, ack.revision, ResolutionReason::DispatchEnabled, 50)?;
        assert_eq!(done.revision.get(), 3);
        assert_eq!(
            u.resolve_attention(
                done.id,
                done.revision,
                ResolutionReason::NoDispatchableWork,
                60
            )?,
            done
        );
        assert_eq!(u.ack_attention(done.id, done.revision, 60)?, done);
        assert_eq!(u.open_attention(&dispatch(), 60)?.revision, Revision::ONE);
        Ok(())
    })
    .await
    .unwrap();
}

pub async fn attention_and_entities_roll_back_together(f: Fixture) {
    let s = f.storage;
    let result = s
        .transaction(caller("p1"), Lane::Maintenance, |u| {
            let row = u.open_attention(&dispatch(), 20)?;
            u.ack_attention(row.id, row.revision, 30)?;
            u.create(Entity::Config(ConfigEntry {
                key: "synthetic".into(),
                value: "value".into(),
            }))?;
            Err::<(), _>(StoreError::InvalidAttention("synthetic rollback".into()))
        })
        .await;
    assert!(result.is_err());
    let snap = s.read(caller("p1")).await.unwrap();
    assert_eq!(
        snap.get(EntityKind::Config, "synthetic").await.unwrap(),
        None
    );
    assert_eq!(snap.read_attention(1).await.unwrap(), None);
    assert_eq!(open(s.as_ref(), "p1", dispatch(), 40).await.id, 1);
}

pub async fn snapshot_and_transaction_reads_keep_their_own_state(f: Fixture) {
    let s = f.storage;
    let row = open(s.as_ref(), "p1", dispatch(), 20).await;
    let before = s.read(caller("p1")).await.unwrap();
    let copy = row.clone();
    s.transaction(caller("p1"), Lane::Maintenance, move |u| {
        let ack = u.ack_attention(copy.id, copy.revision, 30)?;
        assert_eq!(u.get_attention(ack.id)?, Some(ack.clone()));
        assert_eq!(
            u.attention_page(AttentionFilter::Open, None, 1)?.items,
            vec![ack]
        );
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(before.read_attention(row.id).await.unwrap(), Some(row));
    assert_eq!(
        get(s.as_ref(), "p1", copy.id).await.unwrap().revision.get(),
        2
    );
}

pub async fn pages_enumerate_more_than_one_thousand_and_bind_scope_and_filter(f: Fixture) {
    let s = f.storage;
    s.transaction(caller("p1"), Lane::Maintenance, |u| {
        for at in 20..1025 {
            let row = u.open_attention(&dispatch(), at)?;
            u.resolve_attention(row.id, row.revision, ResolutionReason::DispatchEnabled, at)?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let snap = s.read(caller("p1")).await.unwrap();
    let first = snap
        .read_attention_page(AttentionFilter::All, None, 1000)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1000);
    let cursor = first.next.unwrap();
    let last = snap
        .read_attention_page(AttentionFilter::All, Some(&cursor), 1000)
        .await
        .unwrap();
    assert_eq!(last.items.len(), 5);
    assert_eq!(last.items[0].id, 1001);
    assert_eq!(last.items[4].id, 1005);
    assert!(last.next.is_none());
    assert!(
        snap.read_attention_page(AttentionFilter::Open, Some(&cursor), 1)
            .await
            .is_err()
    );
    assert!(
        s.read(caller("p2"))
            .await
            .unwrap()
            .read_attention_page(AttentionFilter::All, Some(&cursor), 1)
            .await
            .is_err()
    );
    for bad in [0, 1001] {
        assert!(
            snap.read_attention_page(AttentionFilter::All, None, bad)
                .await
                .is_err()
        );
        s.transaction(caller("p1"), Lane::Maintenance, move |u| {
            assert!(u.attention_page(AttentionFilter::All, None, bad).is_err());
            assert!(u.expire_attention(5000, bad).is_err());
            Ok(())
        })
        .await
        .unwrap();
    }
    assert_eq!(
        snap.read_attention_page(AttentionFilter::Resolved, None, 1000)
            .await
            .unwrap()
            .items
            .len(),
        1000
    );
    assert!(
        snap.read_attention_page(AttentionFilter::Open, None, 1000)
            .await
            .unwrap()
            .items
            .is_empty()
    );
}

pub async fn retention_is_bounded_strict_and_never_reuses_ids(f: Fixture) {
    let s = f.storage;
    s.transaction(caller("p1"), Lane::Maintenance, |u| {
        for _ in 0..1002 {
            let r = u.open_attention(&dispatch(), 20)?;
            u.resolve_attention(r.id, r.revision, ResolutionReason::DispatchEnabled, 30)?;
        }
        assert_eq!(u.expire_attention(30, 1000)?, 0);
        assert_eq!(u.expire_attention(31, 1000)?, 1000);
        let r = u.open_attention(&dispatch(), 40)?;
        assert_eq!(r.id, 1003);
        let ack = u.ack_attention(r.id, r.revision, 50)?;
        assert_eq!(u.expire_attention(100, 1000)?, 2);
        assert_eq!(u.expire_attention(100, 1000)?, 0);
        assert_eq!(u.get_attention(ack.id)?, Some(ack));
        assert!(u.expire_attention(-1, 1).is_err());
        Ok(())
    })
    .await
    .unwrap();
}

pub async fn fixed_facts_reject_external_text_and_wrong_scope(f: Fixture) {
    let s = f.storage;
    let bad = AttentionFacts::DiskFloor {
        subject: DiskSubject::ProjectWorkspaces {
            project: "$(touch canary); --yes\n".into(),
        },
        free_bytes: 1,
        floor_bytes: 100,
    };
    assert!(bad.remedy().is_err());
    assert!(bad.detail().is_err());
    s.transaction(caller(""), Lane::Maintenance, move |u| {
        assert!(u.open_attention(&bad, 20).is_err());
        assert!(u.open_attention(&disk(), -1).is_err());
        for facts in [
            AttentionFacts::DiskFloor {
                subject: DiskSubject::DataDirectory,
                free_bytes: 100,
                floor_bytes: 100,
            },
            AttentionFacts::DiskFloor {
                subject: DiskSubject::DataDirectory,
                free_bytes: 0,
                floor_bytes: u64::MAX,
            },
        ] {
            assert!(u.open_attention(&facts, 20).is_err());
        }
        let r = u.open_attention(&disk(), 20)?;
        assert_eq!(r.id, 1);
        assert_eq!(r.facts.severity(), Severity::Blocking);
        assert_eq!(r.facts.title(), "Free disk space is below the floor");
        assert_eq!(
            r.facts.detail()?,
            "The filesystem for data directory has 1 free bytes; the floor is 100 bytes."
        );
        assert_eq!(
            r.facts.remedy()?,
            "Free disk space on the affected filesystem."
        );
        Ok(())
    })
    .await
    .unwrap();
    let r = open(s.as_ref(), "p1", dispatch(), 20).await;
    assert_eq!(r.facts.severity(), Severity::Warning);
    assert_eq!(
        r.facts.remedy().unwrap(),
        "Enable dispatch or remove the waiting work."
    );
}

pub async fn restore_keeps_acknowledgements_resolutions_and_counters(f: Fixture) {
    let s = f.storage.clone();
    let row = open(s.as_ref(), "p1", dispatch(), 20).await;
    let expected = s
        .transaction(caller("p1"), Lane::Maintenance, move |u| {
            let ack = u.ack_attention(row.id, row.revision, 30)?;
            u.resolve_attention(
                ack.id,
                ack.revision,
                ResolutionReason::NoDispatchableWork,
                40,
            )
        })
        .await
        .unwrap();
    f.restore.restore_to_lineage("restored").await;
    assert_eq!(
        get(s.as_ref(), "p1", expected.id).await,
        Some(expected.clone())
    );
    assert!(open(s.as_ref(), "p1", dispatch(), 50).await.id > expected.id);
}

pub async fn concurrent_opens_leave_one_item(f: Fixture) {
    let a = f.storage.clone();
    let b = f.storage.clone();
    let (one, two) = tokio::join!(
        open(a.as_ref(), "p1", dispatch(), 20),
        open(b.as_ref(), "p1", dispatch(), 20)
    );
    assert_eq!(one, two);
    assert_eq!(
        a.read(caller("p1"))
            .await
            .unwrap()
            .read_attention_page(AttentionFilter::All, None, 1000)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
}

pub async fn resolution_and_expiry_roll_back_with_other_writes(f: Fixture) {
    let s = f.storage;
    let row = open(s.as_ref(), "p1", dispatch(), 20).await;
    let id = row.id;
    let result = s
        .transaction(caller("p1"), Lane::Maintenance, move |u| {
            u.resolve_attention(row.id, row.revision, ResolutionReason::DispatchEnabled, 30)?;
            assert_eq!(u.expire_attention(31, 1000)?, 1);
            assert!(u.get_attention(row.id)?.is_none());
            Err::<(), _>(StoreError::InvalidAttention("synthetic rollback".into()))
        })
        .await;
    assert!(result.is_err());
    let unchanged = get(s.as_ref(), "p1", id).await.unwrap();
    assert_eq!(unchanged.revision, Revision::ONE);
    assert!(unchanged.resolution.is_none());
}

pub async fn disk_subjects_are_distinct_and_invalid_ack_ids_change_nothing(f: Fixture) {
    let s = f.storage;
    s.transaction(caller(""), Lane::Maintenance, |u| {
        let a = u.open_attention(&disk(), 20)?;
        let facts = AttentionFacts::DiskFloor {
            subject: DiskSubject::ProjectWorkspaces {
                project: "p1".into(),
            },
            free_bytes: 5,
            floor_bytes: 10,
        };
        let b = u.open_attention(&facts, 20)?;
        assert_ne!(a.id, b.id);
        assert_eq!(u.open_attention(&facts, 30)?, b);
        let ack = u.ack_attention(a.id, a.revision, 30)?;
        assert_eq!(u.get_attention(b.id)?, Some(b));
        assert_eq!(u.get_attention(ack.id)?, Some(ack));
        Ok(())
    })
    .await
    .unwrap();
    let bad = Caller {
        project: "".into(),
        principal: Principal::Owner {
            identity: "forged\n$(canary)".into(),
        },
    };
    s.transaction(bad, Lane::Maintenance, |u| {
        let r = u.get_attention(2)?.unwrap();
        assert!(u.ack_attention(r.id, r.revision, 40).is_err());
        assert_eq!(u.get_attention(r.id)?, Some(r));
        Ok(())
    })
    .await
    .unwrap();
}

macro_rules! attention_suite {
    ($module:ident, $factory:path) => {
        mod $module {
            macro_rules! case {
                ($name:ident) => {
                    #[tokio::test]
                    async fn $name() {
                        crate::attention_v2::$name($factory().await).await;
                    }
                };
            }
            case!(duplicate_open_ack_and_resolve_are_revisioned);
            case!(daemon_disk_and_project_dispatch_are_separate_scopes);
            case!(caught_stale_or_invalid_transitions_change_nothing);
            case!(first_ack_and_first_resolution_keep_their_provenance);
            case!(attention_and_entities_roll_back_together);
            case!(snapshot_and_transaction_reads_keep_their_own_state);
            case!(pages_enumerate_more_than_one_thousand_and_bind_scope_and_filter);
            case!(retention_is_bounded_strict_and_never_reuses_ids);
            case!(fixed_facts_reject_external_text_and_wrong_scope);
            case!(restore_keeps_acknowledgements_resolutions_and_counters);
            case!(concurrent_opens_leave_one_item);
            case!(resolution_and_expiry_roll_back_with_other_writes);
            case!(disk_subjects_are_distinct_and_invalid_ack_ids_change_nothing);
        }
    };
}
pub(crate) use attention_suite;
