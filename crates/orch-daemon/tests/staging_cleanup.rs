#![cfg(feature = "testing")]
//! Startup scratch cleanup must never traverse a substituted directory.
use orch_daemon::coordinator::ProjectCoordinator;
use orch_daemon::ops::ServiceHandler;
use orch_engine::container::{EngineSeams, build_full_with};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::Path;
use std::sync::Arc;

async fn handler(root: &Path) -> ServiceHandler {
    let engine = Arc::new(
        build_full_with(
            root,
            EngineSeams {
                global_config_path: Some(root.join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );
    let coordinator = Arc::new(ProjectCoordinator::new(engine.events.operation_boundary()));
    ServiceHandler::new(engine, coordinator)
}

fn private_dir(path: &Path) {
    std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

#[tokio::test]
async fn cleanup_rejects_symlinked_staging_directories_without_touching_targets() {
    for name in ["transfers", "staged"] {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join(".orchestry");
        std::fs::create_dir(&state).unwrap();
        let run = state.join("run");
        private_dir(&run);
        let outside = temp.path().join("outside");
        private_dir(&outside);
        for file in ["keep.part", "keep.png", "keep.txt"] {
            std::fs::write(outside.join(file), b"sentinel").unwrap();
        }
        symlink(&outside, run.join(name)).unwrap();
        let h = handler(temp.path()).await;
        let result = h.cleanup_stale_transfers();
        for file in ["keep.part", "keep.png", "keep.txt"] {
            assert_eq!(
                std::fs::read(outside.join(file)).unwrap(),
                b"sentinel",
                "{name}/{file}"
            );
        }
        assert!(result.is_err(), "unsafe staging directory must be rejected");
    }
}

#[tokio::test]
async fn cleanup_rejects_a_symlinked_run_directory() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join(".orchestry");
    std::fs::create_dir(&state).unwrap();
    let outside = temp.path().join("outside");
    private_dir(&outside);
    for (dir, file) in [("transfers", "keep.part"), ("staged", "keep.png")] {
        private_dir(&outside.join(dir));
        std::fs::write(outside.join(dir).join(file), b"sentinel").unwrap();
    }
    symlink(&outside, state.join("run")).unwrap();
    let h = handler(temp.path()).await;
    let result = h.cleanup_stale_transfers();
    assert_eq!(
        std::fs::read(outside.join("transfers/keep.part")).unwrap(),
        b"sentinel"
    );
    assert_eq!(
        std::fs::read(outside.join("staged/keep.png")).unwrap(),
        b"sentinel"
    );
    assert!(result.is_err());
}

#[tokio::test]
async fn cleanup_removes_only_owned_private_scratch_files() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join(".orchestry");
    std::fs::create_dir(&state).unwrap();
    let h = handler(temp.path()).await;
    h.cleanup_stale_transfers().unwrap(); // Missing scratch is harmless.
    let run = state.join("run");
    private_dir(&run);
    for (dir, stale, keep) in [
        ("transfers", "old.part", "keep.png"),
        ("staged", "old.png", "keep.part"),
    ] {
        let dir = run.join(dir);
        private_dir(&dir);
        std::fs::write(dir.join(stale), b"stale").unwrap();
        std::fs::write(dir.join(keep), b"keep").unwrap();
        std::fs::write(dir.join("keep.txt"), b"keep").unwrap();
    }
    h.cleanup_stale_transfers().unwrap();
    for (dir, stale, keep) in [
        ("transfers", "old.part", "keep.png"),
        ("staged", "old.png", "keep.part"),
    ] {
        assert!(!run.join(dir).join(stale).exists());
        assert_eq!(std::fs::read(run.join(dir).join(keep)).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(run.join(dir).join("keep.txt")).unwrap(),
            b"keep"
        );
    }
}

#[tokio::test]
async fn cleanup_refuses_nonprivate_directories_without_chmod_or_unlink() {
    for name in ["run", "run/transfers", "run/staged"] {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join(".orchestry");
        std::fs::create_dir(&state).unwrap();
        private_dir(&state.join("run"));
        private_dir(&state.join("run/transfers"));
        private_dir(&state.join("run/staged"));
        let untrusted = state.join(name);
        std::fs::set_permissions(&untrusted, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sentinel = untrusted.join("keep.part");
        std::fs::write(&sentinel, b"sentinel").unwrap();
        let h = handler(temp.path()).await;
        assert!(h.cleanup_stale_transfers().is_err(), "{name}");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"sentinel");
        assert_eq!(
            std::fs::metadata(&untrusted).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
