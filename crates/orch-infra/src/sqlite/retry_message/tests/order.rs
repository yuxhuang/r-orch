//! Read order, paging and the independent due statement.
use super::*;

fn ids(rows: &[Retry]) -> Vec<&str> {
    rows.iter().map(|r| r.task.as_str()).collect()
}

#[test]
fn due_rows_are_earliest_first_with_task_ids_compared_as_bytes_and_equality_is_due() {
    let mut rig = rig();
    for (task, due) in [
        ("b", 20),
        ("é", 10),
        ("a", 10),
        ("B", 10),
        ("late", 21),
        ("zero", 0),
        ("max", i64::MAX),
    ] {
        rig.put("p", &retry(task, 1, due, task));
    }
    let reader = rig.reader("p");
    // "B" < "a" < "é" as bytes, and a row due exactly now is due.
    assert_eq!(
        ids(&reader.read_due_retries(20, 10).unwrap()),
        ["zero", "B", "a", "é", "b"]
    );
    assert_eq!(
        ids(&reader.read_due_retries(19, 10).unwrap()),
        ["zero", "B", "a", "é"]
    );
    assert_eq!(ids(&reader.read_due_retries(0, 10).unwrap()), ["zero"]);
    assert!(reader.read_due_retries(-1, 10).unwrap().is_empty());
    assert_eq!(reader.read_due_retries(i64::MAX, 10).unwrap().len(), 7);
    assert_eq!(
        ids(&reader.read_due_retries(i64::MAX, 2).unwrap()),
        ["zero", "B"]
    );
    let all = reader.read_retries(None, 10).unwrap();
    assert_eq!(ids(&all.rows), ["zero", "B", "a", "é", "b", "late", "max"]);
    assert!(all.next.is_none());
}

#[test]
fn pages_end_with_a_cursor_only_when_a_row_follows_and_survive_a_vanished_row() {
    let mut rig = rig();
    for n in 0..6 {
        rig.put("p", &retry(&format!("t{n}"), 1, 100 + (n / 2), "e"));
    }
    let reader = rig.reader("p");
    let first = reader.read_retries(None, 3).unwrap();
    assert_eq!(ids(&first.rows), ["t0", "t1", "t2"]);
    let cursor = first.next.unwrap();
    assert_eq!(cursor, RetryCursor::after(&first.rows[2]));
    let second = reader.read_retries(Some(&cursor), 3).unwrap();
    assert_eq!(ids(&second.rows), ["t3", "t4", "t5"]);
    // An exact multiple of the limit ends with `None`, not an empty page.
    assert!(second.next.is_none());
    // A cursor is a key, not a row: it holds after its row is deleted.
    rig.commit("p", |r| r.delete_retry("t2").unwrap());
    let again = rig.reader("p").read_retries(Some(&cursor), 10).unwrap();
    assert_eq!(ids(&again.rows), ["t3", "t4", "t5"]);
    let none = rig
        .reader("p")
        .read_retries(Some(&RetryCursor::after(&second.rows[2])), 10)
        .unwrap();
    assert!(none.rows.is_empty() && none.next.is_none());
}

/// The statement behind the due read is proved on its own: the expected rows
/// are computed here, in Rust, and the complete walk is a different statement.
#[test]
fn due_read_at_i64_max_returns_the_first_thousand_after_a_complete_restore() {
    let mut rig = rig();
    let total = 2_501usize;
    let mut expected = Vec::new();
    for n in 0..total {
        // Many ties and a spread of deadlines, inserted out of order.
        let due = match n {
            0 => i64::MAX,
            1 => 0,
            n => ((n * 7919) % 97) as i64 * 1_000,
        };
        let task = format!("task-{:05}", (n * 31) % total);
        let row = retry(&task, (n % 9) as u32 + 1, due, &format!("diagnostic {n}"));
        expected.push(row.clone());
        rig.put("p", &row);
    }
    let mut want = expected;
    want.sort_by(|a, b| (a.due_at, a.task.as_bytes()).cmp(&(b.due_at, b.task.as_bytes())));
    assert_eq!(want.len(), total);
    // Restore: a copy of the whole database is what the readers see.
    let restored = rig.temp.path().join("restored.db");
    rig.conn
        .execute("VACUUM INTO ?1", [restored.to_str().unwrap()])
        .unwrap();
    let copy = Connection::open(&restored).unwrap();
    let reader = RetryReader::new(&copy, "p", &rig.blobs);
    // The complete walk is read by pages of a thousand and by the cursor.
    let mut walked = Vec::new();
    let mut cursor = None;
    loop {
        let page = reader.read_retries(cursor.as_ref(), 1_000).unwrap();
        walked.extend(page.rows);
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(walked, want);
    let due = reader.read_due_retries(i64::MAX, 1_000).unwrap();
    assert_eq!(due.len(), 1_000);
    assert_eq!(due, want[..1_000]);
    assert_eq!(want.last().unwrap().due_at, i64::MAX);
    // A smaller `now` cuts the same statement at the deadline, ties included.
    let cut = reader.read_due_retries(40_000, 1_000).unwrap();
    let cut_want: Vec<_> = want
        .iter()
        .filter(|r| r.due_at <= 40_000)
        .take(1_000)
        .cloned()
        .collect();
    assert_eq!(cut, cut_want);
}

#[test]
fn a_page_walk_inside_one_read_transaction_ignores_later_commits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snap.db");
    let writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.execute_batch(migrations::INITIAL_SCHEMA).unwrap();
    let (_temp, data) = test_data();
    let blobs = DiskBlobStore::open(data.root()).unwrap();
    let who = caller("p");
    let mut writer = writer;
    let put = |writer: &mut Connection, task: &str, due: i64| {
        let tx = writer.transaction().unwrap();
        Retries::new(&tx, &who, &blobs)
            .put_retry(&retry(task, 1, due, "e"), 0)
            .unwrap();
        tx.commit().unwrap();
    };
    for n in 0..4 {
        put(&mut writer, &format!("t{n}"), n);
    }
    let mut read = Connection::open(&path).unwrap();
    let snapshot = read.transaction().unwrap();
    let reader = RetryReader::new(&snapshot, "p", &blobs);
    let first = reader.read_retries(None, 2).unwrap();
    // A commit between the pages: an insert before the cursor, a delete after it.
    put(&mut writer, "new", 0);
    writer
        .execute("DELETE FROM retry_queue WHERE task_id='t3'", [])
        .unwrap();
    let second = reader.read_retries(first.next.as_ref(), 10).unwrap();
    assert_eq!(ids(&first.rows), ["t0", "t1"]);
    assert_eq!(ids(&second.rows), ["t2", "t3"]);
}
