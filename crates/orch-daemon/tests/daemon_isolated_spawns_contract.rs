#![cfg(feature = "testing")]
//! The opt-in must fail before daemon ownership without a supplied fake manager.
use orch_daemon::{DaemonError, testing::DaemonHarness};

#[tokio::test]
async fn isolated_fake_spawns_without_a_manager_is_rejected_before_daemon_bring_up() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".orchestry")).unwrap();
    let result = DaemonHarness::builder()
        .isolated_fake_spawns()
        .on_socket()
        .environment([
            ("HOME".into(), root.path().as_os_str().to_owned()),
            ("PATH".into(), std::ffi::OsString::new()),
            ("HOSTNAME".into(), "isolated-contract".into()),
        ])
        .start_in(root.path())
        .await;
    let error = match result {
        Err(error) => error,
        Ok(daemon) => {
            daemon.shutdown().await.unwrap();
            panic!("missing-manager opt-in brought up a daemon");
        }
    };
    assert!(
        matches!(error, DaemonError::InvalidArguments(ref m) if m == "isolated fake spawns require a supplied process manager"),
        "{error}"
    );
    assert_eq!(
        std::fs::read_dir(root.path().join(".orchestry"))
            .unwrap()
            .count(),
        0,
        "rejected harness created socket, owner lock or state"
    );
}
