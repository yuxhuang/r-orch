//! Physical R97 proofs above one object: 64 MiB, the split, repeats and order.
use super::*;
use crate::blob::MAX_BLOB_BYTES;

const MAX: usize = MAX_BLOB_BYTES as usize;

fn manifest(rig: &Rig, task: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(&rig.stored("p", task)).unwrap()["blob"].clone()
}

fn edit_chunks(rig: &Rig, task: &str, change: impl FnOnce(&mut Vec<serde_json::Value>)) {
    let mut document: serde_json::Value = serde_json::from_str(&rig.stored("p", task)).unwrap();
    change(document["blob"]["chunks"].as_array_mut().unwrap());
    rig.set_stored("p", task, &document.to_string());
}

#[test]
fn text_of_exactly_64_mib_is_one_object_named_by_the_whole_hash() {
    let mut rig = rig();
    let text = text_of(MAX);
    rig.put("p", &retry("one", 1, 1, &text));
    let hash = BlobHash::of(text.as_bytes()).to_string();
    assert_eq!(rig.objects(), std::slice::from_ref(&hash));
    let doc = manifest(&rig, "one");
    assert_eq!(doc["sha256"], hash.as_str());
    assert_eq!(doc["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(rig.registrations("p"), [(hash, MAX as i64)]);
    assert!(rig.reader("p").read_retry("one").unwrap().unwrap().error == text);
}

#[test]
fn a_split_value_names_no_object_and_a_failure_on_a_later_chunk_names_nothing() {
    let mut rig = rig();
    // The euro sign is three bytes and starts one byte before the boundary.
    let text = format!("{}€", text_of(MAX - 1));
    assert_eq!(text.len(), MAX + 2);
    let who = caller("p");
    let faulty = Faulty {
        inner: &rig.blobs,
        puts_allowed: Cell::new(1),
        corrupt_reads: false,
        unreadable: false,
    };
    let tx = rig.conn.transaction().unwrap();
    let result = Retries::with_objects(&tx, &who, &faulty).put_retry(&retry("t", 1, 1, &text), 0);
    assert!(matches!(result, Err(SqliteError::Io(_))), "{result:?}");
    tx.commit().unwrap();
    assert!(rig.reader("p").read_retry("t").unwrap().is_none());
    assert!(rig.registrations("p").is_empty());
    // The first object reached the disk and nothing names it.
    assert_eq!(rig.objects().len(), 1);

    // A retry reuses it and completes.
    rig.put("p", &retry("t", 1, 1, &text));
    let doc = manifest(&rig, "t");
    assert_eq!(doc["bytes"], MAX as u64 + 2);
    assert_eq!(doc["chunks"][0]["size"], MAX as u64);
    assert_eq!(doc["chunks"][1]["size"], 2);
    let whole = BlobHash::of(text.as_bytes()).to_string();
    assert_eq!(doc["sha256"], whole.as_str());
    assert!(!rig.object_path(&whole).exists());
    assert_eq!(rig.objects().len(), 2);
    assert_eq!(rig.registrations("p").len(), 2);
    // No object is valid UTF-8 by itself: only the joined bytes decode.
    let first =
        std::fs::read(rig.object_path(doc["chunks"][0]["sha256"].as_str().unwrap())).unwrap();
    assert!(std::str::from_utf8(&first).is_err());
    assert!(rig.reader("p").read_retry("t").unwrap().unwrap().error == text);
}

#[test]
fn repeated_chunks_keep_every_position_and_a_reorder_or_a_lost_one_is_refused() {
    let mut rig = rig();
    let a = text_of(MAX);
    let text = format!("{a}{a}z");
    rig.put("p", &retry("t", 1, 1, &text));
    // Positions 0 and 1 are one object; position 2 is another.
    let named = rig.reader("p").named_objects().unwrap();
    assert_eq!(
        named.iter().map(|n| n.ordinal).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(named[0].sha256, named[1].sha256);
    assert_ne!(named[0].sha256, named[2].sha256);
    assert_eq!(
        named.iter().map(|n| n.size).collect::<Vec<_>>(),
        [MAX as u64, MAX as u64, 1]
    );
    assert!(named.iter().all(|n| n.task == "t"));
    assert!(
        named
            .iter()
            .all(|n| n.whole_sha256 == BlobHash::of(text.as_bytes()).to_string())
    );
    assert_eq!(rig.objects().len(), 2);
    assert_eq!(rig.registrations("p").len(), 2);
    assert!(rig.reader("p").read_retry("t").unwrap().unwrap().error == text);
    drop(text);

    let good = rig.stored("p", "t");
    // Dropping one occurrence is a count error: the objects alone cannot tell.
    edit_chunks(&rig, "t", |chunks| {
        chunks.remove(1);
    });
    is_corrupt(rig.reader("p").read_retry("t"), "chunk count");
    rig.set_stored("p", "t", &good);
    // A reordered manifest is refused before any object is opened.
    edit_chunks(&rig, "t", |chunks| chunks.swap(1, 2));
    is_corrupt(rig.reader("p").read_retry("t"), "chunk size");
    rig.set_stored("p", "t", &good);
    // A damaged and then a missing later object are damage, never truncation.
    let later = manifest(&rig, "t")["chunks"][2]["sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(rig.object_path(&later), b"x").unwrap();
    is_corrupt(rig.reader("p").read_retry("t"), "chunk object");
    std::fs::remove_file(rig.object_path(&later)).unwrap();
    is_corrupt(rig.reader("p").read_retry("t"), "chunk object");
}
