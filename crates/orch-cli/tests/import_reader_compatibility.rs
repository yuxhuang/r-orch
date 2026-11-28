//! Issue 146: synthetic reader decisions through the native template caller.
//! These checks do not claim that non-template entities are imported.
#![cfg(target_os = "linux")]

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const STORES: [&str; 5] = ["tasks", "agents", "goals", "messages", "context"];

fn put(root: &Path, path: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join("source/.orchestry").join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    put(root.path(), "templates/probe.md", "synthetic template");
    root
}

fn run(root: &Path, args: &[&str], exit: i32) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_orch"))
        .env_clear()
        .env("HOME", root.join("home"))
        .env("TMPDIR", root)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("XDG_CACHE_HOME", root.join("xdg"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", root.join("global.yml"))
        .env("ORCH_DATA_HOME", root.join("data"))
        .env("ORCH_SOCKET", root.join("socket"))
        .current_dir(root)
        .args(["--json", "import", "--from"])
        .arg(root.join("source"))
        .args(args)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(exit), "{args:?}: {output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["exit_code"], exit);
    value
}

// Same observer contract as import_offline: observe file bytes and enumerate
// directory descriptors with O_NOATIME. Source access times are excluded, since
// ordinary source reads may advance them. Destination metadata includes access
// time. Never repair a timestamp to make a check pass.
type Snapshot = BTreeMap<PathBuf, (Vec<u8>, Vec<i128>)>;
fn snapshot(root: &Path, source: bool) -> Snapshot {
    fn walk(base: &Path, path: &Path, source: bool, out: &mut Snapshot) {
        let mut bytes = Vec::new();
        let kind = fs::symlink_metadata(path).unwrap().file_type();
        if kind.is_dir() {
            use std::os::unix::ffi::OsStrExt;
            let fd = fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOATIME | nix::libc::O_NOFOLLOW | nix::libc::O_DIRECTORY)
                .open(path)
                .unwrap();
            // Enumerate this open description itself: reopening the path (or
            // /proc/self/fd) would lose O_NOATIME on the directory reader.
            let mut dir = nix::dir::Dir::from_fd(fd.into()).unwrap();
            for entry in dir.iter() {
                let entry = entry.unwrap();
                let name = std::ffi::OsStr::from_bytes(entry.file_name().to_bytes());
                if name != "." && name != ".." {
                    walk(base, &path.join(name), source, out);
                }
            }
        } else if kind.is_file() {
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOATIME | nix::libc::O_NOFOLLOW)
                .open(path)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
        }
        let m = fs::symlink_metadata(path).unwrap();
        out.insert(
            path.strip_prefix(base).unwrap().to_owned(),
            (
                bytes,
                vec![
                    m.dev() as i128,
                    m.ino() as i128,
                    m.mode() as i128,
                    m.nlink() as i128,
                    m.uid() as i128,
                    m.gid() as i128,
                    m.size() as i128,
                    if source { 0 } else { m.atime() as i128 },
                    if source { 0 } else { m.atime_nsec() as i128 },
                    m.mtime() as i128,
                    m.mtime_nsec() as i128,
                    m.ctime() as i128,
                    m.ctime_nsec() as i128,
                ],
            ),
        );
    }
    let mut out = BTreeMap::new();
    if root.exists() {
        walk(root, root, source, &mut out);
    }
    out
}

#[test]
fn repeated_snapshots_preserve_stale_directory_access_time() {
    use std::time::{Duration, SystemTime};

    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("payload"), [0, 255, 1, 2]).unwrap();
    std::os::unix::fs::symlink("nested", root.path().join("link")).unwrap();
    let stale = SystemTime::UNIX_EPOCH + Duration::new(946684800, 123456789);
    for path in [root.path(), nested.as_path()] {
        fs::File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_accessed(stale))
            .unwrap();
    }
    let before: Vec<_> = [root.path(), nested.as_path()]
        .map(|path| fs::symlink_metadata(path).unwrap())
        .iter()
        .map(|m| (m.atime(), m.atime_nsec()))
        .collect();
    assert_eq!(before, vec![(946684800, 123456789); 2]);
    let first = snapshot(root.path(), false);
    for _ in 0..3 {
        assert_eq!(snapshot(root.path(), false), first);
        let after: Vec<_> = [root.path(), nested.as_path()]
            .map(|path| fs::symlink_metadata(path).unwrap())
            .iter()
            .map(|m| (m.atime(), m.atime_nsec()))
            .collect();
        assert_eq!(
            after, before,
            "observer must preserve even the first stale atime"
        );
    }
}

fn refusal_parity(root: &Path, existing: bool) -> Value {
    let source = snapshot(&root.join("source"), true);
    let destination = snapshot(&root.join("data"), false);
    let mut report = None;
    let modes: &[&[&str]] = if existing {
        &[&["--resume", "--dry-run"], &["--resume"]]
    } else {
        &[&["--dry-run"], &[]]
    };
    for args in modes {
        let value = run(root, args, 2);
        assert_eq!(value["imported"], 0);
        assert_eq!(value["unfinished"], false);
        assert_eq!(snapshot(&root.join("source"), true), source);
        assert_eq!(snapshot(&root.join("data"), false), destination);
        if !existing {
            assert!(!root.join("data").exists());
        }
        if let Some(previous) = &report {
            assert_eq!(&value["preflight"], previous);
        } else {
            report = Some(value["preflight"].clone());
        }
    }
    report.unwrap()
}

fn prepare(existing: bool) -> tempfile::TempDir {
    let root = fixture();
    if existing {
        assert_eq!(run(root.path(), &[], 1)["imported"], 1);
    }
    root
}

fn finding<'a>(report: &'a Value, rule: &str) -> &'a Value {
    let listed = report["refusals"]["listed"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{report}");
    assert_eq!(report["refusals"]["more"], 0);
    assert_eq!(listed[0]["rule"], rule, "{report}");
    &listed[0]
}

#[test]
fn c_v2_to_v6_v9_v12_v13_refusals_match_in_both_modes_without_writes() {
    for existing in [false, true] {
        for (input, rule, legacy) in [
            ("", "documents", true),
            (" \n  \n", "documents", true),
            ("# synthetic config\n", "documents", true),
            ("\t\n", "unreadable", false),
            ("null\n", "unreadable", true),
            ("~\n", "unreadable", true),
            ("---\n", "unreadable", true),
            ("{}\n---\n{}\n", "documents", false),
            ("42\n", "unreadable", true),
            ("[1]\n", "unreadable", true),
            ("\"x\"\n", "unreadable", true),
            ("...\n", "unreadable", false),
        ] {
            let root = prepare(existing);
            put(root.path(), "config.yml", input);
            let report = refusal_parity(root.path(), existing);
            let f = finding(&report, rule);
            assert_eq!(f["path"], ".orchestry/config.yml");
            assert_eq!(f["detail"].as_str().unwrap().contains("1.0 read"), legacy);
            if rule == "documents" {
                let documents = if input.contains("---") { "2" } else { "0" };
                assert_eq!(f["limit"], 1);
                assert_eq!(f["observed"], documents);
                if documents == "0" {
                    assert!(
                        f["detail"]
                            .as_str()
                            .unwrap()
                            .contains("expected one document, found 0")
                    );
                }
            }
        }
    }
}

fn notes<'a>(report: &'a Value, rule: &'a str) -> impl Iterator<Item = &'a Value> {
    report["notes"]["listed"]
        .as_array()
        .unwrap()
        .iter()
        .filter(move |f| f["rule"] == rule)
}

#[test]
fn c_v1_v7_v8_v14_defaults_and_explicit_caps_match_before_template_writes() {
    for (input, cap, inherited) in [
        (None, 6, true),
        (Some("{}\n"), 6, true),
        (Some("project: {name: p}\n"), 6, true),
        (Some("scheduling: {max_concurrent_agents: 2}\n"), 2, false),
        (Some("scheduling: {max_concurrent_agents: 6}\n"), 6, false),
    ] {
        let root = fixture();
        if let Some(input) = input {
            put(root.path(), "config.yml", input);
        }
        let before = snapshot(&root.path().join("source"), true);
        let dry = run(root.path(), &["--dry-run"], 0);
        assert!(!root.path().join("data").exists());
        let write = run(root.path(), &[], 1);
        assert_eq!(dry["preflight"], write["preflight"]);
        assert_eq!(dry["preflight"]["run_cap"], cap);
        assert_eq!(
            notes(&dry["preflight"], "config_default").count(),
            usize::from(inherited)
        );
        assert_eq!(dry["preflight"]["refusals"]["listed"], json!([]));
        assert_eq!(write["imported"], 1);
        assert_eq!(write["unfinished"], true);
        assert_eq!(snapshot(&root.path().join("source"), true), before);
    }
}

fn entity(store: &str, id: &str) -> Value {
    let time = "2025-01-01T00:00:00.000Z";
    // Match the real typed fixtures in import_preflight_compat, including the
    // manual agent policy so acceptance tests do not ask for preapproval.
    match store {
        "tasks" => {
            json!({"id":id,"title":id,"description":"d","status":"todo","priority":1,"created_at":time,"updated_at":time,"attempts":0,"max_attempts":3})
        }
        "agents" => {
            json!({"id":id,"name":id,"adapter":"shell","role":"r","status":"idle","created_at":time,"updated_at":time,"config":{"approval_policy":"manual"},"stats":{"tasks_completed":0,"tasks_failed":0,"total_runs":0,"total_runtime_ms":0}})
        }
        "goals" => {
            json!({"id":id,"title":id,"description":"d","status":"active","created_at":time,"updated_at":time})
        }
        "messages" => {
            json!({"id":id,"channel":"broadcast","from_agent_id":"cli","to_agent_id":null,"subject":id,"body":"m","created_at":time,"status":"pending"})
        }
        "context" => json!({"key":id,"value":id,"created_at":time,"updated_at":time}),
        _ => unreachable!(),
    }
}

fn populate(root: &Path, store: &str) {
    let ext = if matches!(store, "messages" | "context") {
        "json"
    } else {
        "yml"
    };
    for id in ["c", "a", "b"] {
        put(
            root,
            &format!("{store}/{id}.{ext}"),
            entity(store, id).to_string(),
        );
    }
}

#[test]
fn x_v4_to_v7_bad_entries_precede_both_duplicate_shapes_without_writes() {
    for existing in [false, true] {
        for store in STORES {
            let key = if store == "context" { "key" } else { "id" };
            for (input, rule, observed) in [
                (json!([0]), "index_entry", Value::Null),
                (json!([{}]), "index_entry", Value::Null),
                (json!([{key:1}]), "index_entry", Value::Null),
                (json!([{key:"b"},{key:"b"},0]), "index_entry", Value::Null),
                (json!([0,{key:"b"},{key:"b"}]), "index_entry", Value::Null),
                (json!([{key:"b"},{key:"b"}]), "index_duplicate", json!("b")),
                (
                    json!([entity(store, "b"), entity(store, "b")]),
                    "index_duplicate",
                    json!("b"),
                ),
                (json!([{key:"z"},{key:"z"}]), "index_duplicate", json!("z")),
            ] {
                let root = prepare(existing);
                populate(root.path(), store);
                put(
                    root.path(),
                    &format!("{store}/_index.json"),
                    input.to_string(),
                );
                let report = refusal_parity(root.path(), existing);
                let f = finding(&report, rule);
                assert_eq!(f["path"], format!(".orchestry/{store}/_index.json"));
                assert_eq!(f["limit"], Value::Null);
                assert_eq!(f["observed"], observed);
                let detail = f["detail"].as_str().unwrap();
                if rule == "index_duplicate" {
                    assert!(detail.contains("only if every entry was a complete entity"));
                    assert!(detail.contains("otherwise rebuilt"));
                } else {
                    assert!(detail.contains("1.0 rebuilt"));
                }
            }
        }
    }
}

#[test]
fn x_v1_to_v3_v8_to_v11_index_order_is_previewed_identically_before_template_writes() {
    for store in STORES {
        let key = if store == "context" { "key" } else { "id" };
        for (input, expected, unreadable) in [
            (None, vec!["a", "b", "c"], true),
            (Some("[".to_owned()), vec!["a", "b", "c"], true),
            (Some("{}".to_owned()), vec!["a", "b", "c"], true),
            (Some("null".to_owned()), vec!["a", "b", "c"], true),
            (Some("[]".to_owned()), vec!["a", "b", "c"], false),
            (
                Some(json!([{key:"A"},{key:"a"}]).to_string()),
                vec!["a", "b", "c"],
                false,
            ),
            (
                Some(json!([{key:"b"},{key:"zzz"}]).to_string()),
                vec!["b", "a", "c"],
                false,
            ),
            (
                Some(json!([{key:"../x"}]).to_string()),
                vec!["a", "b", "c"],
                false,
            ),
        ] {
            let root = fixture();
            populate(root.path(), store);
            if let Some(input) = input {
                put(root.path(), &format!("{store}/_index.json"), input);
            }
            let before = snapshot(&root.path().join("source"), true);
            let dry = run(root.path(), &["--dry-run"], 0);
            assert!(!root.path().join("data").exists());
            let write = run(root.path(), &[], 1);
            assert_eq!(dry["preflight"], write["preflight"]);
            let report = &dry["preflight"];
            let order: Vec<_> = report["order"]["listed"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["store"] == store)
                .map(|row| row["id"].as_str().unwrap())
                .collect();
            assert_eq!(order, expected, "{report}");
            assert_eq!(
                notes(report, "index_unreadable").count(),
                usize::from(unreadable)
            );
            assert_eq!(snapshot(&root.path().join("source"), true), before);
            assert_eq!(write["imported"], 1);
        }
    }
}

#[test]
fn c_v10_and_x_v12_byte_limits_refuse_before_creating_or_changing_destination() {
    for existing in [false, true] {
        for (path, bytes, limit) in [
            ("config.yml", vec![b'#'; 1_048_577], 1_048_576),
            ("tasks/_index.json", vec![b' '; 16_777_217], 16_777_216),
        ] {
            let root = prepare(existing);
            put(root.path(), path, bytes);
            let report = refusal_parity(root.path(), existing);
            let f = finding(&report, "bytes");
            assert_eq!(f["path"], format!(".orchestry/{path}"));
            assert_eq!(f["limit"], limit);
            assert_eq!(f["observed"], (limit + 1).to_string());
            assert_eq!(notes(&report, "index_unreadable").count(), 0);
        }
    }
}

#[test]
fn r111_adjacent_blank_and_malformed_lines_form_one_gap_in_both_mode_reports() {
    let event = r#"{"timestamp":"2025-01-01T00:00:00.000Z","type":"done","data":"synthetic"}"#;
    for blank in ["", " ", "\u{2003}", "\u{00a0}"] {
        let root = fixture();
        put(
            root.path(),
            "runs/run_synthetic.jsonl",
            format!("{event}\r\n{blank}\r\n{blank}\r\ninvalid\r\n{event}\r\n"),
        );
        let before = snapshot(&root.path().join("source"), true);
        let dry = run(root.path(), &["--dry-run"], 0);
        let write = run(root.path(), &[], 1);
        assert_eq!(dry["preflight"], write["preflight"]);
        assert_eq!(dry["preflight"]["counts"]["run_events"], 2);
        assert_eq!(dry["preflight"]["counts"]["run_event_gaps"], 1);
        assert_eq!(snapshot(&root.path().join("source"), true), before);
    }
}

#[test]
fn c_v11_empty_typed_documents_keep_zero_document_wording_without_writes() {
    for existing in [false, true] {
        for store in ["tasks", "agents", "goals", "teams"] {
            let root = prepare(existing);
            put(root.path(), &format!("{store}/empty.yml"), "");
            let report = refusal_parity(root.path(), existing);
            let f = finding(&report, "documents");
            assert_eq!(f["path"], format!(".orchestry/{store}/empty.yml"));
            assert_eq!(f["observed"], "0");
            let detail = f["detail"].as_str().unwrap();
            assert!(detail.contains("expected one document, found 0"));
            assert!(!detail.contains("1.0 read"));
        }
    }
}

#[test]
fn x_v14_symlinks_fifos_and_hard_links_are_terminal_prewrite_refusals() {
    use std::os::unix::fs::symlink;
    for existing in [false, true] {
        for kind in ["symlink", "fifo", "hardlink"] {
            let root = prepare(existing);
            let target = root.path().join("target.json");
            fs::write(&target, "[]").unwrap();
            let index = root.path().join("source/.orchestry/tasks/_index.json");
            fs::create_dir_all(index.parent().unwrap()).unwrap();
            match kind {
                "symlink" => symlink(&target, &index).unwrap(),
                "hardlink" => fs::hard_link(&target, &index).unwrap(),
                "fifo" => nix::unistd::mkfifo(&index, nix::sys::stat::Mode::S_IRUSR).unwrap(),
                _ => unreachable!(),
            }
            let target_before = snapshot(&target, false);
            let report = refusal_parity(root.path(), existing);
            assert_eq!(
                finding(&report, "file_kind")["path"],
                ".orchestry/tasks/_index.json"
            );
            assert_eq!(notes(&report, "index_unreadable").count(), 0);
            assert_eq!(snapshot(&target, false), target_before);
        }
    }
}

#[test]
fn hostile_duplicate_diagnostics_are_bounded_and_escaped_in_both_modes() {
    for existing in [false, true] {
        let root = prepare(existing);
        let identity = format!("../\u{1b}[31m\n{}", "x".repeat(2_000));
        put(
            root.path(),
            "tasks/_index.json",
            json!([{"id":identity},{"id":identity}]).to_string(),
        );
        let report = refusal_parity(root.path(), existing);
        let f = finding(&report, "index_duplicate");
        let observed = f["observed"].as_str().unwrap();
        assert!(observed.len() < 1_100, "{observed}");
        assert!(!observed.contains('\u{1b}'));
        assert!(!observed.contains('\n'));
        assert!(observed.contains("\\u{1b}"));
        assert_eq!(f["path"], ".orchestry/tasks/_index.json");
    }
}

#[test]
fn tab_led_content_vertical_tab_escape_and_bom_tab_refuse_without_legacy_defaults() {
    for existing in [false, true] {
        for input in [
            "\t{}\n",
            "\tproject: {name: p}\n",
            "\u{b}{}\n",
            "\u{1b}{}\n",
            "\u{feff}\t{}\n",
        ] {
            let root = prepare(existing);
            put(root.path(), "config.yml", input);
            let report = refusal_parity(root.path(), existing);
            let f = finding(&report, "unreadable");
            assert_eq!(f["path"], ".orchestry/config.yml");
            assert!(!f["detail"].as_str().unwrap().contains("1.0 read"));
            assert_eq!(report["run_cap"], Value::Null);
        }
    }
}

#[test]
fn terminal_index_open_error_is_io_and_never_an_unreadable_index() {
    use std::os::unix::fs::PermissionsExt;
    for existing in [false, true] {
        let root = prepare(existing);
        put(root.path(), "tasks/_index.json", "[]");
        let index = root.path().join("source/.orchestry/tasks/_index.json");
        // Unlike the unit-level mid-stream injected error, this exercises the
        // real confined open in the CLI. Restore mode only after all assertions.
        fs::set_permissions(&index, fs::Permissions::from_mode(0o000)).unwrap();
        // The snapshot cannot read the intentionally unreadable file. Keep the
        // metadata and inode separately, and use the kernel error as the input.
        let meta = fs::metadata(&index).unwrap();
        let before = snapshot(&root.path().join("data"), false);
        let dry_args: &[&str] = if existing {
            &["--resume", "--dry-run"]
        } else {
            &["--dry-run"]
        };
        let write_args: &[&str] = if existing { &["--resume"] } else { &[] };
        let dry = run(root.path(), dry_args, 2);
        let write = run(root.path(), write_args, 2);
        assert_eq!(dry["preflight"], write["preflight"]);
        assert_eq!(
            finding(&dry["preflight"], "io")["path"],
            ".orchestry/tasks/_index.json"
        );
        assert_eq!(notes(&dry["preflight"], "index_unreadable").count(), 0);
        assert_eq!(snapshot(&root.path().join("data"), false), before);
        assert_eq!(dry["imported"], 0);
        assert_eq!(write["unfinished"], false);
        let after = fs::metadata(&index).unwrap();
        assert_eq!(
            (after.ino(), after.mode(), after.ctime(), after.ctime_nsec()),
            (meta.ino(), meta.mode(), meta.ctime(), meta.ctime_nsec())
        );
        fs::set_permissions(&index, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read(index).unwrap(), b"[]");
    }
}

#[test]
fn dry_run_auto_policy_is_a_confirmation_preview_and_creates_nothing() {
    let root = fixture();
    let mut agent = entity("agents", "a");
    agent["config"]["approval_policy"] = json!("auto");
    put(root.path(), "agents/a.yml", agent.to_string());
    let before = snapshot(&root.path().join("source"), true);
    let dry = run(root.path(), &["--dry-run"], 0);
    let report = &dry["preflight"];
    assert_eq!(report["preapproval_requires_confirmation"], true);
    assert_eq!(notes(report, "preapproval_confirmation").count(), 1);
    assert_eq!(report["refusals"]["listed"], json!([]));
    assert_eq!(dry["imported"], 0);
    assert!(!root.path().join("data").exists());
    assert_eq!(snapshot(&root.path().join("source"), true), before);
}

#[test]
fn x_v12_depth_and_node_boundaries_keep_budget_precedence_in_both_modes() {
    for existing in [false, true] {
        for (input, rule) in [
            (
                format!("{}0{}", "[".repeat(64), "]".repeat(64)),
                "index_entry",
            ),
            (format!("{}0{}", "[".repeat(65), "]".repeat(65)), "depth"),
            (format!("[{}0]", "0,".repeat(999_998)), "index_entry"),
            (format!("[{}0]", "0,".repeat(999_999)), "nodes"),
        ] {
            let root = prepare(existing);
            put(root.path(), "tasks/_index.json", input);
            let report = refusal_parity(root.path(), existing);
            let f = finding(&report, rule);
            assert_eq!(f["path"], ".orchestry/tasks/_index.json");
            assert_eq!(notes(&report, "index_unreadable").count(), 0);
            match rule {
                "depth" => assert_eq!(f["limit"], 64),
                "nodes" => assert_eq!(f["limit"], 1_000_000),
                _ => assert_eq!(f["limit"], Value::Null),
            }
        }
    }
}
