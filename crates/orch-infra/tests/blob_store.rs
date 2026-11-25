//! R58, R59, R101, R102 for blobs: a blob is reached only by a parsed hash, written durably before any row
//! may name it, stored once, verified on read, and never reached through a link.
use orch_infra::blob::{BlobError, BlobHash, DiskBlobStore, MAX_BLOB_BYTES};
use orch_infra::data_dir::confined::ConfinedDir;
use orch_infra::data_dir::{DataDir, Holder, LockKind};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    store: DiskBlobStore,
    _data: DataDir,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("data");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("canary"), b"unchanged").unwrap();
    let data = DataDir::acquire(
        &root,
        &Holder {
            kind: LockKind::Daemon,
            pid: 1,
            started_at: "2026-11-22T00:00:00Z".into(),
            binary_version: "test".into(),
            executable: "test".into(),
        },
    )
    .unwrap();
    let store = DiskBlobStore::open(data.root()).unwrap();
    Fixture {
        root: root.canonicalize().unwrap(),
        _tmp: tmp,
        outside,
        store,
        _data: data,
    }
}

impl Fixture {
    fn blob_path(&self, hash: &BlobHash) -> PathBuf {
        let hex = hash.to_string();
        self.root
            .join("blobs/sha256")
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(&hex)
    }
    fn tmp_entries(&self) -> usize {
        std::fs::read_dir(self.root.join("blobs/tmp"))
            .unwrap()
            .count()
    }
    fn canary_unchanged(&self) {
        assert_eq!(
            std::fs::read(self.outside.join("canary")).unwrap(),
            b"unchanged"
        );
    }
}

fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files_below(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn bytes_exist_before_the_row() {
    let f = fixture();
    let hash = f.store.put(b"rendered prompt").unwrap();
    // The caller gets the hash only once the bytes sit, whole and private, under that name.
    let path = f.blob_path(&hash);
    assert_eq!(std::fs::read(&path).unwrap(), b"rendered prompt");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(f.tmp_entries(), 0);
    assert_eq!(f.store.get(&hash).unwrap(), b"rendered prompt");
    for dir in [
        f.root.join("blobs/sha256").join(&hash.to_string()[0..2]),
        f.root
            .join("blobs/sha256")
            .join(&hash.to_string()[0..2])
            .join(&hash.to_string()[2..4]),
    ] {
        assert_eq!(
            std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn an_empty_blob_round_trips() {
    let f = fixture();
    let hash = f.store.put(b"").unwrap();
    assert_eq!(f.store.get(&hash).unwrap(), b"");
}

#[test]
fn a_corrupted_blob_is_detected_on_read() {
    let f = fixture();
    let hash = f.store.put(b"the original bytes").unwrap();
    let path = f.blob_path(&hash);
    std::fs::write(&path, b"the original byteZ").unwrap();
    assert!(matches!(f.store.get(&hash), Err(BlobError::Corrupt(h)) if h == hash));
    std::fs::write(&path, b"the original").unwrap();
    assert!(matches!(f.store.get(&hash), Err(BlobError::Corrupt(_))));
    std::fs::write(&path, b"").unwrap();
    assert!(matches!(f.store.get(&hash), Err(BlobError::Corrupt(_))));
}

#[test]
fn a_missing_blob_is_not_found_and_an_oversize_file_is_refused() {
    let f = fixture();
    let absent = BlobHash::of(b"never stored");
    assert!(matches!(f.store.get(&absent), Err(BlobError::NotFound(_))));
    let hash = f.store.put(b"x").unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(f.blob_path(&hash))
        .unwrap();
    file.set_len(MAX_BLOB_BYTES + 1).unwrap(); // sparse
    assert!(matches!(
        f.store.get(&hash),
        Err(BlobError::TooLarge { .. })
    ));
}

#[test]
fn equal_prompts_are_stored_once() {
    let f = fixture();
    let first = f.store.put(b"same prompt").unwrap();
    let inode = std::fs::metadata(f.blob_path(&first)).unwrap().ino();
    let second = f.store.put(b"same prompt").unwrap();
    assert_eq!(first, second);
    assert_eq!(files_below(&f.root.join("blobs")).len(), 1);
    // Not rewritten: the same inode, not a rename of a fresh copy.
    assert_eq!(
        std::fs::metadata(f.blob_path(&second)).unwrap().ino(),
        inode
    );
    let other = f.store.put(b"another prompt").unwrap();
    assert_ne!(other, first);
    assert_eq!(files_below(&f.root.join("blobs")).len(), 2);
}

#[test]
fn a_corrupt_blob_at_a_stored_name_is_replaced_by_the_next_put() {
    let f = fixture();
    let hash = f.store.put(b"good bytes").unwrap();
    std::fs::write(f.blob_path(&hash), b"bad bytes!").unwrap();
    assert_eq!(f.store.put(b"good bytes").unwrap(), hash);
    assert_eq!(f.store.get(&hash).unwrap(), b"good bytes");
}

#[test]
fn a_hash_that_is_not_64_hex_digits_is_refused_and_touches_nothing() {
    let f = fixture();
    let before = files_below(&f.root.join("blobs"));
    let good = BlobHash::of(b"x").to_string();
    for bad in [
        "../x",
        "../../../../outside/canary",
        "/outside/canary",
        &good[..63],
        &good.to_uppercase(),
        "a/b",
        "",
        "ab\0cd",
    ] {
        assert!(bad.parse::<BlobHash>().is_err(), "{bad:?}");
    }
    assert_eq!(files_below(&f.root.join("blobs")), before);
    f.canary_unchanged();
}

#[test]
fn a_planted_link_in_the_blob_tree_is_refused() {
    let f = fixture();
    let hash = BlobHash::of(b"payload");
    let hex = hash.to_string();
    let shard = f.root.join("blobs/sha256").join(&hex[0..2]);
    // A link at a shard directory, followed on neither read nor write.
    symlink(&f.outside, &shard).unwrap();
    assert!(f.store.get(&hash).is_err());
    assert!(f.store.put(b"payload").is_err());
    assert!(f.store.remove(&hash).is_err());
    assert_eq!(std::fs::read_dir(&f.outside).unwrap().count(), 1);
    f.canary_unchanged();
    std::fs::remove_file(&shard).unwrap();

    // A link at the final name: refused on read, and replaced (not written through) by a put.
    let stored = f.store.put(b"payload").unwrap();
    let path = f.blob_path(&stored);
    std::fs::remove_file(&path).unwrap();
    symlink(f.outside.join("canary"), &path).unwrap();
    assert!(f.store.get(&stored).is_err());
    assert!(f.store.remove(&stored).is_err());
    assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(f.store.put(b"payload").unwrap(), stored);
    assert!(path.symlink_metadata().unwrap().file_type().is_file());
    assert_eq!(f.store.get(&stored).unwrap(), b"payload");
    f.canary_unchanged();
}

#[test]
fn a_link_at_the_blob_directories_themselves_stops_open() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir(&root).unwrap();
    symlink(&outside, root.join("blobs")).unwrap();
    assert!(DiskBlobStore::open(&ConfinedDir::open(&root).unwrap()).is_err());
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn a_hard_link_at_a_blob_name_reads_only_when_the_bytes_hash_to_the_name() {
    let f = fixture();
    let hash = f.store.put(b"honest bytes").unwrap();
    let path = f.blob_path(&hash);
    // A snapshot made with hard links must not stop a read.
    std::fs::hard_link(&path, f.outside.join("snapshot")).unwrap();
    assert_eq!(f.store.get(&hash).unwrap(), b"honest bytes");
    // A name hard-linked to an outside file with other bytes is detected and nothing is written.
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(f.outside.join("canary"), &path).unwrap();
    assert!(matches!(f.store.get(&hash), Err(BlobError::Corrupt(_))));
    f.canary_unchanged();
}

#[test]
fn a_fifo_at_a_blob_name_is_refused_without_blocking_and_replaced_by_a_put() {
    let f = fixture();
    let hash = BlobHash::of(b"behind a fifo");
    let path = f.blob_path(&hash);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    let (tx, rx) = mpsc::channel();
    let store = std::sync::Arc::new(f.store);
    let worker = store.clone();
    std::thread::spawn(move || {
        let read = worker.get(&hash).is_err();
        let removed = worker.remove(&hash).is_err();
        let put = worker.put(b"behind a fifo");
        tx.send((read, removed, put.is_ok())).unwrap();
    });
    let (read_refused, remove_refused, put_ok) = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("a FIFO at a blob's name must not block the caller");
    assert!(read_refused && remove_refused && put_ok);
    assert_eq!(store.get(&hash).unwrap(), b"behind a fifo");
}

#[test]
fn a_failed_write_leaves_no_blob_and_no_temporary_file() {
    let f = fixture();
    let hash = BlobHash::of(b"cannot rename here");
    // A directory at the final name makes the rename fail after the bytes were written and synced.
    std::fs::create_dir_all(f.blob_path(&hash)).unwrap();
    assert!(f.store.put(b"cannot rename here").is_err());
    assert_eq!(f.tmp_entries(), 0);
    assert!(f.blob_path(&hash).is_dir());
    assert!(matches!(f.store.get(&hash), Err(BlobError::Confined(_))));
}

#[test]
fn a_put_into_an_unwritable_temporary_directory_fails_cleanly() {
    if nix::unistd::geteuid().is_root() {
        return; // root ignores the mode, so there is no failure to observe
    }
    let f = fixture();
    let tmp = f.root.join("blobs/tmp");
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = f.store.put(b"no room");
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(f.tmp_entries(), 0);
    assert!(files_below(&f.root.join("blobs")).is_empty());
}

#[test]
fn a_blob_over_the_limit_is_refused_before_anything_is_written() {
    let f = fixture();
    let too_big = vec![0u8; MAX_BLOB_BYTES as usize + 1];
    assert!(matches!(
        f.store.put(&too_big),
        Err(BlobError::TooLarge { .. })
    ));
    assert_eq!(f.tmp_entries(), 0);
    assert!(files_below(&f.root.join("blobs")).is_empty());
}

#[test]
fn remove_deletes_the_file_and_a_repeat_is_success() {
    let f = fixture();
    let hash = f.store.put(b"to delete").unwrap();
    assert!(f.store.remove(&hash).unwrap());
    assert!(!f.blob_path(&hash).exists());
    assert!(!f.store.remove(&hash).unwrap());
    assert!(matches!(f.store.get(&hash), Err(BlobError::NotFound(_))));
}

#[test]
fn a_stray_file_in_the_blob_tree_is_never_opened_or_deleted() {
    let f = fixture();
    let hash = f.store.put(b"neighbour").unwrap();
    let stray = f.blob_path(&hash).with_file_name("notes.txt");
    std::fs::write(&stray, b"notes").unwrap();
    assert!(f.store.remove(&hash).unwrap());
    assert_eq!(std::fs::read(&stray).unwrap(), b"notes");
}

#[test]
fn concurrent_puts_of_the_same_bytes_leave_one_intact_blob() {
    let f = fixture();
    let store = std::sync::Arc::new(f.store);
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || store.put(&[7u8; 100_000]).unwrap())
        })
        .collect();
    let hashes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(hashes.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(store.get(&hashes[0]).unwrap(), vec![7u8; 100_000]);
    assert_eq!(
        std::fs::read_dir(f.root.join("blobs/tmp")).unwrap().count(),
        0
    );
}

// Real schema/reader boundary: no mock database or parallel schema sketch.
fn database(f: &Fixture) -> orch_infra::sqlite::ConnectionHandle<'_> {
    use orch_infra::sqlite::{ConnectionRole, Synchronous, migrations::migrate, open_connection};
    let mut c = open_connection(&f._data, ConnectionRole::Writer, Synchronous::Normal).unwrap();
    migrate(&f._data, &mut c).unwrap();
    for project in ["p", "other"] {
        c.execute("INSERT INTO tasks (project_id,id,title,description,status,priority,attempts,max_attempts,position,created_by_kind,created_at,updated_at,revision) VALUES (?1,'task','t','','todo',1,0,3,0,'owner',0,0,1)", [project]).unwrap();
        for (run, number) in [("run", 1), ("run2", 2)] {
            c.execute("INSERT INTO runs (project_id,id,task_id,agent_id,run_no,counted,attempt_no,epoch,status,started_at,revision) VALUES (?1,?2,'task','agent',?3,1,?3,?3,'running',0,1)", rusqlite::params![project, run, number]).unwrap();
        }
    }
    c
}

fn count(c: &rusqlite::Connection, table: &str) -> i64 {
    // Table names are test constants, never application input.
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn prompt_rows_deduplicate_and_round_trip_with_the_real_reader() {
    use orch_infra::blob::BlobReferences;
    let f = fixture();
    let mut c = database(&f);
    let refs = BlobReferences::new(&f.store, "p");
    let mut tx = c.transaction().unwrap();
    let first = refs
        .store_prompt(&mut tx, "run", "rendered prompt", 123)
        .unwrap();
    let second = refs
        .store_prompt(&mut tx, "run2", "rendered prompt", 456)
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(count(&tx, "blobs"), 1);
    let row: (String, i64, String, i64, Option<i64>) = tx
        .query_row(
            "SELECT sha256,size,storage_location,created_at,unreferenced_since FROM blobs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(row, (first.to_string(), 15, "disk".into(), 123, None));
    assert_eq!(
        f.store.get(&row.0.parse().unwrap()).unwrap(),
        b"rendered prompt"
    );
    tx.commit().unwrap();
    assert_eq!(
        refs.load_prompt(&c, "run").unwrap(),
        Some("rendered prompt".into())
    );
    assert_eq!(
        BlobReferences::new(&f.store, "other")
            .load_prompt(&c, "run")
            .unwrap(),
        None
    );
    assert_eq!(files_below(&f.root.join("blobs")).len(), 1);
}

#[test]
fn attachment_is_a_blob_and_its_name_is_only_data() {
    use orch_infra::blob::BlobReferences;
    let f = fixture();
    let mut c = database(&f);
    let refs = BlobReferences::new(&f.store, "p");
    let name = "../../outside/canary";
    let mut tx = c.transaction().unwrap();
    let hash = refs
        .store_attachment(&mut tx, "task", name, &[0xff, 0x00], 0)
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(count(&c, "task_attachments"), 1);
    assert_eq!(
        refs.load_attachment(&c, "task", name).unwrap(),
        Some(vec![0xff, 0])
    );
    assert_eq!(
        BlobReferences::new(&f.store, "other")
            .load_attachment(&c, "task", name)
            .unwrap(),
        None
    );
    assert_eq!(c.query_row::<String,_,_>("SELECT sha256 FROM task_attachments WHERE project_id='p' AND task_id='task' AND name=?1", [name], |r| r.get(0)).unwrap(), hash.to_string());
    f.canary_unchanged();
}

#[test]
fn rolled_back_references_leave_no_rows_and_are_replayable() {
    use orch_infra::blob::BlobReferences;
    let f = fixture();
    let mut c = database(&f);
    let refs = BlobReferences::new(&f.store, "p");
    let mut tx = c.transaction().unwrap();
    let hash = refs.store_prompt(&mut tx, "run", "rollback", 0).unwrap();
    refs.store_attachment(&mut tx, "task", "a", b"rollback", 0)
        .unwrap();
    tx.rollback().unwrap();
    assert_eq!(count(&c, "blobs"), 0);
    assert_eq!(count(&c, "task_attachments"), 0);
    assert_eq!(refs.load_prompt(&c, "run").unwrap(), None);
    assert_eq!(f.store.get(&hash).unwrap(), b"rollback");
    let mut tx = c.transaction().unwrap();
    refs.store_prompt(&mut tx, "run", "rollback", 1).unwrap();
    tx.commit().unwrap();
    assert_eq!(count(&c, "blobs"), 1);
}

#[test]
fn failed_owners_and_failed_disk_publication_leave_no_metadata() {
    use orch_infra::blob::BlobReferences;
    let f = fixture();
    let mut c = database(&f);
    let refs = BlobReferences::new(&f.store, "p");
    let mut tx = c.transaction().unwrap();
    assert!(refs.store_prompt(&mut tx, "missing", "x", 0).is_err());
    assert!(
        refs.store_attachment(&mut tx, "missing", "a", b"x", 0)
            .is_err()
    );
    std::fs::remove_dir(f.root.join("blobs/tmp")).unwrap();
    assert!(
        refs.store_prompt(&mut tx, "run", "disk failure", 0)
            .is_err()
    );
    assert_eq!(count(&tx, "blobs"), 0);
    assert_eq!(count(&tx, "task_attachments"), 0);
    tx.commit().unwrap();
    assert_eq!(refs.load_prompt(&c, "run").unwrap(), None);
}

#[test]
fn row_hashes_and_metadata_are_checked_before_reading_bytes() {
    use orch_infra::blob::BlobReferences;
    let f = fixture();
    let mut c = database(&f);
    let refs = BlobReferences::new(&f.store, "p");
    let mut tx = c.transaction().unwrap();
    let hash = refs.store_prompt(&mut tx, "run", "intact", 0).unwrap();
    tx.commit().unwrap();
    c.execute("UPDATE blobs SET size=99", []).unwrap();
    assert!(refs.load_prompt(&c, "run").is_err());
    let mut tx = c.transaction().unwrap();
    assert!(refs.store_prompt(&mut tx, "run2", "intact", 0).is_err());
    assert_eq!(refs.load_prompt(&tx, "run2").unwrap(), None);
    tx.rollback().unwrap();
    c.execute("UPDATE blobs SET size=6", []).unwrap();
    std::fs::write(f.blob_path(&hash), b"broken").unwrap();
    assert!(refs.load_prompt(&c, "run").is_err());
    c.execute(
        "UPDATE runs SET prompt_sha256='../../outside/canary' WHERE project_id='p'",
        [],
    )
    .unwrap();
    assert!(refs.load_prompt(&c, "run").is_err());
    f.canary_unchanged();
}

// Project and error-contract regressions from independent review I1.
#[test]
fn owner_only_in_another_project_is_missing() {
    use orch_infra::blob::{BlobReferences, ReferenceError};
    let f = fixture();
    let mut c = database(&f);
    c.execute("INSERT INTO tasks (project_id,id,title,description,status,priority,attempts,max_attempts,position,created_by_kind,created_at,updated_at,revision) VALUES ('other','only_other','t','','todo',1,0,3,0,'owner',0,0,1)", []).unwrap();
    c.execute("INSERT INTO runs (project_id,id,task_id,agent_id,run_no,counted,attempt_no,epoch,status,started_at,revision) VALUES ('other','run_other','only_other','agent',9,1,9,9,'running',0,1)", []).unwrap();
    let refs = BlobReferences::new(&f.store, "p");
    let mut tx = c.transaction().unwrap();
    assert!(matches!(
        refs.store_prompt(&mut tx, "run_other", "x", 0),
        Err(ReferenceError::MissingOwner { kind: "run" })
    ));
    assert!(matches!(
        refs.store_attachment(&mut tx, "only_other", "a", b"x", 0),
        Err(ReferenceError::MissingOwner { kind: "task" })
    ));
    assert_eq!(count(&tx, "blobs"), 0);
    assert_eq!(count(&tx, "task_attachments"), 0);
}

#[test]
fn another_projects_blob_row_does_not_authorize_a_read() {
    use orch_infra::blob::{BlobReferences, ReferenceError};
    let f = fixture();
    let mut c = database(&f);
    let mut tx = c.transaction().unwrap();
    let hash = BlobReferences::new(&f.store, "p")
        .store_prompt(&mut tx, "run", "shared", 0)
        .unwrap();
    tx.commit().unwrap();
    c.execute(
        "UPDATE runs SET prompt_sha256=?1 WHERE project_id='other' AND id='run'",
        [hash.to_string()],
    )
    .unwrap();
    assert!(matches!(
        BlobReferences::new(&f.store, "other").load_prompt(&c, "run"),
        Err(ReferenceError::Metadata)
    ));
}

#[test]
fn reregistration_clears_the_sweep_mark_and_attachment_replaces() {
    use orch_infra::blob::BlobReferences;
    let f = fixture();
    let mut c = database(&f);
    let refs = BlobReferences::new(&f.store, "p");
    let mut tx = c.transaction().unwrap();
    refs.store_prompt(&mut tx, "run", "mark", 0).unwrap();
    tx.execute("UPDATE blobs SET unreferenced_since=5", [])
        .unwrap();
    refs.store_prompt(&mut tx, "run2", "mark", 1).unwrap();
    assert_eq!(
        tx.query_row::<Option<i64>, _, _>("SELECT unreferenced_since FROM blobs", [], |r| r.get(0))
            .unwrap(),
        None
    );
    refs.store_attachment(&mut tx, "task", "a", b"one", 0)
        .unwrap();
    refs.store_attachment(&mut tx, "task", "a", b"two", 0)
        .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        refs.load_attachment(&c, "task", "a").unwrap(),
        Some(b"two".to_vec())
    );
    assert_eq!(count(&c, "task_attachments"), 1);
}

#[test]
fn error_variants_and_location() {
    use orch_infra::blob::{BlobError, BlobReferences, ReferenceError};
    let f = fixture();
    let mut c = database(&f);
    let refs = BlobReferences::new(&f.store, "p");
    let mut tx = c.transaction().unwrap();
    let hash = refs.store_prompt(&mut tx, "run", "loc", 0).unwrap();
    tx.commit().unwrap();
    c.execute("UPDATE blobs SET storage_location='object'", [])
        .unwrap();
    assert!(matches!(
        refs.load_prompt(&c, "run"),
        Err(ReferenceError::Metadata)
    ));
    c.execute("UPDATE blobs SET storage_location='disk'", [])
        .unwrap();
    std::fs::write(f.blob_path(&hash), b"bad").unwrap();
    assert!(matches!(
        refs.load_prompt(&c, "run"),
        Err(ReferenceError::Blob(BlobError::Corrupt(_)))
    ));
    std::fs::remove_file(f.blob_path(&hash)).unwrap();
    assert!(matches!(
        refs.load_prompt(&c, "run"),
        Err(ReferenceError::Blob(BlobError::NotFound(_)))
    ));
    c.execute(
        "UPDATE runs SET prompt_sha256='../x' WHERE project_id='p' AND id='run'",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO task_attachments (project_id,task_id,name,sha256) VALUES ('p','task','bad','../x')", []).unwrap();
    c.execute("UPDATE blobs SET sha256='../x'", []).unwrap();
    assert!(matches!(
        refs.load_prompt(&c, "run"),
        Err(ReferenceError::Blob(BlobError::InvalidHash(_)))
    ));
    assert!(matches!(
        refs.load_attachment(&c, "task", "bad"),
        Err(ReferenceError::Blob(BlobError::InvalidHash(_)))
    ));
    f.canary_unchanged();
}

#[test]
fn metadata_refusal_preserves_existing_prompt_and_attachment_links() {
    use orch_infra::blob::{BlobReferences, ReferenceError};
    for (size, location) in [(99, "disk"), (9, "object")] {
        let f = fixture();
        let mut c = database(&f);
        let refs = BlobReferences::new(&f.store, "p");
        let mut tx = c.transaction().unwrap();
        let original = refs.store_prompt(&mut tx, "run", "original", 1).unwrap();
        assert_eq!(
            refs.store_attachment(&mut tx, "task", "a", b"original", 1)
                .unwrap(),
            original
        );
        let candidate = f.store.put(b"candidate").unwrap();
        tx.execute(
            "INSERT INTO blobs VALUES ('p',?1,?2,?3,2,3)",
            rusqlite::params![candidate.to_string(), size, location],
        )
        .unwrap();
        assert!(matches!(
            refs.store_prompt(&mut tx, "run", "candidate", 4),
            Err(ReferenceError::Metadata)
        ));
        assert!(matches!(
            refs.store_attachment(&mut tx, "task", "a", b"candidate", 4),
            Err(ReferenceError::Metadata)
        ));
        tx.commit().unwrap();
        assert_eq!(count(&c, "blobs"), 2);
        assert_eq!(count(&c, "task_attachments"), 1);
        let metadata: (i64, String, i64, Option<i64>) = c
            .query_row(
                "SELECT size,storage_location,created_at,unreferenced_since FROM blobs WHERE project_id='p' AND sha256=?1",
                [candidate.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(metadata, (size, location.into(), 2, Some(3)));
        assert_eq!(
            refs.load_prompt(&c, "run").unwrap(),
            Some("original".into())
        );
        assert_eq!(
            refs.load_attachment(&c, "task", "a").unwrap(),
            Some(b"original".to_vec())
        );
    }
}

#[test]
fn run_prompt_lookup_and_update_distinguish_literal_and_digest_ids() {
    use orch_infra::blob::BlobReferences;
    let f = fixture();
    let mut c = database(&f);
    // Both native keys contain the same bytes in one project. Only the form differs.
    // Count UTF-8 bytes: this ID has fewer than 65,536 characters.
    let indirect = "é".repeat(32_769);
    let literal = BlobHash::of(indirect.as_bytes()).to_string();
    for project in ["p", "other"] {
        for form in [0, 1] {
            c.execute(
                "INSERT INTO runs(project_id,id,id_form,id_sha256,id_bytes,task_id,agent_id,status,revision)
                 VALUES(?1,?2,?3,?4,?5,'task','agent','running',1)",
                rusqlite::params![project,literal,form,
                    (form == 1).then_some(&literal),
                    (form == 1).then_some(indirect.len() as i64)],
            ).unwrap();
        }
    }
    assert_eq!(count(&c, "runs"), 8);
    let refs = BlobReferences::new(&f.store, "p");
    let other = BlobReferences::new(&f.store, "other");
    let mut tx = c.transaction().unwrap();
    refs.store_prompt(&mut tx, &literal, "literal prompt", 1)
        .unwrap();
    assert_eq!(
        refs.load_prompt(&tx, &literal).unwrap().as_deref(),
        Some("literal prompt")
    );
    assert_eq!(refs.load_prompt(&tx, &indirect).unwrap(), None);
    let digest_link: Option<String> = tx
        .query_row(
            "SELECT prompt_sha256 FROM runs WHERE project_id='p' AND id_form=1 AND id=?1",
            [&literal],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(digest_link, None);
    refs.store_prompt(&mut tx, &indirect, "indirect prompt", 2)
        .unwrap();
    assert_eq!(
        refs.load_prompt(&tx, &literal).unwrap().as_deref(),
        Some("literal prompt")
    );
    assert_eq!(
        refs.load_prompt(&tx, &indirect).unwrap().as_deref(),
        Some("indirect prompt")
    );
    assert_eq!(other.load_prompt(&tx, &literal).unwrap(), None);
    assert_eq!(other.load_prompt(&tx, &indirect).unwrap(), None);
    tx.commit().unwrap();
    assert_eq!(count(&c, "runs"), 8);
    assert_eq!(count(&c, "blobs"), 2);

    // An absent form must not fall back to its other-form sibling. A caught
    // refusal creates no registration and leaves the sibling link intact.
    c.execute(
        "DELETE FROM runs WHERE project_id='p' AND id_form=1 AND id=?1",
        [&literal],
    )
    .unwrap();
    let mut tx = c.transaction().unwrap();
    assert!(matches!(
        refs.store_prompt(&mut tx, &indirect, "missing form", 3),
        Err(orch_infra::blob::ReferenceError::MissingOwner { kind: "run" })
    ));
    assert_eq!(refs.load_prompt(&tx, &indirect).unwrap(), None);
    assert_eq!(
        refs.load_prompt(&tx, &literal).unwrap().as_deref(),
        Some("literal prompt")
    );
    tx.commit().unwrap();
    assert_eq!(count(&c, "runs"), 7);
    assert_eq!(count(&c, "blobs"), 2);
}
