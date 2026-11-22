//! The 1.0 owner lock is opened as-is and never written or created.
use orch_infra::data_dir::LegacyGuard;
use std::io::{Read, Write};

#[test]
fn legacy_owner_lock_is_held_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".daemon-owner.lock");
    std::fs::write(&path, b"legacy-holder\n").unwrap();
    let guard = LegacyGuard::acquire(temp.path()).unwrap().unwrap();
    let file = std::fs::File::open(&path).unwrap();
    assert!(nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"legacy-holder\n");
    drop(guard);
}

#[test]
fn start_is_refused_while_a_legacy_daemon_holds_the_project() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(".daemon-owner.lock");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"legacy\n").unwrap();
    let mut lock =
        nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock).unwrap();
    assert!(LegacyGuard::acquire(temp.path()).is_err());
    let mut text = String::new();
    // A read from an independent descriptor proves acquisition did not truncate.
    std::fs::File::open(path)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "legacy\n");
    lock.flush().unwrap();
}

#[test]
fn an_absent_legacy_lock_file_is_not_created() {
    let temp = tempfile::tempdir().unwrap();
    assert!(LegacyGuard::acquire(temp.path()).unwrap().is_none());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}
