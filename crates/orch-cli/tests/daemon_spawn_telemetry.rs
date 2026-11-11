//! The real binary's parsed start aliases and detached re-exec keep the
//! existing lifecycle/sink; parked-stage default delivery is checked by the
//! fresh-process production initializer tests in lib.rs.
mod daemon_support;
use daemon_support::*;
use std::os::unix::fs::MetadataExt;

#[test]
fn default_owner_routes_and_detached_private_sink() {
    for args in [
        vec!["daemon", "start"],
        vec!["serve"],
        vec!["run", "--watch"],
    ] {
        let sandbox = Sandbox::new();
        sandbox.init_project();
        let mut command = sandbox.orch();
        command
            .args(&args)
            .env_remove("RUST_LOG")
            .env_remove("ORCHESTRY_DEBUG");
        let mut child = DaemonChild::spawn(command, sandbox.short_dir().join("owner.err"));
        child.expect_ready(&sandbox.socket());
        assert!(sandbox.run(&["daemon", "status"]).status.success());
        assert!(sandbox.run(&["daemon", "stop"]).status.success());
        assert!(child.wait_exit(CALL).success());
        assert!(
            !child.stderr().contains("spawn_id="),
            "short successful spawns must clear observations"
        );
    }
    let sandbox = Sandbox::new();
    sandbox.init_project();
    let mut command = sandbox.orch();
    command
        .args(["daemon", "start", "--detach"])
        .env_remove("RUST_LOG")
        .env_remove("ORCHESTRY_DEBUG");
    let output = output_with_deadline(command, READY);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let status = sandbox.status().unwrap();
    let owned = KillOnDrop(status.pid);
    assert!(sandbox.run(&["daemon", "stop"]).status.success());
    // Status proves this is our just-started daemon, and stop is the normal
    // owner lifecycle. The guard covers assertion failures.
    std::mem::forget(owned);
    let log = sandbox.state_dir().join("daemon.log");
    let metadata = std::fs::metadata(&log).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert!(metadata.len() <= 10 * 1024 * 1024);
    assert!(!text(&output.stdout).contains("spawn_id="));
}
