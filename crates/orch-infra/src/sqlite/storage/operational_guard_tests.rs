//! Operational refusal after an actual cctg savepoint cleanup failure (#453).
use super::*;
use orch_core::store::{ConfigEntry, UnitOfWork, attention::AttentionWrite};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};

#[tokio::test]
async fn caught_cctg_cleanup_failure_refuses_operational_read_on_active_unit() {
    let (_temp, data) = crate::sqlite::test_data();
    let data = Arc::new(data);
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    let mut conn = open_connection(&data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    conn.for_migration()
        .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
            AuthAction::Savepoint {
                savepoint_name: "cctg_entity",
                operation: TransactionOperation::Release | TransactionOperation::Rollback,
            } => Authorization::Deny,
            _ => Authorization::Allow,
        }))
        .unwrap();
    let tx = conn.transaction().unwrap();
    let caller = owner();
    let mut unit = Unit::new(&tx, &caller, &storage.owner.shared);
    // An absent attention item is a successful read before poisoning.
    assert!(unit.get_attention(1).unwrap().is_none());
    // Direct-Unit characterization only: contain observes the typed leaf
    // unwind before this test catches it to inspect refusal on the same Unit.
    // Production Storage::write lets it escape and retires the writer.
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::sqlite::writer::contain::contain(&storage.failure_state(), || {
            let _ = unit.create(Entity::Config(ConfigEntry {
                key: "must-rollback".into(),
                value: "synthetic".into(),
            }));
            Ok(())
        })
    }))
    .unwrap_err();
    assert!(unwind.is::<crate::sqlite::writer::contain::CleanupUnwind>());
    assert!(!tx.is_autocommit(), "autocommit must not explain refusal");
    assert!(storage.failure_state().transaction_must_abort().is_some());
    assert!(storage.failure_state().failure().is_none());
    let refusal = unit.get_attention(1);
    assert!(
        matches!(refusal, Err(StoreError::Backend(_))),
        "operational read must refuse the cctg-poisoned unit: {refusal:?}"
    );
    drop(unit);
    tx.rollback().unwrap();
    drop(conn);
    storage.close().await.unwrap();
}
