#![cfg(feature = "testing")]
//! Contracts retained when the local store/execution frontends are removed.

#[tokio::test]
async fn offline_init_creates_files_without_starting_a_daemon() {
    let root = tempfile::tempdir().unwrap();
    let outcome = orch_daemon::initialize(orch_daemon::InitOptions {
        project_root: root.path().to_owned(),
        adapter: Some("shell".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    assert!(!outcome.already_initialized);
    assert!(root.path().join(".orchestry/config.yml").is_file());
    for runtime in ["state.json", "daemon.json", "run/daemon.sock"] {
        assert!(
            !root.path().join(".orchestry").join(runtime).exists(),
            "{runtime}"
        );
    }
}
