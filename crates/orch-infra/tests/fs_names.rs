//! R101/R129 producer tests. Runtime transactions remain the engine's responsibility.
use orch_domain::{RunStatus, TokenUsage};
use orch_infra::data_dir::{
    confined::ConfinedDir,
    spool::{FinishRecord, FinishResult, FinishSpool, ReplayDisposition, finish_file_name},
};
use std::os::unix::fs::MetadataExt;

fn canary_metadata(m: &std::fs::Metadata) -> [i128; 14] {
    [
        m.dev() as i128,
        m.ino() as i128,
        m.mode() as i128,
        m.uid() as i128,
        m.gid() as i128,
        m.nlink() as i128,
        m.len() as i128,
        m.atime() as i128,
        m.atime_nsec() as i128,
        m.mtime() as i128,
        m.mtime_nsec() as i128,
        m.ctime() as i128,
        m.ctime_nsec() as i128,
        m.rdev() as i128,
    ]
}

const VECTORS: [(&str, &str, &str); 7] = [
    (
        "prj_test1",
        "run_test1",
        "7a402d9d06dbc9a9c1e288e8ee7d2636bdab972350a11cb513de22ca059a8ae6",
    ),
    (
        "prj_test1",
        "../../../../outside-canary",
        "300053d414157031bb39555fcccdfb92cee8e3e09aa295ef82c003a48deeb8c8",
    ),
    (
        "prj_test1",
        "/outside-canary",
        "318a5d16cb6133a09a5a8af597ae8e142b783e201bd72ddb11bf64a609416f61",
    ),
    (
        "prj_test1",
        "a\0b",
        "f816d7b547b5d3226225a4e844e8f0ea3f145136efbd160981562b8b8fcc9f5e",
    ),
    (
        "prj_test1",
        "a/b",
        "026c08d56cbb044bf5df5b48f6431c20dacc214fda32ebbb2af85ab402ddee41",
    ),
    (
        "prj_test1",
        "..",
        "93143077dd25bea2a3b43f38cff5c6edfc691d35366fe3e9bb1c1af31eff0c2a",
    ),
    (
        "prj_test",
        "1run_test1",
        "a14c156547860418c7880716bab69d8a08dfb1b23a9fa7cea2714e430fb32262",
    ),
];

fn record(project: &str, run: &str) -> FinishRecord {
    FinishRecord {
        project: project.into(),
        run: run.into(),
        lineage: "synthetic-lineage".into(),
        epoch: 42,
        status: RunStatus::Succeeded,
        finished_at: "2026-11-28T00:00:00Z".into(),
        result: Some(FinishResult::Inline("synthetic result".into())),
        usage: Some(TokenUsage {
            input: 2,
            output: 3,
            total: 5,
            ..Default::default()
        }),
        last_event_number: Some(17),
    }
}

fn fixture() -> (tempfile::TempDir, ConfinedDir) {
    let temp = tempfile::tempdir().unwrap();
    orch_core::test_support::assert_hermetic(temp.path());
    let root = ConfinedDir::open(temp.path()).unwrap();
    root.create_dir("data").unwrap();
    let data = root.open_dir("data").unwrap();
    data.create_dir("spool")
        .unwrap()
        .create_dir("finish")
        .unwrap();
    data.create_dir("blobs").unwrap().create_dir("tmp").unwrap();
    std::fs::write(temp.path().join("outside-canary"), b"unchanged").unwrap();
    (temp, data)
}

#[test]
fn finish_file_name_vector() {
    assert_eq!(finish_file_name("prj_test1", "run_test1"), VECTORS[0].2);
}

#[test]
fn the_finish_file_name_is_the_whole_hash_for_any_run_id() {
    for (project, run, want) in VECTORS {
        assert_eq!(finish_file_name(project, run), want);
    }
}

#[test]
fn production_write_reopen_replay_and_removal_use_only_the_hash_names() {
    for (project, run, want) in VECTORS {
        let (temp, data) = fixture();
        let canary = temp.path().join("outside-canary");
        let before = std::fs::metadata(&canary).unwrap();
        let spool = FinishSpool::open(&data).unwrap();
        let finish = record(project, run);
        spool.write(&finish).unwrap();
        let dir = data.open_dir("spool").unwrap().open_dir("finish").unwrap();
        assert_eq!(dir.list().unwrap(), [std::ffi::OsString::from(want)]);
        assert_eq!(
            std::fs::metadata(temp.path().join("data/spool/finish").join(want))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            data.open_dir("blobs")
                .unwrap()
                .open_dir("tmp")
                .unwrap()
                .list()
                .unwrap()
                .is_empty()
        );
        drop(spool);
        let spool = FinishSpool::open(&data).unwrap();
        let mut seen = Vec::new();
        let report = spool
            .replay(|found| {
                assert_eq!(dir.list().unwrap().len(), 1); // not removed before the receipt
                seen.push(found.clone());
                Ok::<_, std::io::Error>(ReplayDisposition::Committed)
            })
            .unwrap();
        assert_eq!(seen, [finish]);
        assert_eq!(report.committed, 1);
        assert_eq!(report.discarded, 0);
        assert!(dir.list().unwrap().is_empty());
        let after = std::fs::metadata(&canary).unwrap();
        assert_eq!(canary_metadata(&before), canary_metadata(&after));
        assert_eq!(std::fs::read(&canary).unwrap(), b"unchanged");
        assert_eq!(
            data.list().unwrap(),
            [
                std::ffi::OsString::from("blobs"),
                std::ffi::OsString::from("spool")
            ]
        );
        assert_eq!(
            data.open_dir("spool").unwrap().list().unwrap(),
            [std::ffi::OsString::from("finish")]
        );
        assert_eq!(
            data.open_dir("blobs").unwrap().list().unwrap(),
            [std::ffi::OsString::from("tmp")]
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 2);
    }
}

#[test]
fn a_failed_transaction_keeps_the_finish_for_the_next_replay() {
    let (_temp, data) = fixture();
    let spool = FinishSpool::open(&data).unwrap();
    let finish = record("project", "run");
    spool.write(&finish).unwrap();
    assert!(
        spool
            .replay(|_| Err::<ReplayDisposition, _>(std::io::Error::other("writer unavailable")))
            .is_err()
    );
    let report = spool
        .replay(|found| {
            assert_eq!(found, &finish);
            Ok::<_, std::io::Error>(ReplayDisposition::Committed)
        })
        .unwrap();
    assert_eq!(report.committed, 1);
    assert_eq!(
        spool
            .replay(|_| -> Result<_, std::io::Error> { panic!("removed finish replayed twice") })
            .unwrap()
            .committed,
        0
    );
}

#[test]
fn retrying_the_same_finish_is_idempotent_but_a_conflict_preserves_the_first() {
    let (_temp, data) = fixture();
    let spool = FinishSpool::open(&data).unwrap();
    let finish = record("project", "run");
    spool.write(&finish).unwrap();
    spool.write(&finish).unwrap();
    let mut different = finish.clone();
    different.epoch += 1;
    assert!(spool.write(&different).is_err());
    spool
        .replay(|found| {
            assert_eq!(found, &finish);
            Ok::<_, std::io::Error>(ReplayDisposition::Committed)
        })
        .unwrap();
}

#[test]
fn damaged_and_obsolete_finishes_are_counted_but_unknown_names_are_untouched() {
    let (temp, data) = fixture();
    let spool = FinishSpool::open(&data).unwrap();
    spool.write(&record("project", "obsolete")).unwrap();
    let dir = data.open_dir("spool").unwrap().open_dir("finish").unwrap();
    use std::io::Write;
    dir.create_file("a".repeat(64))
        .unwrap()
        .write_all(b"truncated")
        .unwrap();
    dir.create_file("not-a-hash")
        .unwrap()
        .write_all(b"keep me")
        .unwrap();
    let report = spool
        .replay(|finish| {
            assert_eq!(finish.run, "obsolete");
            Ok::<_, std::io::Error>(ReplayDisposition::Discard)
        })
        .unwrap();
    assert_eq!(report.discarded, 2);
    assert_eq!(
        report.unrecognized,
        [std::ffi::OsString::from("not-a-hash")]
    );
    assert_eq!(
        std::fs::read(temp.path().join("data/spool/finish/not-a-hash")).unwrap(),
        b"keep me"
    );
    assert_eq!(
        dir.list().unwrap(),
        [std::ffi::OsString::from("not-a-hash")]
    );
}

#[test]
fn changed_bytes_or_a_different_hash_name_never_reach_the_transaction() {
    for wrong_name in [false, true] {
        let (temp, data) = fixture();
        let spool = FinishSpool::open(&data).unwrap();
        spool.write(&record("project", "run")).unwrap();
        let path = temp
            .path()
            .join("data/spool/finish")
            .join(finish_file_name("project", "run"));
        if wrong_name {
            std::fs::rename(&path, path.with_file_name("0".repeat(64))).unwrap();
        } else {
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("synthetic result", "tampered result");
            std::fs::write(&path, text).unwrap();
        }
        let report = spool
            .replay(|_| -> Result<_, std::io::Error> { panic!("invalid finish applied") })
            .unwrap();
        assert_eq!(report.discarded, 1);
    }
}

#[test]
fn inline_results_are_bounded_by_bytes_and_large_results_use_published_blob_references() {
    let (_temp, data) = fixture();
    let spool = FinishSpool::open(&data).unwrap();
    let mut finish = record("project", "run");
    finish.result = Some(FinishResult::Inline("é".repeat(32768)));
    spool.write(&finish).unwrap();
    spool.remove_committed(&finish).unwrap();
    finish.result = Some(FinishResult::Inline("x".repeat(65537)));
    assert!(spool.write(&finish).is_err());
    finish.result = Some(FinishResult::Blob {
        hash: "../outside-canary".into(),
        bytes: 65537,
    });
    assert!(spool.write(&finish).is_err());
    // Blob publication is the caller's responsibility; the spool never opens this hash.
    finish.result = Some(FinishResult::Blob {
        hash: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
        bytes: 65537,
    });
    spool.write(&finish).unwrap();
    spool
        .replay(|found| {
            assert_eq!(found, &finish);
            Ok::<_, std::io::Error>(ReplayDisposition::Committed)
        })
        .unwrap();
}

#[test]
fn oversized_records_and_nonterminal_statuses_are_refused_before_any_file_is_created() {
    let (_temp, data) = fixture();
    let spool = FinishSpool::open(&data).unwrap();
    let mut finish = record("project", "run");
    finish.project = "p".repeat(128 * 1024);
    assert!(spool.write(&finish).is_err());
    finish = record("project", "run");
    for status in [RunStatus::Preparing, RunStatus::Running] {
        finish.status = status;
        assert!(spool.write(&finish).is_err());
    }
    assert!(
        data.open_dir("spool")
            .unwrap()
            .open_dir("finish")
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
    assert!(
        data.open_dir("blobs")
            .unwrap()
            .open_dir("tmp")
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_oversized_file_is_discarded_without_applying_a_prefix() {
    let (_temp, data) = fixture();
    use std::io::Write;
    data.open_dir("spool")
        .unwrap()
        .open_dir("finish")
        .unwrap()
        .create_file("a".repeat(64))
        .unwrap()
        .write_all(&vec![b'x'; 128 * 1024 + 1])
        .unwrap();
    let report = FinishSpool::open(&data)
        .unwrap()
        .replay(|_| -> Result<_, std::io::Error> { panic!("oversized file applied") })
        .unwrap();
    assert_eq!(report.discarded, 1);
}

#[test]
fn write_failure_leaves_the_callers_finish_available_for_a_later_attempt() {
    let (temp, data) = fixture();
    let spool = FinishSpool::open(&data).unwrap();
    let finish = record("project", "run");
    let tmp = temp.path().join("data/blobs/tmp");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failure = spool.write(&finish);
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        failure.is_err(),
        "this fixture must run as an ordinary user"
    );
    assert!(
        data.open_dir("spool")
            .unwrap()
            .open_dir("finish")
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
    spool.write(&finish).unwrap();
    spool.remove_committed(&finish).unwrap();
}

#[test]
fn links_and_special_files_at_the_finish_name_are_refused_without_touching_the_canary() {
    use std::os::unix::fs::symlink;
    for kind in ["symlink", "hardlink", "fifo", "directory"] {
        let (temp, data) = fixture();
        let spool = FinishSpool::open(&data).unwrap();
        let finish = record("project", "run");
        let path = temp
            .path()
            .join("data/spool/finish")
            .join(finish_file_name("project", "run"));
        let canary = temp.path().join("outside-canary");
        match kind {
            "symlink" => symlink(&canary, &path).unwrap(),
            "hardlink" => std::fs::hard_link(&canary, &path).unwrap(),
            "fifo" => nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap(),
            _ => std::fs::create_dir(&path).unwrap(),
        }
        let before = std::fs::metadata(&canary).unwrap();
        assert!(spool.write(&finish).is_err(), "{kind}");
        assert!(
            spool
                .replay(|_| -> Result<_, std::io::Error> { panic!("unsafe entry applied") })
                .is_err(),
            "{kind}"
        );
        assert!(spool.remove_committed(&finish).is_err(), "{kind}");
        assert!(std::fs::symlink_metadata(&path).is_ok());
        let after = std::fs::metadata(&canary).unwrap();
        assert_eq!(canary_metadata(&before), canary_metadata(&after));
        assert_eq!(std::fs::read(&canary).unwrap(), b"unchanged");
    }
}

// A4: archived predecessor scenarios, now retained alongside producer tests.
use orch_core::{
    store::{Caller, Entity, EntityKind, Lane, Principal, Storage, StorageExt},
    test_support::make_run,
};
use orch_infra::{
    data_dir::{DataDir, Holder, LockKind},
    sqlite::{SqliteStorage, StorageOptions},
    storage::v2::FileStorage,
};
use std::sync::Arc;

const A4_VECTORS: [(&str, &str); 7] = [
    ("prj_test1", "run_test1"),
    ("prj_test1", "../../../../outside-canary"),
    ("prj_test1", "/outside-canary"),
    ("prj_test1", "a\0b"),
    ("prj_test1", "a/b"),
    ("prj_test1", ".."),
    ("prj_test", "1run_test1"),
];

fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}

async fn write_vectors(storage: &dyn Storage) {
    for (project, id) in A4_VECTORS {
        let mut run = make_run();
        run.id = id.into();
        run.task_id = "synthetic-task".into();
        run.agent_id = "synthetic-agent".into();
        storage
            .transaction(owner(project), Lane::Entities, move |u| {
                u.create(Entity::Run(Box::new(run)))
            })
            .await
            .unwrap();
    }
}

async fn read_vectors(storage: &dyn Storage) {
    for (project, id) in A4_VECTORS {
        let snapshot = storage.read(owner(project)).await.unwrap();
        let row = snapshot.get(EntityKind::Run, id).await.unwrap().unwrap();
        assert_eq!(row.entity.id().as_bytes(), id.as_bytes());
    }
}

#[tokio::test]
async fn a4_finish_ids_round_trip_on_memory_port() {
    let storage = orch_core::store::fake::FakeStorage::default();
    write_vectors(&storage).await;
    read_vectors(&storage).await;
}

#[tokio::test]
async fn a4_finish_ids_survive_file_port_reopen() {
    let root = tempfile::tempdir().unwrap();
    orch_core::test_support::assert_hermetic(root.path());
    let storage = FileStorage::new(root.path());
    write_vectors(&storage).await;
    drop(storage);
    read_vectors(&FileStorage::new(root.path())).await;
}

#[tokio::test]
async fn a4_finish_ids_survive_sqlite_port_reopen() {
    let root = tempfile::tempdir().unwrap();
    orch_core::test_support::assert_hermetic(root.path());
    let data = Arc::new(
        DataDir::acquire(
            &root.path().join("data"),
            &Holder {
                kind: LockKind::Daemon,
                pid: std::process::id(),
                started_at: "synthetic".into(),
                binary_version: "test".into(),
                executable: "/synthetic/orch".into(),
            },
        )
        .unwrap(),
    );
    let storage = SqliteStorage::open(data.clone(), StorageOptions::default()).unwrap();
    write_vectors(&storage).await;
    storage.close().await.unwrap();
    drop(storage);
    let storage = SqliteStorage::open(data, StorageOptions::default()).unwrap();
    read_vectors(&storage).await;
    storage.close().await.unwrap();
}
