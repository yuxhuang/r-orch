//! The production stop entry points seal before waiting for project work.
use super::*;
use crate::lifecycle::internal_context;
use orch_proto::ops::args::DaemonStopArgs;
use std::os::unix::fs::PermissionsExt;

async fn poll_once<F: std::future::Future>(
    mut future: std::pin::Pin<&mut F>,
) -> std::task::Poll<F::Output> {
    std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx))).await
}

async fn handler(root: &std::path::Path) -> ServiceHandler {
    std::fs::create_dir(root.join(".orchestry")).unwrap();
    let engine = Arc::new(orch_engine::build_full(root).await.unwrap());
    engine.daemon_initialize().await.unwrap();
    let coordinator = Arc::new(ProjectCoordinator::new(engine.operation_boundary()));
    ServiceHandler::new(engine, coordinator)
}

#[tokio::test]
async fn pending_stop_seals_diagnostics_before_waiting_and_stays_sealed_when_dropped() {
    let root = tempfile::tempdir().unwrap();
    let h = handler(root.path()).await;
    let marker = root.path().join("started");
    let executable = root.path().join("probe");
    std::fs::write(
        &executable,
        format!("#!/bin/sh\nprintf started > '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let boundary = h.engine.operation_boundary();
    let holder = boundary.run(false, std::future::pending::<()>());
    tokio::pin!(holder);
    assert!(poll_once(holder.as_mut()).await.is_pending());
    let ctx = internal_context();
    {
        // Call the actual handler, including its synchronous prologue. The
        // project boundary is owned by a different operation.
        let stop = h.daemon_stop(&ctx, DaemonStopArgs {});
        tokio::pin!(stop);
        assert!(poll_once(stop.as_mut()).await.is_pending());
        assert!(
            poll_once(Box::pin(h.diagnostics.spawn_work().cancelled()).as_mut())
                .await
                .is_ready()
        );
        let output = h
            .diagnostics
            .output(
                executable.to_str().unwrap(),
                &[],
                root.path(),
                std::time::Duration::from_secs(1),
                128,
            )
            .await;
        assert!(output.is_err());
        assert!(!marker.exists());
    }
    assert!(
        poll_once(Box::pin(h.diagnostics.spawn_work().cancelled()).as_mut())
            .await
            .is_ready()
    );
    assert!(!marker.exists());
    assert!(h.diagnostics.spawn_work().is_idle());
}

#[tokio::test]
async fn seal_requests_seals_diagnostics_synchronously() {
    let root = tempfile::tempdir().unwrap();
    let h = handler(root.path()).await;
    let boundary = h.engine.operation_boundary();
    let holder = boundary.run(false, std::future::pending::<()>());
    tokio::pin!(holder);
    assert!(poll_once(holder.as_mut()).await.is_pending());
    h.seal_requests();
    assert!(
        poll_once(Box::pin(h.diagnostics.spawn_work().cancelled()).as_mut())
            .await
            .is_ready()
    );
    assert!(
        h.diagnostics
            .output(
                "/bin/true",
                &[],
                root.path(),
                std::time::Duration::from_secs(1),
                128
            )
            .await
            .is_err()
    );
    assert!(h.diagnostics.spawn_work().is_idle());
}
