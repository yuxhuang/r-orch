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

const CASES: [&str; 6] = [
    "index-absent",
    "index-absent-update-order",
    "index-disagrees",
    "index-malformed",
    "index-null",
    "index-object",
];
/// The cases whose `_index.json` is missing or unusable (R122: "lists nothing").
const REBUILT: [&str; 5] = [
    "index-absent",
    "index-absent-update-order",
    "index-malformed",
    "index-null",
    "index-object",
];

/// The rebuilt case whose id order, creation order and 1.0 list order all differ.
const DISTINCT_ORDERS: &str = "index-absent-update-order";

const INDEX_DIFFERS: &str = "index_differs";
const INDEX_UNREADABLE: &str = "index_unreadable";
const NO_FILE: &str = "an index entry with no file";
const UNLISTED: &str = "an entity its index does not list";

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

/// The project root a case reads from: its own `source/`, or a reference that stays
/// inside `fixtures/` (the stale-index case is another directory's source).
fn confine_source(case: &str, source: &str) -> Result<PathBuf, String> {
    if Path::new(source).is_absolute() {
        return Err(format!("{case}: source {source:?} is absolute"));
    }
    let fixtures = corpus()
        .join("../..")
        .canonicalize()
        .map_err(|e| format!("fixtures root: {e}"))?;
    let resolved = corpus()
        .join(case)
        .join(source)
        .canonicalize()
        .map_err(|e| format!("{case}: source {source:?}: {e}"))?;
    if !resolved.starts_with(&fixtures) {
        return Err(format!("{case}: source {source:?} leaves the fixtures"));
    }
    if !resolved.join(".orchestry").is_dir() {
        return Err(format!("{case}: source {source:?} holds no .orchestry"));
    }
    Ok(corpus().join(case).join(source))
}

fn source_of(case: &str, expected: &Expected) -> PathBuf {
    confine_source(case, &expected.source).unwrap_or_else(|why| panic!("{why}"))
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

/// What the spec says the importer makes of a task directory, worked out from the
/// source files alone (R69 "Order", R122): the import order, and the report as one
/// (class, subject) pair per finding, sorted. No expectation file is read.
struct Derived {
    order: Vec<String>,
    report: Vec<(String, String)>,
}

fn derive(source: &Path) -> Result<Derived, String> {
    let dir = source.join(".orchestry").join("tasks");
    let mut files = BTreeMap::new();
    for id in entity_ids(source) {
        let text = std::fs::read_to_string(dir.join(format!("{id}.yml")))
            .map_err(|e| format!("{id}: {e}"))?;
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            format!("{id}: the entity file does not parse (R123, not this corpus): {e}")
        })?;
        files.insert(id, value);
    }
    // A missing file and an unparseable one are the same thing to R122: no usable index.
    let index = std::fs::read(dir.join("_index.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let Some(serde_json::Value::Array(entries)) = index else {
        return Ok(Derived {
            order: files.keys().cloned().collect(),
            report: vec![(INDEX_UNREADABLE.into(), "tasks".into())],
        });
    };

    let mut order = Vec::new();
    let mut report = Vec::new();
    let mut listed = BTreeSet::new();
    for entry in &entries {
        let id = entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or("an index entry has no string id; the spec is silent on it")?;
        if !listed.insert(id.to_owned()) {
            return Err(format!("{id} is listed twice; the spec is silent on it"));
        }
        match files.get(id) {
            None => report.push((NO_FILE.into(), id.to_owned())),
            Some(file) => {
                // The index's position is the array position (R69); its content is never used.
                order.push(id.to_owned());
                if file != entry {
                    report.push((INDEX_DIFFERS.into(), id.to_owned()));
                }
            }
        }
    }
    for id in files.keys().filter(|id| !listed.contains(*id)) {
        report.push((UNLISTED.into(), id.clone()));
        order.push(id.clone());
    }
    report.sort();
    Ok(Derived { order, report })
}

/// The entity ids of a source ordered by a timestamp field of the files themselves,
/// ascending. The timestamps must be pairwise different, or the order is a tie.
fn ids_by_field(source: &Path, field: &str) -> Vec<String> {
    let mut keyed: Vec<(String, String)> = entity_ids(source)
        .into_iter()
        .map(|id| {
            let text =
                std::fs::read_to_string(source.join(format!(".orchestry/tasks/{id}.yml"))).unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            (value[field].as_str().unwrap().to_owned(), id)
        })
        .collect();
    keyed.sort();
    assert!(
        keyed.windows(2).all(|w| w[0].0 != w[1].0),
        "{field} ties in {}",
        source.display()
    );
    keyed.into_iter().map(|(_, id)| id).collect()
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
        // The published array follows directory enumeration, which nothing specifies:
        // membership only. (`list()` above is specified; this file is not.)
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

    // Order and report come from the source alone, and must match exactly (R69, R122).
    let derived = derive(source)?;
    if want.task_order != derived.order {
        return Err(format!(
            "task_order {:?}, the source gives {:?}",
            want.task_order, derived.order
        ));
    }
    let mut recorded: Vec<(String, String)> = want
        .report
        .iter()
        .map(|f| (f.class.clone(), f.subject.clone()))
        .collect();
    recorded.sort();
    if recorded != derived.report {
        return Err(format!(
            "report {recorded:?}, the source gives {:?}",
            derived.report
        ));
    }
    if REBUILT.contains(&expected.case.as_str()) && want.task_order != on_disk {
        // An index that lists nothing: every file, in id order (R122).
        return Err(format!(
            "task_order {:?}, id order is {on_disk:?}",
            want.task_order
        ));
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
fn rebuilt_cases_have_files_whose_id_order_differs_from_creation_order() {
    for case in REBUILT {
        let expected = load(case);
        let source = source_of(case, &expected);
        assert!(
            entity_ids(&source).len() >= 2,
            "{case}: id order needs two entity files"
        );
        assert_ne!(
            ids_by_field(&source, "created_at"),
            expected.importer_1_1.task_order,
            "{case}: creation order equals id order"
        );
        // 1.0's list order is specified wherever `updated_at` has no tie (equal status).
        assert!(expected.legacy_1_0.list_order_specified, "{case}");
        assert_eq!(expected.importer_1_1.task_order, entity_ids(&source));
    }
}

/// An importer that copies 1.0's `list()` (newest update first) or follows creation
/// time must fail one case: here the three orders are pairwise different.
#[test]
fn one_rebuilt_case_tells_id_creation_and_1_0_list_order_apart() {
    let expected = load(DISTINCT_ORDERS);
    let source = source_of(DISTINCT_ORDERS, &expected);
    let id_order = entity_ids(&source);
    let created = ids_by_field(&source, "created_at");
    let mut updated = ids_by_field(&source, "updated_at");
    updated.reverse(); // list(): equal status, `updated_at` descending
    for status in entity_ids(&source).iter().map(|id| {
        let text = std::fs::read_to_string(source.join(format!(".orchestry/tasks/{id}.yml")));
        let value: serde_json::Value = serde_json::from_str(&text.unwrap()).unwrap();
        value["status"].as_str().unwrap().to_owned()
    }) {
        assert_eq!(status, "todo", "a status difference would reorder list()");
    }

    assert_eq!(expected.legacy_1_0.list_order, updated);
    assert_eq!(expected.importer_1_1.task_order, id_order);
    assert_ne!(id_order, created);
    assert_ne!(id_order, updated);
    assert_ne!(created, updated);
}

#[test]
fn a_source_reference_must_stay_inside_the_fixtures() {
    assert!(confine_source("index-absent", "source").is_ok());
    assert!(
        confine_source("index-disagrees", "../../../storage-v1/index-disagrees").is_ok(),
        "the one reference the corpus uses"
    );
    for (case, bad) in [
        ("index-absent", "/etc"),
        ("index-absent", "../../../../crates"),
        ("index-absent", "../../../.."),
        (
            "index-absent",
            "../../../storage-v1/index-disagrees/../../../../crates",
        ),
        ("index-absent", "../index-absent-update-order/missing"),
        ("index-absent", "../../../storage-v1"), // inside, but no .orchestry
    ] {
        assert!(confine_source(case, bad).is_err(), "{case} source {bad:?}");
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

/// Every one-step change to a case's recorded report: drop, repeat, re-class or
/// re-subject each finding, and add one that the source does not give. The reviewed
/// survivors M1-M4 and M6, and a dropped orphan finding, are among them (see the
/// test `the_named_report_mutants_are_caught`).
fn report_mutants(expected: &Expected) -> Vec<(String, Expected)> {
    let mut out = Vec::new();
    let report = &expected.importer_1_1.report;
    let classes = [INDEX_DIFFERS, INDEX_UNREADABLE, NO_FILE, UNLISTED];
    let mut push = |label: String, edit: &dyn Fn(&mut Vec<Finding>)| {
        let mut wrong = expected.clone();
        edit(&mut wrong.importer_1_1.report);
        out.push((label, wrong));
    };
    for (i, f) in report.iter().enumerate() {
        let name = format!("{} {}", f.class, f.subject);
        push(format!("drop {name}"), &|r| {
            r.remove(i);
        });
        push(format!("repeat {name}"), &|r| {
            let again = r[i].clone();
            r.push(again);
        });
        push(format!("re-subject {name}"), &|r| {
            r[i].subject = "tsk_idx_a_other".into();
        });
        for class in classes.iter().filter(|c| **c != f.class) {
            push(format!("{name} as {class}"), &|r| {
                r[i].class = (*class).to_owned();
            });
        }
    }
    push("an extra finding".into(), &|r| {
        r.push(Finding {
            class: NO_FILE.into(),
            subject: "tsk_nowhere".into(),
            named_in_spec: false,
            import: "goes_on".into(),
        });
    });
    out
}

type ReportEdit = fn(&mut Vec<Finding>);

/// The reviewed report mutants (M1-M4, M6, a dropped orphan), by name, each of which must be caught
/// (they survived the first version of this suite).
#[tokio::test]
async fn the_named_report_mutants_are_caught() {
    let spec = spec_text();
    let load_case = |case: &str| {
        let expected = load(case);
        let source = source_of(case, &expected);
        (expected, source)
    };
    let mutants: [(&str, &str, ReportEdit); 6] = [
        (
            "M1 index-absent: index_unreadable -> index_differs",
            "index-absent",
            |r| {
                r[0].class = INDEX_DIFFERS.into();
            },
        ),
        (
            "M2 index-disagrees: drop the tsk_ghost finding",
            "index-disagrees",
            |r| {
                r.retain(|f| f.subject != "tsk_ghost");
            },
        ),
        (
            "M3 index-disagrees: drop index_differs tsk_b",
            "index-disagrees",
            |r| {
                r.retain(|f| f.class != INDEX_DIFFERS);
            },
        ),
        (
            "M4 index-null: subject tasks -> tsk_idx_a",
            "index-null",
            |r| {
                r[0].subject = "tsk_idx_a".into();
            },
        ),
        (
            "M6 index-disagrees: index_differs tsk_b -> index_unreadable tasks",
            "index-disagrees",
            |r| {
                let f = r.iter_mut().find(|f| f.class == INDEX_DIFFERS).unwrap();
                f.class = INDEX_UNREADABLE.into();
                f.subject = "tasks".into();
            },
        ),
        (
            "index-disagrees: drop the tsk_orphan finding",
            "index-disagrees",
            |r| {
                r.retain(|f| f.subject != "tsk_orphan");
            },
        ),
    ];
    for (label, case, edit) in mutants {
        let (expected, source) = load_case(case);
        assert!(
            check_expectation(&source, &expected, &spec).await.is_ok(),
            "{label}: baseline"
        );
        let mut wrong = expected.clone();
        edit(&mut wrong.importer_1_1.report);
        assert!(
            check_expectation(&source, &wrong, &spec).await.is_err(),
            "survives: {label}"
        );
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

        // The report: class, subject, multiplicity and membership are all checked.
        for (label, wrong) in report_mutants(&expected) {
            assert!(
                check_expectation(&source, &wrong, &spec).await.is_err(),
                "{case}: report mutant survives: {label}"
            );
        }
        if case == DISTINCT_ORDERS {
            // An importer that copies 1.0's list() order.
            let mut wrong = expected.clone();
            wrong.importer_1_1.task_order = expected.legacy_1_0.list_order.clone();
            assert!(
                check_expectation(&source, &wrong, &spec).await.is_err(),
                "{case}: list order taken for import order"
            );
        }
    }
}

// X-V5 / F19: use each real directory's entity type, rather than the one-field
// test type in import::tests::compatibility. These are legacy reference checks;
// the integrated importer's identical refusal of both inputs remains with T21.
mod typed_duplicates {
    use super::*;
    use orch_core::storage::{ContextEntry, ContextStore};
    use orch_core::test_support::FixedClock;
    use orch_domain::{Agent, Goal, Message, Task};
    use orch_infra::storage::index::ItemFormat;
    use orch_infra::storage::{FileContextStore, IndexManager};
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use std::sync::Arc;

    const TIME: &str = "2026-01-01T00:00:00.000Z";

    // Construct and serialize real entities: no simplified stand-in type can
    // accidentally accept an identity-only object as a complete legacy entry.
    fn entity<T: DeserializeOwned + Serialize>(directory: &str, identity: &str) -> Value {
        let value = match directory {
            "tasks" => json!({
                "id": identity, "title": identity, "description": "synthetic task",
                "status": "todo", "priority": 2, "created_at": TIME,
                "updated_at": TIME, "attempts": 0, "max_attempts": 3
            }),
            "agents" => json!({
                "id": identity, "name": identity, "adapter": "shell", "config": {},
                "status": "idle", "stats": {"tasks_completed": 0, "tasks_failed": 0,
                    "total_runs": 0, "total_runtime_ms": 0}
            }),
            "goals" => json!({
                "id": identity, "title": identity, "description": "synthetic goal",
                "status": "active", "created_at": TIME, "updated_at": TIME
            }),
            "messages" => json!({
                "id": identity, "channel": "broadcast", "from_agent_id": "cli",
                "to_agent_id": null, "subject": identity, "body": "synthetic message",
                "created_at": TIME, "status": "pending"
            }),
            "context" => json!({
                "key": identity, "value": identity, "created_at": TIME, "updated_at": TIME
            }),
            _ => panic!("not an indexed legacy directory: {directory}"),
        };
        serde_json::to_value(serde_json::from_value::<T>(value).unwrap()).unwrap()
    }

    async fn check<T>(directory: &str, format: ItemFormat, complete: bool)
    where
        T: DeserializeOwned + Serialize + Send + Sync,
    {
        let source = tempfile::tempdir().unwrap();
        assert_hermetic(source.path());
        let relative = format!(".orchestry/{directory}");
        let dir = source.path().join(&relative);
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<Value> = ["a", "b", "c"]
            .into_iter()
            .map(|id| entity::<T>(directory, id))
            .collect();
        for (id, value) in ["a", "b", "c"].into_iter().zip(&files) {
            let (extension, bytes) = match format {
                ItemFormat::Yaml => ("yml", serde_yaml_ng::to_string(value).unwrap()),
                ItemFormat::Json => ("json", serde_json::to_string(value).unwrap()),
            };
            std::fs::write(dir.join(format!("{id}.{extension}")), bytes).unwrap();
        }
        let identity = if directory == "context" { "key" } else { "id" };
        let input = if complete {
            // Deliberately non-id order, with a real unlisted file a. Neither
            // sorting, deduplication nor rebuilding may pass this assertion.
            json!([files[2], files[1], files[1]])
        } else {
            json!([{identity: "b"}, {identity: "b"}])
        };
        let bytes = serde_json::to_vec(&input).unwrap();
        std::fs::write(dir.join("_index.json"), &bytes).unwrap();
        let before = snapshot(source.path());
        let copy = tempfile::tempdir().unwrap();
        assert_hermetic(copy.path());
        copy_tree(source.path(), copy.path());
        let manager = IndexManager::<T>::new(copy.path().join(&relative), format);
        let read = serde_json::to_value(manager.read_index().await.unwrap()).unwrap();
        let published = std::fs::read(manager.index_path()).unwrap();
        if complete {
            assert_eq!(
                read, input,
                "{directory}: preserve physical duplicate order"
            );
            assert_eq!(
                published, bytes,
                "{directory}: do not rewrite a typed index"
            );
            assert_eq!(snapshot(copy.path()), before);
        } else {
            // Rebuild enumeration order is unspecified. Assert exact typed
            // membership, including the two files hidden by the damaged index.
            let mut rebuilt: Vec<Value> = serde_json::from_value(read).unwrap();
            rebuilt.sort_by(|a, b| a[identity].as_str().cmp(&b[identity].as_str()));
            assert_eq!(rebuilt, files, "{directory}: rebuild all entity files once");
            assert_ne!(published, bytes, "{directory}: publish the rebuilt index");
            let mut cached: Vec<Value> = serde_json::from_slice(&published).unwrap();
            cached.sort_by(|a, b| a[identity].as_str().cmp(&b[identity].as_str()));
            assert_eq!(cached, files, "{directory}: persist the complete rebuild");
            let after = snapshot(copy.path());
            for (name, value) in &before {
                if name != &format!("{relative}/_index.json") {
                    assert_eq!(after.get(name), Some(value), "entity bytes changed: {name}");
                }
            }
        }
        assert_eq!(
            snapshot(source.path()),
            before,
            "read only the disposable copy"
        );
    }

    #[tokio::test]
    async fn tasks_identity_only_duplicates_rebuild_all_files() {
        check::<Task>("tasks", ItemFormat::Yaml, false).await;
    }

    #[tokio::test]
    async fn tasks_complete_duplicates_keep_physical_order_and_hide_unlisted_files() {
        check::<Task>("tasks", ItemFormat::Yaml, true).await;
    }

    #[tokio::test]
    async fn agents_identity_only_duplicates_rebuild_all_files() {
        check::<Agent>("agents", ItemFormat::Yaml, false).await;
    }

    #[tokio::test]
    async fn agents_complete_duplicates_keep_physical_order_and_hide_unlisted_files() {
        check::<Agent>("agents", ItemFormat::Yaml, true).await;
    }

    #[tokio::test]
    async fn goals_identity_only_duplicates_rebuild_all_files() {
        check::<Goal>("goals", ItemFormat::Yaml, false).await;
    }

    #[tokio::test]
    async fn goals_complete_duplicates_keep_physical_order_and_hide_unlisted_files() {
        check::<Goal>("goals", ItemFormat::Yaml, true).await;
    }

    #[tokio::test]
    async fn messages_identity_only_duplicates_rebuild_all_files() {
        check::<Message>("messages", ItemFormat::Json, false).await;
    }

    #[tokio::test]
    async fn messages_complete_duplicates_keep_physical_order_and_hide_unlisted_files() {
        check::<Message>("messages", ItemFormat::Json, true).await;
    }

    #[tokio::test]
    async fn context_identity_only_duplicates_rebuild_all_files() {
        check::<ContextEntry>("context", ItemFormat::Json, false).await;
    }

    #[tokio::test]
    async fn context_complete_duplicates_keep_physical_order_and_hide_unlisted_files() {
        check::<ContextEntry>("context", ItemFormat::Json, true).await;
    }

    #[tokio::test]
    async fn context_list_rebuilds_in_memory_and_startup_publishes_the_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        assert_hermetic(temp.path());
        let dir = temp.path().join(".orchestry/context");
        std::fs::create_dir_all(&dir).unwrap();
        for key in ["c", "a", "b"] {
            let entry = entity::<ContextEntry>("context", key);
            std::fs::write(
                dir.join(format!("{key}.json")),
                serde_json::to_vec(&entry).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(dir.join("_index.json"), br#"[{"key":"b"},{"key":"b"}]"#).unwrap();
        let before = snapshot(temp.path());
        let store = FileContextStore::new(Paths::new(temp.path()), Arc::new(FixedClock::new()));
        let listed = store.list().await.unwrap();
        let keys: Vec<_> = listed.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(keys, ["a", "b", "c"]);
        assert_eq!(
            snapshot(temp.path()),
            before,
            "list rebuild must not publish"
        );
        store.ensure_index().await.unwrap();
        let published: Vec<ContextEntry> =
            serde_json::from_slice(&std::fs::read(dir.join("_index.json")).unwrap()).unwrap();
        let mut keys: Vec<_> = published.iter().map(|entry| entry.key.as_str()).collect();
        keys.sort();
        assert_eq!(keys, ["a", "b", "c"], "startup publishes all files once");
        let after = snapshot(temp.path());
        for (path, bytes) in before
            .iter()
            .filter(|(path, _)| !path.ends_with("_index.json"))
        {
            assert_eq!(
                after.get(path),
                Some(bytes),
                "startup changed entity {path}"
            );
        }
    }

    #[tokio::test]
    async fn context_list_keeps_duplicate_keys_and_projection_uses_last_value_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        assert_hermetic(temp.path());
        let dir = temp.path().join(".orchestry/context");
        std::fs::create_dir_all(&dir).unwrap();
        let b = entity::<ContextEntry>("context", "b");
        let mut last_b = b.clone();
        last_b["value"] = json!("last b");
        let input = json!([entity::<ContextEntry>("context", "c"), b, last_b]);
        std::fs::write(dir.join("_index.json"), serde_json::to_vec(&input).unwrap()).unwrap();
        let before = snapshot(temp.path());
        let store = FileContextStore::new(Paths::new(temp.path()), Arc::new(FixedClock::new()));
        let listed: Vec<_> = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.key, e.value))
            .collect();
        assert_eq!(
            listed,
            [
                ("b".into(), "b".into()),
                ("b".into(), "last b".into()),
                ("c".into(), "c".into())
            ]
        );
        let projected: Vec<_> = store.get_all().await.unwrap().into_iter().collect();
        assert_eq!(
            projected,
            [("b".into(), "last b".into()), ("c".into(), "c".into())]
        );
        assert_eq!(snapshot(temp.path()), before);
    }
}
