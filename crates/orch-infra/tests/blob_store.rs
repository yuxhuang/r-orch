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
