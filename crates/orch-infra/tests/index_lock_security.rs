//! Unix lock creation must neither expose the lock nor follow a symlink.
#![cfg(unix)]

use std::os::unix::fs::{PermissionsExt, symlink};

use orch_core::Error;
use orch_infra::storage::index::{IndexManager, ItemFormat};
use serde_json::{Value, json};

#[tokio::test]
async fn fresh_index_writer_lock_is_owner_only() {
    let temp = tempfile::tempdir().unwrap();
    let index = IndexManager::<Value>::new(temp.path().into(), ItemFormat::Json);
    index.write_index(&[]).await.unwrap();

    let path = temp.path().join(".index.lock");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{} mode was {mode:o}", path.display());
}

#[tokio::test]
async fn symlinked_index_writer_lock_fails_without_publishing() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    std::fs::write(&target, "unchanged\n").unwrap();
    let path = temp.path().join(".index.lock");
    symlink(&target, &path).unwrap();
    let index = IndexManager::<Value>::new(temp.path().into(), ItemFormat::Json);
    std::fs::write(index.index_path(), "[]\n").unwrap();

    for operation in ["write", "update", "rebuild"] {
        let result = match operation {
            "write" => index.write_index(&[json!({"id": "new"})]).await,
            "update" => index.update_index(|_| vec![json!({"id": "new"})]).await,
            _ => index.rebuild_index().await.map(|_| ()),
        };
        let error = result.expect_err("symlinked lock must fail closed");
        assert!(
            error.to_string().contains(&path.display().to_string()),
            "{error}"
        );
        assert!(
            matches!(error, Error::Io { source, .. } if source.raw_os_error() == Some(nix::libc::ELOOP)),
            "{operation} must propagate the no-follow error"
        );
        assert_eq!(std::fs::read_to_string(index.index_path()).unwrap(), "[]\n");
    }
    assert_eq!(std::fs::read_to_string(target).unwrap(), "unchanged\n");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    std::fs::remove_file(path).unwrap();
    index.write_index(&[json!({"id": "after"})]).await.unwrap();
    assert_eq!(
        index.read_index().await.unwrap(),
        vec![json!({"id": "after"})]
    );
}

#[tokio::test]
async fn dangling_index_writer_lock_symlink_fails_without_creating_target() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("missing-target");
    let path = temp.path().join(".index.lock");
    symlink(&target, &path).unwrap();
    let index = IndexManager::<Value>::new(temp.path().into(), ItemFormat::Json);

    let error = index
        .write_index(&[])
        .await
        .expect_err("dangling lock symlink must fail closed");
    assert!(
        error.to_string().contains(&path.display().to_string()),
        "{error}"
    );
    assert!(
        matches!(error, Error::Io { source, .. } if source.raw_os_error() == Some(nix::libc::ELOOP))
    );
    assert!(
        !target.exists(),
        "opening the lock created its symlink target"
    );
    assert!(
        !index.index_path().exists(),
        "failed acquisition published an index"
    );
}
