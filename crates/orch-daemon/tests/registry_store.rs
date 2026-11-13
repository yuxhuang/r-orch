//! Registry failures at the daemon's storage boundary. These replace the three
//! ignored CLI-writer tests in remote_peers.rs: ServiceHandler's remote handlers
//! use this same FileRemoteStore, while CLI clients never own its files/locks.

use orch_domain::RemoteConfig;
use orch_infra::storage::{paths::Paths, remote::FileRemoteStore};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn entry(alias: &str) -> RemoteConfig {
    RemoteConfig {
        alias: alias.into(),
        addr: "127.0.0.1:9".into(),
        added_at: "2026-11-09T00:00:00Z".into(),
    }
}

fn fixture() -> (tempfile::TempDir, Paths, FileRemoteStore) {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::new(temp.path());
    fs::create_dir(paths.root()).unwrap();
    let store = FileRemoteStore::new(paths.clone());
    (temp, paths, store)
}

fn lock_file(paths: &Paths) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(paths.remotes_path().with_extension("lock"))
        .unwrap()
}

fn assert_unlocked(paths: &Paths) {
    let lock = lock_file(paths);
    // A concurrent process start can briefly retain the closed lock's file
    // reference. Retry contention, but still fail promptly for a leaked guard.
    let timeout = Duration::from_secs(1);
    let started = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                let waited = started.elapsed();
                assert!(
                    waited < timeout,
                    "a failed mutation releases the registry lock: still locked after {waited:?} (limit {timeout:?})"
                );
                std::thread::yield_now();
            }
            Err(error) => panic!("observe registry lock after failed mutation: {error}"),
        }
    }
    lock.unlock().unwrap();
}

async fn assert_malformed_registry_is_preserved(malformed: &str) {
    let (_temp, paths, store) = fixture();
    fs::write(paths.remotes_path(), malformed).unwrap();
    for remove in [false, true] {
        let result = if remove {
            store.remove("same").await
        } else {
            store.add(entry("new")).await
        };
        let error = result.expect_err("must reject a malformed registry");
        assert!(
            !error.to_string().is_empty(),
            "registry error must be visible"
        );
        assert_eq!(
            fs::read(paths.remotes_path()).unwrap(),
            malformed.as_bytes()
        );
        assert_unlocked(&paths);
    }
    fs::write(paths.remotes_path(), "[]").unwrap();
    store.add(entry("repaired")).await.unwrap();
    assert_eq!(store.list().await.unwrap(), vec![entry("repaired")]);
}

#[tokio::test]
async fn malformed_yaml_fails_loudly_without_overwrite_and_releases_lock() {
    assert_malformed_registry_is_preserved("[broken YAML").await;
}

#[tokio::test]
async fn invalid_stored_alias_fails_loudly_without_overwrite_and_releases_lock() {
    assert_malformed_registry_is_preserved(
        r#"[{"alias":"../bad","addr":"127.0.0.1:9","added_at":"now"}]"#,
    )
    .await;
}

#[tokio::test]
async fn duplicate_stored_alias_fails_loudly_without_overwrite_and_releases_lock() {
    assert_malformed_registry_is_preserved(
        r#"[{"alias":"same","addr":"127.0.0.1:9","added_at":"now"},{"alias":"same","addr":"127.0.0.1:9","added_at":"now"}]"#,
    )
    .await;
}

#[tokio::test]
async fn registry_lock_open_failure_is_visible_and_preserves_existing_records() {
    let (_temp, paths, store) = fixture();
    store.add(entry("existing")).await.unwrap();
    let lock_path = paths.remotes_path().with_extension("lock");
    fs::remove_file(&lock_path).unwrap();
    fs::create_dir(&lock_path).unwrap();
    let before = fs::read(paths.remotes_path()).unwrap();
    for remove in [false, true] {
        let result = if remove {
            store.remove("existing").await
        } else {
            store.add(entry("new")).await
        };
        let error = result.expect_err("must report a lock-open failure");
        assert!(error.to_string().contains("registry lock"), "{error}");
        assert_eq!(fs::read(paths.remotes_path()).unwrap(), before);
    }
    fs::remove_dir(lock_path).unwrap();
    store.add(entry("after-repair")).await.unwrap();
    assert_eq!(
        store.list().await.unwrap(),
        vec![entry("existing"), entry("after-repair")]
    );
}

// A store mutation must really be killed in another process: cancelling an
// async future cannot model kernel descriptor cleanup after SIGKILL. Re-enter
// this test binary with only this test selected and a private fixture path.
const WRITER_ROOT: &str = "ORCH_TEST_REGISTRY_KILLED_WRITER_ROOT";

struct Writer(Child);

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn registry_killed_writer_releases_lock_without_stale_file_cleanup() {
    if let Some(root) = std::env::var_os(WRITER_ROOT) {
        let store = FileRemoteStore::new(Paths::new(root));
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(store.add(entry("interrupted")))
            .expect("writer stays blocked reading the FIFO until killed");
        panic!("writer unexpectedly returned from the FIFO read");
    }

    let (temp, paths, store) = fixture();
    // The FIFO pauses the real store's read after lock acquisition, with no
    // race-prone delay or production hook. Only the child opens its read end.
    nix::unistd::mkfifo(
        &paths.remotes_path(),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let lock = lock_file(&paths);
    let before = lock.metadata().unwrap();
    let mut writer = Writer(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "registry_killed_writer_releases_lock_without_stale_file_cleanup",
            ])
            .env_clear()
            .env(WRITER_ROOT, temp.path())
            .env("HOME", temp.path())
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                temp.path().join("global.yml"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            writer.0.try_wait().unwrap().is_none(),
            "writer exited before acquiring the lock"
        );
        match lock.try_lock() {
            Ok(()) => lock.unlock().unwrap(),
            Err(std::fs::TryLockError::WouldBlock) => break,
            Err(error) => panic!("observe registry lock: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "writer never acquired the registry lock"
        );
        std::thread::yield_now();
    }
    writer.0.kill().unwrap();
    let killed = writer.0.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(killed.signal(), Some(9), "writer must die by SIGKILL");
    lock.try_lock()
        .expect("kernel releases killed writer's lock");
    lock.unlock().unwrap();
    let after = fs::metadata(paths.remotes_path().with_extension("lock")).unwrap();
    assert_eq!(
        (after.dev(), after.ino()),
        (before.dev(), before.ino()),
        "lock file remains the same stable inode"
    );
    fs::remove_file(paths.remotes_path()).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            store.add(entry("after-crash")).await.unwrap();
            assert_eq!(store.list().await.unwrap(), vec![entry("after-crash")]);
        });
}
