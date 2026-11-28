//! Stored-data errors, publication failures and caught failures.
use super::*;

const BIG: usize = 70_000;

fn insert(rig: &Rig, task: &str, attempt: i64, due: i64, error: Option<&str>) {
    rig.conn
        .execute(
            "INSERT INTO retry_queue (project_id,task_id,attempt,due_at,error) VALUES ('p',?1,?2,?3,?4)",
            params![task, attempt, due, error],
        )
        .unwrap();
}

fn edit(rig: &Rig, task: &str, change: impl FnOnce(&mut serde_json::Value)) {
    let mut document: serde_json::Value = serde_json::from_str(&rig.stored("p", task)).unwrap();
    change(&mut document);
    rig.set_stored("p", task, &document.to_string());
}

#[test]
fn rows_that_no_write_makes_are_stored_data_errors() {
    let rig = rig();
    let inline = r#"{"v":1,"inline":"ok"}"#;
    let over = format!(r#"{{"v":1,"inline":"{}"}}"#, "a".repeat(65_537));
    let small_blob = r#"{"v":1,"blob":{"sha256":"00","bytes":5,"chunks":[]}}"#;
    let cases: Vec<(&str, i64, i64, Option<String>, &str)> = vec![
        ("zero", 0, 1, Some(inline.into()), "attempt"),
        ("negative", -1, 1, Some(inline.into()), "attempt"),
        ("wide", 4_294_967_296, 1, Some(inline.into()), "attempt"),
        ("early", 1, -1, Some(inline.into()), "deadline"),
        ("null", 1, 1, None, "missing diagnostic"),
        ("empty-task", 1, 1, Some(inline.into()), "task id"),
        (
            "garbage",
            1,
            1,
            Some("plain text".into()),
            "document syntax",
        ),
        (
            "version",
            1,
            1,
            Some(r#"{"v":2,"inline":"x"}"#.into()),
            "format version",
        ),
        (
            "neither",
            1,
            1,
            Some(r#"{"v":1}"#.into()),
            "document members",
        ),
        (
            "both",
            1,
            1,
            Some(r#"{"v":1,"inline":"x","blob":{"sha256":"00","bytes":1,"chunks":[]}}"#.into()),
            "document members",
        ),
        (
            "unknown",
            1,
            1,
            Some(r#"{"v":1,"inline":"x","extra":1}"#.into()),
            "document syntax",
        ),
        ("over", 1, 1, Some(over), "inline size"),
        ("small", 1, 1, Some(small_blob.into()), "whole hash syntax"),
        (
            "huge",
            1,
            1,
            Some(format!(
                r#"{{"v":1,"inline":"{}"}}"#,
                "a".repeat(4 * 1024 * 1024)
            )),
            "document size",
        ),
    ];
    for (task, attempt, due, error, _) in &cases {
        let task = if *task == "empty-task" { "" } else { task };
        insert(&rig, task, *attempt, *due, error.as_deref());
    }
    for (task, _, _, _, detail) in &cases {
        let task = if *task == "empty-task" { "" } else { task };
        is_corrupt(rig.reader("p").read_retry(task), detail);
    }
}

#[test]
fn a_damaged_row_fails_every_read_that_returns_it_and_only_those() {
    let mut rig = rig();
    rig.put("p", &retry("a", 1, 10, "fine"));
    rig.put("p", &retry("z", 1, 20, "fine"));
    insert(&rig, "m", 0, 15, Some(r#"{"v":1,"inline":"x"}"#));
    let reader = rig.reader("p");
    is_corrupt(reader.read_retry("m"), "attempt");
    is_corrupt(reader.read_due_retries(15, 10), "attempt");
    is_corrupt(reader.read_retries(None, 10), "attempt");
    is_corrupt(reader.named_objects(), "attempt");
    // A row that the deadline or the limit excludes is never resolved.
    assert_eq!(reader.read_due_retries(14, 10).unwrap().len(), 1);
    let first = reader.read_retries(None, 1).unwrap();
    assert_eq!(first.rows[0].task, "a");
    is_corrupt(reader.read_retries(first.next.as_ref(), 1), "attempt");
    assert!(reader.read_retry("z").is_ok());
}

#[test]
fn a_missing_corrupt_short_or_unregistered_object_is_never_a_partial_text() {
    let mut rig = rig();
    let text = text_of(BIG);
    let hash = BlobHash::of(text.as_bytes()).to_string();
    rig.put("p", &retry("t", 1, 1, &text));
    assert_eq!(
        rig.reader("p").read_retry("t").unwrap().unwrap().error,
        text
    );
    let path = rig.object_path(&hash);
    let good = std::fs::read(&path).unwrap();
    for (name, bytes) in [
        (
            "different bytes",
            text_of(BIG - 1 + 1).replace('a', "b").into_bytes(),
        ),
        ("short bytes", good[..BIG - 1].to_vec()),
        ("empty", Vec::new()),
    ] {
        std::fs::write(&path, bytes).unwrap();
        is_corrupt(rig.reader("p").read_retry("t"), "chunk object");
        is_corrupt(rig.reader("p").read_due_retries(1, 5), "chunk object");
        is_corrupt(rig.reader("p").read_retries(None, 5), "chunk object");
        eprintln!("{name} refused");
    }
    std::fs::remove_file(&path).unwrap();
    is_corrupt(rig.reader("p").read_retry("t"), "chunk object");
    std::fs::write(&path, &good).unwrap();
    assert_eq!(
        rig.reader("p").read_retry("t").unwrap().unwrap().error,
        text
    );

    // Registration damage is found before any object is opened, even while the
    // object itself is unreadable.
    let registered = |sql: &str| {
        rig.conn.execute(sql, []).unwrap();
    };
    registered("UPDATE blobs SET size=size+1");
    is_corrupt(rig.reader("p").read_retry("t"), "blob registration");
    registered("UPDATE blobs SET size=size-1, storage_location='elsewhere'");
    is_corrupt(rig.reader("p").read_retry("t"), "blob registration");
    registered("UPDATE blobs SET storage_location='disk'");
    // A name that is registered at the manifest's size but holds other bytes is
    // refused by length, not read as a shorter text.
    let other = text_of(5);
    let other_hash = rig.blobs.put(other.as_bytes()).unwrap().to_string();
    rig.conn
        .execute(
            "INSERT INTO blobs (project_id,sha256,size,storage_location,created_at)
             VALUES ('p',?1,?2,'disk',0)",
            params![other_hash, BIG as i64],
        )
        .unwrap();
    let stored = rig.stored("p", "t");
    edit(&rig, "t", |d| {
        d["blob"]["chunks"][0]["sha256"] = other_hash.clone().into()
    });
    is_corrupt(rig.reader("p").read_retry("t"), "chunk object size");
    rig.set_stored("p", "t", &stored);
    std::fs::remove_file(&path).unwrap();
    registered("DELETE FROM blobs");
    is_corrupt(rig.reader("p").read_retry("t"), "blob registration");
}

#[test]
fn manifest_damage_is_a_stored_data_error() {
    let mut rig = rig();
    let text = text_of(BIG);
    rig.put("p", &retry("t", 1, 1, &text));
    let good = rig.stored("p", "t");
    type Change = Box<dyn Fn(&mut serde_json::Value)>;
    let cases: Vec<(&str, Change, &str)> = vec![
        (
            "whole hash",
            Box::new(|d| d["blob"]["sha256"] = "0".repeat(64).into()),
            "whole length or hash",
        ),
        (
            "whole hash syntax",
            Box::new(|d| d["blob"]["sha256"] = "XYZ".into()),
            "whole hash syntax",
        ),
        (
            "length",
            Box::new(|d| d["blob"]["bytes"] = (BIG as u64 + 1).into()),
            "chunk size",
        ),
        (
            "small length",
            Box::new(|d| d["blob"]["bytes"] = 100.into()),
            "indirect size",
        ),
        (
            "no chunks",
            Box::new(|d| d["blob"]["chunks"] = serde_json::json!([])),
            "chunk count",
        ),
        (
            "extra chunk",
            Box::new(|d| {
                let chunk = d["blob"]["chunks"][0].clone();
                d["blob"]["chunks"].as_array_mut().unwrap().push(chunk)
            }),
            "chunk count",
        ),
        (
            "chunk size",
            Box::new(|d| d["blob"]["chunks"][0]["size"] = 5.into()),
            "chunk size",
        ),
        (
            "chunk hash",
            Box::new(|d| d["blob"]["chunks"][0]["sha256"] = "q".into()),
            "chunk hash syntax",
        ),
        (
            "other chunk hash",
            Box::new(|d| d["blob"]["chunks"][0]["sha256"] = "1".repeat(64).into()),
            "blob registration",
        ),
        (
            "unknown member",
            Box::new(|d| d["blob"]["extra"] = 1.into()),
            "document syntax",
        ),
    ];
    for (name, change, detail) in cases {
        edit(&rig, "t", change);
        let reader = rig.reader("p");
        is_corrupt(reader.read_retry("t"), detail);
        is_corrupt(reader.read_retries(None, 1), detail);
        rig.set_stored("p", "t", &good);
        eprintln!("{name} refused");
    }
    assert_eq!(
        rig.reader("p").read_retry("t").unwrap().unwrap().error,
        text
    );
}

#[test]
fn an_object_that_cannot_be_read_is_not_stored_data_damage() {
    let mut rig = rig();
    rig.put("p", &retry("t", 1, 1, &text_of(BIG)));
    let unreadable = Faulty {
        inner: &rig.blobs,
        puts_allowed: Cell::new(usize::MAX),
        corrupt_reads: false,
        unreadable: true,
    };
    let reader = RetryReader::with_objects(&rig.conn, "p", &unreadable);
    assert!(matches!(
        reader.read_retry("t"),
        Err(SqliteError::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied
    ));
}

#[test]
fn a_failed_publication_or_failed_verification_changes_nothing() {
    let mut rig = rig();
    rig.put("p", &retry("t", 2, 9, "before"));
    let who = caller("p");
    let new = retry("t", 1, 1, &text_of(BIG));
    for (puts, corrupt_reads) in [(0, false), (1, true)] {
        let faulty = Faulty {
            inner: &rig.blobs,
            puts_allowed: Cell::new(puts),
            corrupt_reads,
            unreadable: false,
        };
        let tx = rig.conn.transaction().unwrap();
        let result = Retries::with_objects(&tx, &who, &faulty).put_retry(&new, 0);
        if corrupt_reads {
            is_corrupt(result, "published bytes");
        } else {
            assert!(matches!(result, Err(SqliteError::Io(_))), "{result:?}");
        }
        tx.commit().unwrap();
        assert_eq!(
            rig.reader("p").read_retry("t").unwrap(),
            Some(retry("t", 2, 9, "before"))
        );
        assert!(rig.registrations("p").is_empty());
    }
    // The bytes that did reach the disk are unreferenced, which the sweep removes.
    assert_eq!(rig.objects().len(), 1);
    rig.put("p", &new);
    assert_eq!(rig.reader("p").read_retry("t").unwrap(), Some(new));
    assert_eq!(rig.objects().len(), 1);
}

#[test]
fn a_row_failure_after_publication_rolls_back_the_registrations_even_when_caught() {
    let mut rig = rig();
    rig.put("p", &retry("t", 2, 9, "before"));
    rig.conn
        .execute_batch(
            "CREATE TRIGGER refuse BEFORE INSERT ON retry_queue
             BEGIN SELECT RAISE(ABORT,'refused'); END;",
        )
        .unwrap();
    let who = caller("p");
    let tx = rig.conn.transaction().unwrap();
    // A caller that catches the refusal and commits other work.
    let result = Retries::new(&tx, &who, &rig.blobs).put_retry(&retry("t", 1, 1, &text_of(BIG)), 0);
    assert!(matches!(result, Err(SqliteError::Sql(_))), "{result:?}");
    tx.execute(
        "INSERT INTO counters (project_id,name,value) VALUES ('p','other',1)",
        [],
    )
    .unwrap();
    tx.commit().unwrap();
    rig.conn.execute_batch("DROP TRIGGER refuse").unwrap();
    assert!(rig.registrations("p").is_empty());
    assert_eq!(
        rig.reader("p").read_retry("t").unwrap(),
        Some(retry("t", 2, 9, "before"))
    );
    assert_eq!(rig.objects().len(), 1);
}

#[test]
fn a_rolled_back_transaction_leaves_published_bytes_unreferenced() {
    let mut rig = rig();
    let who = caller("p");
    let tx = rig.conn.transaction().unwrap();
    Retries::new(&tx, &who, &rig.blobs)
        .put_retry(&retry("t", 1, 1, &text_of(BIG)), 0)
        .unwrap();
    tx.rollback().unwrap();
    assert!(rig.reader("p").read_retry("t").unwrap().is_none());
    assert!(rig.registrations("p").is_empty());
    assert_eq!(rig.objects().len(), 1);
}

#[test]
fn an_existing_registration_that_disagrees_refuses_the_write() {
    let mut rig = rig();
    let text = text_of(BIG);
    let hash = BlobHash::of(text.as_bytes()).to_string();
    rig.conn
        .execute(
            "INSERT INTO blobs (project_id,sha256,size,storage_location,created_at) VALUES ('p',?1,5,'disk',0)",
            [&hash],
        )
        .unwrap();
    let who = caller("p");
    let tx = rig.conn.transaction().unwrap();
    let result = Retries::new(&tx, &who, &rig.blobs).put_retry(&retry("t", 1, 1, &text), 0);
    is_corrupt(result, "blob registration");
    tx.commit().unwrap();
    assert!(rig.reader("p").read_retry("t").unwrap().is_none());
    let _ = &mut rig;
}

// Catches interleaving parse/reconstruct: later structural damage must win over
// an earlier access failure, independently of the ordering of the two rows.
#[test]
fn mixed_rows_validate_all_metadata_before_opening_any_object() {
    let mut rig = rig();
    rig.put("p", &retry("object", 1, 10, &text_of(BIG)));
    insert(&rig, "damaged", 0, 20, Some(r#"{"v":1,"inline":"ok"}"#));
    let unreadable = Faulty {
        inner: &rig.blobs,
        puts_allowed: Cell::new(usize::MAX),
        corrupt_reads: false,
        unreadable: true,
    };
    let reader = RetryReader::with_objects(&rig.conn, "p", &unreadable);
    assert!(matches!(
        reader.read_retry("object"),
        Err(SqliteError::Io(_))
    ));
    for due in [20, 0] {
        rig.conn
            .execute(
                "UPDATE retry_queue SET due_at=?1 WHERE task_id='damaged'",
                [due],
            )
            .unwrap();
        is_corrupt(reader.read_due_retries(20, 2), "attempt");
        is_corrupt(reader.read_retries(None, 2), "attempt");
    }
}

// Catches a registration lookup by hash alone: a healthy shared disk object and
// another project's matching registration do not repair this project's row.
#[test]
fn another_projects_registration_cannot_authorize_retry_reads() {
    let mut rig = rig();
    let value = retry("t", 1, 10, &text_of(BIG));
    rig.put("p", &value);
    rig.put("other", &value);
    rig.conn
        .execute("DELETE FROM blobs WHERE project_id='p'", [])
        .unwrap();
    assert_eq!(rig.reader("other").read_retry("t").unwrap(), Some(value));
    let reader = rig.reader("p");
    is_corrupt(reader.read_retry("t"), "blob registration");
    is_corrupt(reader.read_due_retries(10, 2), "blob registration");
    is_corrupt(reader.read_retries(None, 2), "blob registration");
    is_corrupt(reader.named_objects(), "blob registration");
}

// Invalid UTF-8 TEXT distinguishes the SQL projection from the parser's same
// size error: conversion to Rust String fails if the SQL guard is removed.
#[test]
fn sql_size_guard_excludes_oversized_text_before_rust_conversion() {
    let rig = rig();
    insert(&rig, "t", 1, 10, Some(""));
    let reader = rig.reader("p");
    let after = RetryCursor {
        due_at: 0,
        task: "a".into(),
    };
    for size in [
        super::super::text::MAX_STORED_BYTES,
        super::super::text::MAX_STORED_BYTES + 1,
    ] {
        rig.conn
            .execute(
                "UPDATE retry_queue SET error=CAST(x'80' || zeroblob(?1) AS TEXT)",
                [size as i64 - 1],
            )
            .unwrap();
        if size == super::super::text::MAX_STORED_BYTES {
            // At the admitted boundary the invalid byte reaches row conversion.
            assert!(matches!(reader.read_retry("t"), Err(SqliteError::Sql(_))));
            assert!(matches!(
                reader.read_due_retries(10, 1),
                Err(SqliteError::Sql(_))
            ));
            assert!(matches!(
                reader.read_retries(None, 1),
                Err(SqliteError::Sql(_))
            ));
            assert!(matches!(
                reader.read_retries(Some(&after), 1),
                Err(SqliteError::Sql(_))
            ));
        } else {
            is_corrupt(reader.read_retry("t"), "document size");
            is_corrupt(reader.read_due_retries(10, 1), "document size");
            is_corrupt(reader.read_retries(None, 1), "document size");
            is_corrupt(reader.read_retries(Some(&after), 1), "document size");
            is_corrupt(reader.named_objects(), "document size");
        }
    }
}
