//! Proof E is an EOF regression, separate from deterministic exclusion (S).
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use crate::models::{DiscoveryRuntime, SnapshotDiscoveryRuntime};
use crate::process::{EnvSnapshot, SnapshotEnvProcessManager, SystemProcessManager};
use orch_core::process::{ProcessManager, Signal};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_capture_finishes_while_discovery_child_is_still_alive() {
    let root = tempfile::tempdir().unwrap();
    let holder = root.path().join("holder");
    let victim = root.path().join("victim");
    std::fs::write(
        &holder,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$HOME/holder.pid\"\nexec /bin/sleep 120\n",
    )
    .unwrap();
    std::fs::write(
        &victim,
        "#!/bin/sh\nprintf victim-output\nprintf victim-error >&2\n",
    )
    .unwrap();
    for script in [&holder, &victim] {
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let snapshot = EnvSnapshot {
        base: vec![
            ("HOME".into(), root.path().as_os_str().to_owned()),
            ("PATH".into(), root.path().as_os_str().to_owned()),
        ],
        forced: vec![],
    };
    let runtime = SnapshotDiscoveryRuntime::new(snapshot.clone());
    let long_lived = tokio::spawn(async move { runtime.run_command("holder", &[]).await });
    // A dropped diagnostic future kills the child's entire group on any failure.
    struct Abort(tokio::task::JoinHandle<orch_core::Result<crate::models::CommandOutput>>);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let long_lived = Abort(long_lived);
    let pidfile = root.path().join("holder.pid");
    let pid = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(&pidfile).await
                && let Ok(pid) = text.parse::<u32>()
            {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("holder started");
    let manager = SnapshotEnvProcessManager::new(snapshot);
    // Use the existing diagnostic capture budget, without adding a workspace
    // timeout to production. The holder cannot expire before this assertion.
    let captured = tokio::time::timeout(
        Duration::from_secs(15),
        crate::workspace::run(&manager, root.path(), "victim", &[]),
    )
    .await;
    let holder_alive = SystemProcessManager.is_alive(pid);
    long_lived.0.abort();
    SystemProcessManager.kill(pid, Signal::Kill);
    let captured = captured
        .expect("workspace output EOF within the diagnostic budget")
        .unwrap();
    assert_eq!(captured.code, 0);
    assert_eq!(captured.stdout, "victim-output");
    assert!(captured.combined.contains("victim-error"));
    assert!(
        holder_alive,
        "holder must still be alive when victim EOF arrives"
    );
}
