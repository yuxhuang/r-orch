use orch_daemon::coordinator::{Lane, ProjectCoordinator};
use orch_proto::frame::Store;

#[tokio::test]
async fn mutations_and_reads_observe_one_commit_order() {
    let coordinator = ProjectCoordinator::default();
    let mut invalidations = coordinator.subscribe_invalidations();
    let write = coordinator
        .run_op(Lane::Ordinary, |permit| {
            Box::pin(async move {
                permit.invalidate(Store::Tasks);
                Ok(42)
            })
        })
        .await
        .unwrap();
    assert_eq!(write.commit_seq, 1);
    assert_eq!(invalidations.recv().await.unwrap(), 1);
    let read = coordinator
        .run_op(Lane::Ordinary, |_| Box::pin(async { Ok(7) }))
        .await
        .unwrap();
    assert_eq!(read.commit_seq, 1);
}

#[tokio::test]
async fn failed_partial_write_still_invalidates() {
    let coordinator = ProjectCoordinator::default();
    let mut invalidations = coordinator.subscribe_invalidations();
    let result = coordinator
        .run_op::<(), _>(Lane::Ordinary, |permit| {
            Box::pin(async move {
                permit.invalidate(Store::Goals);
                Err(orch_proto::error::ErrorBody::unimplemented(
                    "injected failure after write",
                ))
            })
        })
        .await;
    assert!(result.is_err());
    assert_eq!(invalidations.recv().await.unwrap(), 1);
    assert_eq!(coordinator.current_commit_seq(), 1);
}

#[tokio::test]
async fn saturation_retains_control_and_internal_capacity() {
    use orch_proto::frame::limits;
    let coordinator = ProjectCoordinator::default();
    let ordinary = (0..limits::MAX_QUEUED_OPERATIONS)
        .map(|_| coordinator.admit(Lane::Ordinary, 1).unwrap())
        .collect::<Vec<_>>();
    assert!(coordinator.admit(Lane::Ordinary, 1).is_err());
    assert!(coordinator.admit(Lane::Control, 1).is_ok());
    assert!(coordinator.admit(Lane::Internal, 1).is_ok());
    let result = coordinator
        .run_op(Lane::Control, |permit| {
            Box::pin(async move {
                permit.invalidate(Store::State);
                Ok(())
            })
        })
        .await
        .unwrap();
    assert_eq!(result.commit_seq, 1);
    drop(ordinary);
    assert!(
        coordinator
            .admit(Lane::Ordinary, limits::MAX_QUEUED_REQUEST_BYTES + 1)
            .is_err()
    );
    assert!(
        coordinator.admit(Lane::Ordinary, 0).is_ok(),
        "failed byte admission releases its slot"
    );
    coordinator.stop_admission();
    assert!(coordinator.admit(Lane::Ordinary, 0).is_err());
    assert!(coordinator.admit(Lane::Control, 0).is_ok());
}
