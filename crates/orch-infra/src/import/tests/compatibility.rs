//! Ordinary synthetic inputs compared with the real 1.0 readers; no importer caller.
use super::super::{
    document::*,
    event::{EventItem, EventStream},
    index::{IndexProblem, reconcile},
};
use crate::storage::index::ItemFormat;
use crate::storage::{FileConfigStore, FileRunStore, IndexManager, Paths};
use orch_core::storage::RunStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, Read};

#[tokio::test]
async fn effective_config_defaults_and_r109_document_count_are_distinct() {
    let defaults = serde_json::to_value(orch_domain::default_config()).unwrap();
    for (name, input, documents) in [
        ("missing", None, None),
        ("empty", Some(""), Some(0)),
        ("whitespace", Some(" \n  \n"), Some(0)),
        ("comment", Some("# synthetic config\n"), Some(0)),
        ("null", Some("null\n"), Some(1)),
        ("mapping", Some("{}\n"), Some(1)),
        ("multiple", Some("{}\n---\n{}\n"), Some(2)),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        std::fs::create_dir_all(paths.root()).unwrap();
        if let Some(input) = input {
            std::fs::write(paths.config_path(), input).unwrap();
        }
        let legacy = FileConfigStore::new(paths.clone()).read_merged().await;
        if documents == Some(2) {
            assert!(legacy.is_err(), "{name}");
        } else {
            assert_eq!(legacy.unwrap(), defaults, "{name}");
        }
        if let Some(input) = input {
            let checked = check_yaml(input.as_bytes(), Limits::YAML);
            match documents.unwrap() {
                1 => {
                    checked.unwrap();
                    let _: Value = parse_yaml(input.as_bytes(), Limits::YAML).await.unwrap();
                }
                count => {
                    let refusal = checked.unwrap_err();
                    assert_eq!(refusal.rule, "documents", "{name}");
                    assert_eq!(refusal.observed, count);
                }
            }
            assert_eq!(
                std::fs::read(paths.config_path()).unwrap(),
                input.as_bytes()
            );
        } else {
            assert!(!paths.config_path().exists());
        }
    }
    // A syntactically valid null document is not a valid typed entity.
    assert_eq!(
        parse_yaml::<orch_domain::task::Task>(&b"null"[..], Limits::YAML)
            .await
            .unwrap_err()
            .rule,
        "type"
    );
}

#[tokio::test]
async fn physical_blank_lines_form_gaps_while_legacy_skips_unicode_whitespace() {
    let first = include_bytes!(
        "../../../../../fixtures/storage/generated/small/.orchestry/runs/run_FkMwTnz.jsonl"
    )
    .split(|b| *b == b'\n')
    .find(|line| !line.is_empty())
    .unwrap();
    for blank in ["", " \t", "\u{2003}", "\u{00a0}"] {
        let event = std::str::from_utf8(first).unwrap();
        // CRLF, two consecutive blanks, a malformed line, final newline.
        let input = format!("{event}\r\n{blank}\r\n{blank}\r\ninvalid\r\n{event}\r\n");
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        let path = paths.run_events_path("run_synthetic").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &input).unwrap();
        let legacy = FileRunStore::new(paths)
            .read_events("run_synthetic")
            .await
            .unwrap();
        assert_eq!(legacy.len(), 2);
        let rows =
            EventStream::<_, orch_domain::run::RunEvent>::new(input.as_bytes(), Limits::EVENT)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], EventItem::Event { line: 1, .. }));
        assert!(matches!(rows[1], EventItem::Gap { first: 2, last: 4 }));
        assert!(matches!(rows[2], EventItem::Event { line: 5, .. }));
        assert_eq!(std::fs::read(path).unwrap(), input.as_bytes());
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Entry {
    id: String,
}

#[tokio::test]
async fn index_damage_distinguishes_unreadable_streams_from_bad_array_entries() {
    for (name, input, refusal, legacy_duplicates) in [
        ("missing", None, false, false),
        ("syntax", Some("["), false, false),
        ("object", Some("{}"), false, false),
        ("null", Some("null"), false, false),
        ("non_object", Some("[0]"), true, false),
        ("missing_identity", Some("[{}]"), true, false),
        ("numeric_identity", Some("[{\"id\":1}]"), true, false),
        (
            "duplicate",
            Some("[{\"id\":\"b\"},{\"id\":\"b\"}]"),
            true,
            true,
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        for id in ["b", "a", "c"] {
            std::fs::write(dir.join(format!("{id}.yml")), format!("id: {id}\n")).unwrap();
        }
        let path = dir.join("_index.json");
        if let Some(input) = input {
            std::fs::write(&path, input).unwrap();
        }
        let before = std::fs::read(&path).ok();
        let parsed = input.map(|text| parse_json::<Value>(text.as_bytes(), Limits::JSON));
        // Only malformed JSON maps to unreadable. Budget and I/O refusals must propagate.
        let index = match parsed {
            Some(Ok(value)) => Some(value),
            Some(Err(error)) => {
                assert_eq!(error.rule, "parse");
                None
            }
            None => None,
        };
        let files = ["a", "b", "c"]
            .map(|id| (id.to_owned(), json!({"id":id})))
            .into();
        let result = reconcile(index.as_ref(), files, "id");
        if refusal {
            assert_eq!(result.unwrap_err().rule, "index_entry", "{name}");
        } else {
            let (rows, problems) = result.unwrap();
            assert_eq!(
                rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
                ["a", "b", "c"]
            );
            assert_eq!(problems, [IndexProblem::Unreadable]);
        }
        assert_eq!(std::fs::read(&path).ok(), before);
        // Real typed legacy reader on the disposable copy. It may rebuild this index.
        let legacy = IndexManager::<Entry>::new(dir.to_path_buf(), ItemFormat::Yaml)
            .read_index()
            .await
            .unwrap();
        let mut ids: Vec<_> = legacy.iter().map(|item| item.id.as_str()).collect();
        ids.sort();
        assert_eq!(
            ids,
            if legacy_duplicates {
                vec!["b", "b"]
            } else {
                vec!["a", "b", "c"]
            },
            "{name}"
        );
    }
}

struct FaultOnce {
    bytes: &'static [u8],
    offset: usize,
    kind: io::ErrorKind,
    fired: bool,
}
impl Read for FaultOnce {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.offset == 2 && !self.fired {
            self.fired = true;
            return Err(self.kind.into());
        }
        let count = output.len().min(2).min(self.bytes.len() - self.offset);
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}
fn fault(kind: io::ErrorKind) -> FaultOnce {
    FaultOnce {
        bytes: b"12\n34\n",
        offset: 0,
        kind,
        fired: false,
    }
}

#[test]
fn interrupted_buffered_reads_retry_and_terminal_errors_end_the_stream() {
    let rows = EventStream::<_, u64>::new(fault(io::ErrorKind::Interrupted), Limits::EVENT)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            EventItem::Event { line: 1, value: 12 },
            EventItem::Event { line: 2, value: 34 }
        ]
    );
    let mut stream =
        EventStream::<_, u64>::new(fault(io::ErrorKind::PermissionDenied), Limits::EVENT);
    assert_eq!(stream.next().unwrap().unwrap_err().rule, "io");
    assert!(stream.next().is_none());
    assert_eq!(
        read_bounded(fault(io::ErrorKind::Interrupted), 16).unwrap(),
        b"12\n34\n"
    );
    assert_eq!(
        read_bounded(fault(io::ErrorKind::PermissionDenied), 16)
            .unwrap_err()
            .rule,
        "io"
    );
}

#[test]
fn json_depth_clamp_remains_64_when_the_requested_limit_is_higher() {
    let limits = Limits {
        depth: 100,
        ..Limits::JSON
    };
    let at = format!("{}0{}", "[".repeat(64), "]".repeat(64));
    let over = format!("[{at}]");
    assert!(parse_json::<Value>(at.as_bytes(), limits).is_ok());
    let refusal = check_json(over.as_bytes(), limits).unwrap_err();
    assert_eq!(
        (refusal.rule, refusal.limit, refusal.observed),
        ("depth", 64, 65)
    );
}

// C-V5. The event pass counts documents, and its scanner emits nothing for a stream
// whose only content is a tab at the start of a line. 1.0's scanner rejects it, and a
// file that does not scan is never a stream of no document.
#[tokio::test]
async fn a_config_that_does_not_scan_stops_before_its_documents_are_counted() {
    for input in [
        "\t\n",
        " \n\t \n",
        "\t",
        "\n\t\n",
        "# c\n\t# c2\n",
        "\t# c\n",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path());
        std::fs::create_dir_all(paths.root()).unwrap();
        std::fs::write(paths.config_path(), input).unwrap();
        // The effective 1.0 reader refuses every one of these as a parse error.
        FileConfigStore::new(paths.clone())
            .read_merged()
            .await
            .unwrap_err();
        let bytes = input.as_bytes();
        let checked = check_yaml(bytes, Limits::YAML).unwrap_err();
        let parsed = parse_yaml::<Value>(bytes, Limits::YAML).await.unwrap_err();
        let decoded = decode_yaml::<Value>(bytes, Limits::YAML).await.unwrap_err();
        for refusal in [checked, parsed, decoded] {
            assert_eq!(refusal.rule, "parse", "{input:?}: {refusal}");
            assert!(
                !refusal.detail.contains("expected one document"),
                "{input:?}"
            );
        }
        assert_eq!(std::fs::read(paths.config_path()).unwrap(), bytes);
    }
    // Streams that scan and hold no document keep their own refusal.
    for input in ["", "\n", " \n  \n", "# c\n", "  # c\r\n\r\n"] {
        let refusal = check_yaml(input.as_bytes(), Limits::YAML).unwrap_err();
        assert_eq!(
            (refusal.rule, refusal.observed),
            ("documents", 0),
            "{input:?}"
        );
    }
    // The byte bound still comes first.
    let over = "\t".repeat(Limits::YAML.bytes + 1);
    assert_eq!(
        check_yaml(over.as_bytes(), Limits::YAML).unwrap_err().rule,
        "bytes"
    );
}
