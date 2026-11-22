//! What 1.0's own file reader does with the damaged-index corpus of
//! `fixtures/storage/index-recovery/`, next to what the 1.1 importer is expected to
//! make of the same directories (spec R65, R69, R122, R123).
//!
//! Each case directory holds a `source/` project root and, beside it, an
//! `expected.json`. Two parts of that file are kept apart on purpose:
//!
//! - `legacy_1_0` is what the real `FileTaskStore` returns. This test establishes it.
//! - `importer_1_1` is what the importer must produce. No importer exists yet; this
//!   test only checks that the expectation agrees with the source files and with the
//!   spec's problem-class table. The importer's own assertions belong to T21.
//!
//! A 1.0 read can rebuild and publish an index, so every read here happens on a copy.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orch_core::storage::{TaskFilter, TaskStore};
use orch_core::test_support::assert_hermetic;
use orch_infra::storage::{FileTaskStore, Paths};
use serde::Deserialize;

const CASES: [&str; 5] = [
    "index-absent",
    "index-disagrees",
    "index-malformed",
    "index-null",
    "index-object",
];
/// The cases whose `_index.json` is missing or unusable (R122: "lists nothing").
const REBUILT: [&str; 4] = [
    "index-absent",
    "index-malformed",
    "index-null",
    "index-object",
];

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    case: String,
    source: String,
    spec: Vec<String>,
    legacy_1_0: Legacy,
    importer_1_1: Importer,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Legacy {
    listed_ids_sorted: Vec<String>,
    list_order_specified: bool,
    #[serde(default)]
    list_order: Vec<String>,
    #[serde(default)]
    list_titles: BTreeMap<String, String>,
    read_publishes_an_index: bool,
    titles_by_get: BTreeMap<String, String>,
    #[serde(default)]
    get_absent: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Importer {
    task_order: Vec<String>,
    tasks: BTreeMap<String, Payload>,
    report: Vec<Finding>,
}

#[derive(Clone, Deserialize, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
struct Payload {
    title: String,
    description: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Finding {
    class: String,
    subject: String,
    named_in_spec: bool,
    import: String,
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/storage/index-recovery")
}

fn load(case: &str) -> Expected {
    let path = corpus().join(case).join("expected.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The project root a case reads from, resolved from its own `source` entry.
fn source_of(case: &str, expected: &Expected) -> PathBuf {
    corpus().join(case).join(&expected.source)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            assert!(kind.is_file(), "fixtures cannot contain links");
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

/// Every file under `root` with its bytes, by relative path.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Ids of the entity files in a project root's `tasks/` directory, in id order.
fn entity_ids(source: &Path) -> Vec<String> {
    let dir = source.join(".orchestry").join("tasks");
    let mut ids: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|name| name.strip_suffix(".yml").map(str::to_owned))
        .collect();
    ids.sort();
    ids
}

/// Run 1.0's real reader on a copy of `source` and compare it with `expected.legacy_1_0`.
async fn check_legacy(source: &Path, expected: &Expected) -> Result<(), String> {
    let temp = tempfile::tempdir().unwrap();
    assert_hermetic(temp.path());
    copy_tree(source, temp.path());
    let index = temp.path().join(".orchestry/tasks/_index.json");
    let before = std::fs::read(&index).ok();

    let tasks = FileTaskStore::new(Paths::new(temp.path()));
    let listed = tasks
        .list(&TaskFilter::default())
        .await
        .map_err(|e| format!("list failed: {e}"))?;
    let want = &expected.legacy_1_0;

    let mut ids: Vec<String> = listed.iter().map(|t| t.id.clone()).collect();
    if want.list_order_specified && ids != want.list_order {
        return Err(format!(
            "list order {ids:?}, recorded {:?}",
            want.list_order
        ));
    }
    ids.sort();
    if ids != want.listed_ids_sorted {
        return Err(format!(
            "listed ids {ids:?}, recorded {:?}",
            want.listed_ids_sorted
        ));
    }
    for (id, title) in &want.list_titles {
        let got = listed
            .iter()
            .find(|t| &t.id == id)
            .map(|t| t.title.as_str());
        if got != Some(title.as_str()) {
            return Err(format!(
                "listed title of {id} is {got:?}, recorded {title:?}"
            ));
        }
    }
    for (id, title) in &want.titles_by_get {
        let got = tasks.get(id).await.map_err(|e| format!("get {id}: {e}"))?;
        if got.as_ref().map(|t| t.title.as_str()) != Some(title.as_str()) {
            return Err(format!(
                "title of {id} by get is {:?}, recorded {title:?}",
                got.map(|t| t.title)
            ));
        }
    }
    for id in &want.get_absent {
        if tasks
            .get(id)
            .await
            .map_err(|e| format!("get {id}: {e}"))?
            .is_some()
        {
            return Err(format!("{id} is readable, recorded as absent"));
        }
    }
    let after = std::fs::read(&index).ok();
    let published = after != before;
    if published != want.read_publishes_an_index {
        return Err(format!(
            "read published an index: {published}, recorded {}",
            want.read_publishes_an_index
        ));
    }
    if published {
        // The rebuilt index is an array of the entities the read returned.
        let rebuilt: Vec<serde_json::Value> =
            serde_json::from_slice(&after.ok_or("no index published")?)
                .map_err(|e| format!("published index is not an array: {e}"))?;
        let mut published_ids: Vec<&str> =
            rebuilt.iter().filter_map(|v| v["id"].as_str()).collect();
        published_ids.sort();
        if published_ids != want.listed_ids_sorted {
            return Err(format!("published index lists {published_ids:?}"));
        }
    }
    Ok(())
}

/// Check the importer expectation against the source files and the spec table.
/// This does not read an index and does not import anything.
async fn check_expectation(source: &Path, expected: &Expected, spec: &str) -> Result<(), String> {
    let want = &expected.importer_1_1;
    let on_disk = entity_ids(source);

    let members: BTreeSet<&str> = want.task_order.iter().map(String::as_str).collect();
    if members.len() != want.task_order.len() {
        return Err("task_order repeats an id".into());
    }
    if members != on_disk.iter().map(String::as_str).collect() {
        return Err(format!(
            "task_order {:?} is not the entity files {on_disk:?}",
            want.task_order
        ));
    }
    if want
        .tasks
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != members
    {
        return Err("tasks and task_order name different ids".into());
    }

    // The payload is the entity file's, whatever any index says.
    let temp = tempfile::tempdir().unwrap();
    assert_hermetic(temp.path());
    copy_tree(source, temp.path());
    let tasks = FileTaskStore::new(Paths::new(temp.path()));
    for (id, payload) in &want.tasks {
        let file = tasks
            .get(id)
            .await
            .map_err(|e| format!("get {id}: {e}"))?
            .ok_or(format!("{id} has no readable file"))?;
        let actual = Payload {
            title: file.title,
            description: file.description,
        };
        if &actual != payload {
            return Err(format!("{id}: file holds {actual:?}, expected {payload:?}"));
        }
    }

    if REBUILT.contains(&expected.case.as_str()) {
        // An index that lists nothing: every file, in id order (R122).
        if want.task_order != on_disk {
            return Err(format!(
                "task_order {:?}, id order is {on_disk:?}",
                want.task_order
            ));
        }
    } else {
        // A usable index gives order: its entries that have a file keep their
        // relative order and come first (R69, "Order").
        let kept: Vec<&String> = expected
            .legacy_1_0
            .list_order
            .iter()
            .filter(|id| members.contains(id.as_str()))
            .collect();
        if want.task_order.iter().take(kept.len()).collect::<Vec<_>>() != kept {
            return Err(format!(
                "task_order {:?} does not start with the index order {kept:?}",
                want.task_order
            ));
        }
    }

    // The classification is the spec's: every finding goes on, and the row says so.
    let row = spec
        .lines()
        .find(|line| {
            line.trim_start()
                .starts_with("| `index_differs`, `index_unreadable`;")
        })
        .ok_or("the spec's problem-class row for the index classes is gone")?;
    if !row.contains("Goes on") {
        return Err(format!(
            "the spec row no longer says the import goes on: {row}"
        ));
    }
    if want.report.is_empty() {
        return Err("no finding is recorded".into());
    }
    for finding in &want.report {
        if finding.import != "goes_on" {
            return Err(format!(
                "{}: import is {:?}, the spec row says goes on",
                finding.class, finding.import
            ));
        }
        if !row.contains(&finding.class) {
            return Err(format!("{:?} is not in the spec row", finding.class));
        }
        if finding.named_in_spec != finding.class.starts_with("index_") {
            return Err(format!(
                "{:?}: named_in_spec is {}",
                finding.class, finding.named_in_spec
            ));
        }
        if !members.contains(finding.subject.as_str())
            && finding.subject != "tasks"
            && !expected
                .legacy_1_0
                .listed_ids_sorted
                .contains(&finding.subject)
        {
            return Err(format!(
                "finding subject {:?} is neither a directory nor a known id",
                finding.subject
            ));
        }
    }
    Ok(())
}

fn spec_text() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/specs/2026-11-20-sqlite-storage.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn every_case_has_an_expectation_beside_its_source_and_none_inside_it() {
    let mut dirs: Vec<String> = std::fs::read_dir(corpus())
        .unwrap()
        .map(|e| e.unwrap())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    assert_eq!(
        dirs, CASES,
        "a case directory is missing from CASES or the corpus"
    );
    for case in CASES {
        let expected = load(case);
        assert_eq!(expected.case, case);
        assert!(
            expected.spec.contains(&"R122".to_string()),
            "{case} names no rule"
        );
        let source = source_of(case, &expected);
        assert!(
            source.join(".orchestry").is_dir(),
            "{case}: no .orchestry in its source"
        );
        assert!(
            !snapshot(&source)
                .keys()
                .any(|name| name.ends_with("expected.json")),
            "{case}: an expectation lies inside the source root"
        );
        // Only `index-disagrees` is another directory's source, by reference.
        assert_eq!(
            expected.source != "source",
            case == "index-disagrees",
            "{case}"
        );
    }
}

#[tokio::test]
async fn the_1_0_reader_does_what_the_recorded_observations_say() {
    for case in CASES {
        let expected = load(case);
        if let Err(why) = check_legacy(&source_of(case, &expected), &expected).await {
            panic!("{case}: {why}");
        }
    }
}

#[tokio::test]
async fn the_importer_expectations_agree_with_the_source_files_and_the_spec() {
    let spec = spec_text();
    for case in CASES {
        let expected = load(case);
        if let Err(why) = check_expectation(&source_of(case, &expected), &expected, &spec).await {
            panic!("{case}: {why}");
        }
    }
}

#[test]
fn rebuilt_cases_have_files_whose_id_order_differs_from_creation_and_file_order() {
    for case in REBUILT {
        let expected = load(case);
        let source = source_of(case, &expected);
        assert!(
            entity_ids(&source).len() >= 2,
            "{case}: id order needs two entity files"
        );
        let mut by_created: Vec<(String, String)> = entity_ids(&source)
            .into_iter()
            .map(|id| {
                let text =
                    std::fs::read_to_string(source.join(format!(".orchestry/tasks/{id}.yml")))
                        .unwrap();
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                (value["created_at"].as_str().unwrap().to_owned(), id)
            })
            .collect();
        by_created.sort();
        let created_order: Vec<String> = by_created.into_iter().map(|(_, id)| id).collect();
        assert_ne!(
            created_order, expected.importer_1_1.task_order,
            "{case}: creation order equals id order"
        );
    }
}

#[test]
fn a_1_0_read_leaves_the_checked_in_corpus_untouched() {
    // Every case reads from a copy; prove the originals still have no published index.
    let before = snapshot(&corpus());
    let before_v1 = snapshot(&corpus().join("../../storage-v1/index-disagrees"));
    assert!(!before.contains_key("index-absent/source/.orchestry/tasks/_index.json"));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        for case in CASES {
            let expected = load(case);
            check_legacy(&source_of(case, &expected), &expected)
                .await
                .unwrap();
        }
    });
    assert_eq!(snapshot(&corpus()), before);
    assert_eq!(
        snapshot(&corpus().join("../../storage-v1/index-disagrees")),
        before_v1
    );
}

#[test]
fn the_corpus_carries_no_private_location() {
    for (name, bytes) in snapshot(&corpus()) {
        let text = String::from_utf8_lossy(&bytes);
        for marker in [
            "/Users/",
            "/home/",
            "/Volumes/",
            "/private/",
            "C:\\",
            "@",
            ".local",
            "ssh-",
        ] {
            assert!(!text.contains(marker), "{name} contains {marker:?}");
        }
    }
}

/// The checks must fail on a changed payload or a changed membership; a check that
/// cannot fail would pass any corpus.
#[tokio::test]
async fn a_changed_payload_or_membership_is_caught() {
    let spec = spec_text();
    for case in CASES {
        let expected = load(case);
        let source = source_of(case, &expected);
        let tasks_of = |root: &Path| root.join(".orchestry/tasks");
        let fresh = || {
            let temp = tempfile::tempdir().unwrap();
            assert_hermetic(temp.path());
            copy_tree(&source, temp.path());
            temp
        };
        assert!(
            check_legacy(&source, &expected).await.is_ok(),
            "{case}: baseline"
        );
        assert!(
            check_expectation(&source, &expected, &spec).await.is_ok(),
            "{case}: baseline"
        );

        let id = expected.importer_1_1.task_order[0].clone();

        // A changed entity title.
        let temp = fresh();
        let file = tasks_of(temp.path()).join(format!("{id}.yml"));
        let text = std::fs::read_to_string(&file).unwrap();
        let altered = text.replacen("\"title\": \"", "\"title\": \"Changed ", 1);
        assert_ne!(text, altered);
        std::fs::write(&file, altered).unwrap();
        assert!(
            check_legacy(temp.path(), &expected).await.is_err(),
            "{case}: changed title, legacy"
        );
        assert!(
            check_expectation(temp.path(), &expected, &spec)
                .await
                .is_err(),
            "{case}: changed title, expectation"
        );

        // A removed and an added entity file.
        let temp = fresh();
        std::fs::remove_file(tasks_of(temp.path()).join(format!("{id}.yml"))).unwrap();
        assert!(
            check_legacy(temp.path(), &expected).await.is_err(),
            "{case}: removed file, legacy"
        );
        assert!(
            check_expectation(temp.path(), &expected, &spec)
                .await
                .is_err(),
            "{case}: removed file, expectation"
        );
        let temp = fresh();
        let dir = tasks_of(temp.path());
        std::fs::copy(dir.join(format!("{id}.yml")), dir.join("tsk_idx_extra.yml")).unwrap();
        assert!(
            check_expectation(temp.path(), &expected, &spec)
                .await
                .is_err(),
            "{case}: added file, expectation"
        );

        // A changed expectation: membership, order and payload.
        let mut wrong = expected.clone();
        wrong.importer_1_1.task_order.pop();
        assert!(
            check_expectation(&source, &wrong, &spec).await.is_err(),
            "{case}: dropped id"
        );
        let mut wrong = expected.clone();
        wrong.importer_1_1.task_order.reverse();
        assert!(
            check_expectation(&source, &wrong, &spec).await.is_err(),
            "{case}: reversed order"
        );
        let mut wrong = expected.clone();
        wrong
            .importer_1_1
            .tasks
            .get_mut(&id)
            .unwrap()
            .title
            .push('!');
        assert!(
            check_expectation(&source, &wrong, &spec).await.is_err(),
            "{case}: changed payload"
        );
        let mut wrong = expected.clone();
        wrong.legacy_1_0.listed_ids_sorted.pop();
        assert!(
            check_legacy(&source, &wrong).await.is_err(),
            "{case}: changed legacy membership"
        );
        let mut wrong = expected.clone();
        wrong.importer_1_1.report[0].import = "stops".into();
        assert!(
            check_expectation(&source, &wrong, &spec).await.is_err(),
            "{case}: changed classification"
        );
    }
}
