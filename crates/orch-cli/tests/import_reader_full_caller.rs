//! Issue 146: reviewed reader vectors extended to persisted native rows.
//! Synthetic inputs only. The candidate remains unfinished; this does not prove
//! state/events/attachments, policy activation, R66, R121 or R125 acceptance.
//! Retained-payload validation-loop isolation belongs to the infra unit test
//! `prewrite_validation_detects_native_drift_and_never_creates_unseen_rows`;
//! this file checks observable refusal and couples its EIO fault to read count.
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

// Same observer contract as import_offline: observe file bytes with O_NOATIME;
// complete directory listing before recording metadata. Source access times are
// excluded, since ordinary source reads may advance them. Destination metadata
// includes access time. Never repair a timestamp to make a check pass.
type Snapshot = BTreeMap<PathBuf, (Vec<u8>, Vec<i128>)>;
fn snapshot(root: &Path, source: bool) -> Snapshot {
    fn walk(base: &Path, path: &Path, source: bool, out: &mut Snapshot) {
        let mut bytes = Vec::new();
        let kind = fs::symlink_metadata(path).unwrap().file_type();
        if kind.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                walk(base, &entry.unwrap().path(), source, out);
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
        assert_eq!(run(root.path(), &[], 1)["imported"], 20);
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
fn c_v1_v7_v8_v14_defaults_and_explicit_caps_are_persisted() {
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
        assert_eq!(write["imported"], 20);
        let settings = observe(root.path())["project_config"].clone();
        assert_eq!(settings["run_cap"], cap);
        assert_eq!(
            settings["project.name"],
            if input == Some("project: {name: p}\n") {
                "p"
            } else {
                "my-project"
            }
        );
        assert_eq!(settings["defaults.agent.max_turns"], 50);
        assert_eq!(settings["project.description"], Value::Null);
        assert_eq!(settings["legacy.config"], json!({}));
        assert!(settings.get("scheduling.max_concurrent_agents").is_none());
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
fn x_v1_to_v3_v8_to_v11_index_order_is_persisted_in_every_family() {
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
            assert_eq!(write["imported"], 23);
            assert_family(root.path(), store, &expected);
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
    if nix::unistd::geteuid().is_root() {
        eprintln!("mode-000 EACCES fixture requires an unprivileged process");
        return;
    }
    for existing in [false, true] {
        let root = prepare(existing);
        put(root.path(), "tasks/_index.json", "[]");
        let index = root.path().join("source/.orchestry/tasks/_index.json");
        fs::set_permissions(&index, fs::Permissions::from_mode(0o000)).unwrap();
        // A capability-bearing process can bypass mode 000 (issue 533).
        // Ordinary users must actually observe the intended kernel EACCES.
        match fs::File::open(&index) {
            Ok(_) => {
                fs::set_permissions(&index, fs::Permissions::from_mode(0o600)).unwrap();
                eprintln!("mode-000 scenario unavailable to a privileged reader");
                continue;
            }
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied),
        }
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
fn auto_policy_requires_owner_confirmation_before_either_mode_can_write() {
    let root = fixture();
    let mut agent = entity("agents", "a");
    agent["config"]["approval_policy"] = json!("auto");
    put(root.path(), "agents/a.yml", agent.to_string());
    let before = snapshot(&root.path().join("source"), true);
    for args in [&["--dry-run"][..], &[][..]] {
        let result = run(root.path(), args, 2);
        assert_eq!(
            result["preflight"]["preapproval_requires_confirmation"],
            true
        );
        assert!(
            result["detail"]
                .as_str()
                .unwrap()
                .contains("owner confirmation")
        );
        assert_eq!(
            notes(&result["preflight"], "preapproval_confirmation").count(),
            1
        );
        assert_eq!(result["preflight"]["refusals"]["listed"], json!([]));
        assert_eq!(result["imported"], 0);
        assert_eq!(result["unfinished"], false);
        assert!(!root.path().join("data").exists());
        assert!(!root.path().join("source/.orch-project").exists());
        assert_eq!(snapshot(&root.path().join("source"), true), before);
    }
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

// Fresh, isolated Python processes observe committed SQLite state only after the
// real importer exits. This matches import_offline without adding dependencies.
fn python(root: &Path) -> Command {
    let mut command = Command::new("python3");
    command
        .env_clear()
        .env("HOME", root.join("home"))
        .env("TMPDIR", root)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("XDG_CACHE_HOME", root.join("xdg"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .arg("-I");
    command
}
fn database(root: &Path) -> PathBuf {
    fs::read_dir(root.join("data"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("orch.db").is_file())
        .unwrap()
        .join("orch.db")
}
fn sql(root: &Path, script: &str) -> Value {
    let output = python(root)
        .args(["-c", script])
        .arg(database(root))
        .output()
        .expect("Linux importer tests require Python 3 with sqlite3");
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}
fn observe(root: &Path) -> Value {
    sql(
        root,
        r#"import sqlite3,sys,json
c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True)
s={}
for table in ['tasks','agents','goals','messages','context_entries','templates']:
 cur=c.execute('SELECT * FROM '+table+' ORDER BY position')
 keys=[x[0] for x in cur.description]
 s[table]=[dict(zip(keys,r)) for r in cur.fetchall()]
s['project_config']={k:json.loads(v) for k,v in c.execute('SELECT key,value FROM project_config')}
s['config_revisions']=list(c.execute('SELECT key,revision FROM project_config ORDER BY key'))
s['manifest']=list(c.execute('SELECT entity,entity_id,source_sha256 FROM import_manifest ORDER BY entity,entity_id'))
s['progress']=list(c.execute("SELECT key,value,revision FROM daemon_config WHERE key LIKE 'import:%' ORDER BY key"))
print(json.dumps(s));c.close()"#,
    )
}
fn assert_family(root: &Path, store: &str, expected: &[&str]) {
    let db = observe(root);
    let table = if store == "context" {
        "context_entries"
    } else {
        store
    };
    let key = if store == "context" { "key" } else { "id" };
    let rows = db[table].as_array().unwrap();
    let actual: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r[key].as_str().unwrap(),
                r["position"].as_u64().unwrap(),
                r["revision"].as_u64().unwrap(),
            )
        })
        .collect();
    let expected: Vec<_> = expected
        .iter()
        .enumerate()
        .map(|(i, id)| (*id, i as u64 + 1, 1))
        .collect();
    assert_eq!(actual, expected, "{store}");
}
fn full_fixture() -> tempfile::TempDir {
    let root = fixture();
    for store in STORES {
        populate(root.path(), store);
        let key = if store == "context" { "key" } else { "id" };
        put(
            root.path(),
            &format!("{store}/_index.json"),
            json!([{key:"b","title":"stale index","value":"stale index"},{key:"gone"},{key:"a"}])
                .to_string(),
        );
    }
    root
}
#[test]
fn native_values_and_dense_positions_survive_writer_shutdown_and_unchanged_resume() {
    let root = full_fixture();
    let before = snapshot(&root.path().join("source"), true);
    assert_eq!(run(root.path(), &[], 1)["imported"], 35);
    for store in STORES {
        assert_family(root.path(), store, &["b", "a", "c"]);
    }
    let first = observe(root.path());
    assert_eq!(first["tasks"][0]["title"], "b");
    assert_eq!(first["tasks"][0]["description"], "d");
    assert_eq!(first["agents"][0]["name"], "b");
    assert_eq!(first["goals"][0]["title"], "b");
    assert_eq!(first["messages"][0]["body"], "m");
    assert_eq!(first["context_entries"][0]["value"], "b");
    assert_eq!(first["manifest"].as_array().unwrap().len(), 35);
    assert_eq!(run(root.path(), &["--resume"], 1)["imported"], 35);
    let mut resumed = observe(root.path());
    let mut expected = first;
    // Replaying retained rows advances unfinished progress revisions, not entities.
    resumed.as_object_mut().unwrap().remove("progress");
    expected.as_object_mut().unwrap().remove("progress");
    assert_eq!(resumed, expected);
    assert_eq!(snapshot(&root.path().join("source"), true), before);
}
#[test]
fn known_config_values_keep_json_types_and_unknown_policy_keys_stay_inert() {
    let root = fixture();
    put(
        root.path(),
        "config.yml",
        "project: {name: synthetic, description: retained}\nscheduling: {max_concurrent_agents: 2, poll_interval_ms: 1234}\ndefaults: {agent: {max_turns: 7}, task: {priority: 1}}\nprompt: {template: custom, extension: inert}\npreapproval: true\nrun_cap: 999\ncustom: {values: [true, null, 42]}\n",
    );
    let before = snapshot(&root.path().join("source"), true);
    let dry = run(root.path(), &["--dry-run"], 0);
    assert!(!root.path().join("data").exists());
    let write = run(root.path(), &[], 1);
    assert_eq!(dry["preflight"], write["preflight"]);
    let db = observe(root.path());
    let config = &db["project_config"];
    assert_eq!(config["run_cap"], 2);
    assert_eq!(config["project.name"], "synthetic");
    assert_eq!(config["project.description"], "retained");
    assert_eq!(config["defaults.agent.max_turns"], 7);
    assert_eq!(config["defaults.task.priority"], 1);
    assert_eq!(config["scheduling.poll_interval_ms"], 1234);
    assert_eq!(config["prompt.template"], "custom");
    assert_eq!(
        config["legacy.config"],
        json!({"preapproval":true,"run_cap":999,"prompt":{"extension":"inert"},"custom":{"values":[true,null,42]}})
    );
    assert!(!config.as_object().unwrap().contains_key("preapproval"));
    assert!(
        !config
            .as_object()
            .unwrap()
            .contains_key("scheduling.max_concurrent_agents")
    );
    assert_eq!(snapshot(&root.path().join("source"), true), before);
}
#[test]
fn changed_retained_order_or_native_payload_refuses_resume_before_new_rows() {
    for changed in ["order", "native", "source"] {
        let root = full_fixture();
        run(root.path(), &[], 1);
        put(
            root.path(),
            "tasks/zz_new.yml",
            entity("tasks", "zz_new").to_string(),
        );
        match changed {
            "order" => put(
                root.path(),
                "tasks/_index.json",
                json!([{"id":"a"},{"id":"b"}]).to_string(),
            ),
            "source" => {
                let mut e = entity("tasks", "b");
                e["description"] = json!("changed");
                put(root.path(), "tasks/b.yml", e.to_string());
            }
            "native" => {
                sql(
                    root.path(),
                    r#"import sqlite3,sys,json
c=sqlite3.connect(sys.argv[1]);c.execute("UPDATE project_config SET value='999' WHERE key='run_cap'");c.commit();c.close();print('null')"#,
                );
            }
            _ => unreachable!(),
        }
        let before = observe(root.path());
        let source = snapshot(&root.path().join("source"), true);
        assert_eq!(run(root.path(), &["--resume"], 1)["imported"], 0);
        assert_eq!(observe(root.path()), before);
        assert_eq!(snapshot(&root.path().join("source"), true), source);
    }
}
#[test]
fn resumed_append_matches_uninterrupted_logical_entities_and_manifest_hashes() {
    let root = full_fixture();
    run(root.path(), &[], 1);
    put(
        root.path(),
        "tasks/zz_new.yml",
        entity("tasks", "zz_new").to_string(),
    );
    run(root.path(), &["--resume"], 1);
    assert_family(root.path(), "tasks", &["b", "a", "c", "zz_new"]);
    let resumed = observe(root.path());
    // Re-run the same immutable source at the same canonical path into another
    // private destination. Project identity and imported source hashes then agree.
    fs::rename(root.path().join("data"), root.path().join("resumed-data")).unwrap();
    run(root.path(), &[], 1);
    let fresh = observe(root.path());
    for table in [
        "tasks",
        "agents",
        "goals",
        "messages",
        "context_entries",
        "templates",
        "project_config",
        "config_revisions",
        "manifest",
    ] {
        assert_eq!(resumed[table], fresh[table], "{table}");
    }
}
#[test]
fn committed_entities_and_manifest_rollback_together_when_manifest_insert_fails() {
    let root = full_fixture();
    run(root.path(), &[], 1);
    put(
        root.path(),
        "tasks/zz_new.yml",
        entity("tasks", "zz_new").to_string(),
    );
    // Fail after the native entity INSERT, inside the same real writer transaction.
    sql(
        root.path(),
        r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1]);c.execute("CREATE TRIGGER fail_manifest BEFORE INSERT ON import_manifest WHEN NEW.entity_id='zz_new' BEGIN SELECT RAISE(ABORT,'synthetic manifest failure'); END");c.commit();c.close();print('null')"#,
    );
    let before = observe(root.path());
    let outcome = run(root.path(), &["--resume"], 1);
    assert!(
        outcome["detail"]
            .as_str()
            .unwrap()
            .contains("batch did not confirm")
    );
    let after = observe(root.path());
    for table in [
        "tasks",
        "agents",
        "goals",
        "messages",
        "context_entries",
        "templates",
        "project_config",
        "config_revisions",
        "manifest",
    ] {
        assert_eq!(after[table], before[table], "{table}");
    }
    // The failing batch must not advance progress (earlier retained batches may).
    let old = before["progress"][0][2].as_u64().unwrap();
    let new = after["progress"][0][2].as_u64().unwrap();
    assert_eq!(new - old, outcome["imported"].as_u64().unwrap());
    sql(
        root.path(),
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('DROP TRIGGER fail_manifest'); c.commit(); c.close(); print('null')",
    );
    run(root.path(), &["--resume"], 1);
    assert_family(root.path(), "tasks", &["b", "a", "c", "zz_new"]);
}

#[test]
fn terminal_entity_read_after_commits_retains_only_confirmed_rows_and_resumes() {
    // Fault the genuine third File::read of one inode: check, validation, then
    // writing load. No production seam, scheduling delay or source mutation.
    let root = full_fixture();
    let c = root.path().join("read-error.c");
    let library = root.path().join("read-error.so");
    fs::write(
        &c,
        r#"
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/stat.h>
static ssize_t (*next_read)(int,void*,size_t);
static unsigned int seen;
__attribute__((constructor)) static void initialize(void) { next_read=dlsym(RTLD_NEXT,"read"); }
ssize_t read(int fd,void *buf,size_t count) {
 const char *target=getenv("IMPORT_FAULT_FILE");
 struct stat actual,expected;
 if(target && fstat(fd,&actual)==0 && stat(target,&expected)==0 &&
    actual.st_dev==expected.st_dev && actual.st_ino==expected.st_ino &&
    lseek(fd,0,SEEK_CUR)==0 && ++seen==3) {
   int marker=open(getenv("IMPORT_FAULT_MARKER"),O_WRONLY|O_CREAT|O_EXCL,0600);
   if(marker>=0) close(marker);
   errno=EIO; return -1;
 }
 return next_read(fd,buf,count);
}
"#,
    )
    .unwrap();
    let compiled = Command::new("cc")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.path().join("home"))
        .env("TMPDIR", root.path())
        .args(["-shared", "-fPIC", "-Wall", "-Wextra", "-Werror"])
        .arg(&c)
        .args(["-ldl", "-o"])
        .arg(&library)
        .output()
        .expect("Linux terminal-read regression requires cc");
    assert!(compiled.status.success(), "{compiled:?}");
    let marker = root.path().join("faulted");
    let source = snapshot(&root.path().join("source"), true);
    let output = Command::new(env!("CARGO_BIN_EXE_orch"))
        .env_clear()
        .env("HOME", root.path().join("home"))
        .env("TMPDIR", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("xdg"))
        .env("XDG_CACHE_HOME", root.path().join("xdg"))
        .env("XDG_RUNTIME_DIR", root.path().join("run"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            root.path().join("global.yml"),
        )
        .env("ORCH_DATA_HOME", root.path().join("data"))
        .env("ORCH_SOCKET", root.path().join("socket"))
        .env("LD_PRELOAD", &library)
        .env(
            "IMPORT_FAULT_FILE",
            root.path().join("source/.orchestry/tasks/b.yml"),
        )
        .env("IMPORT_FAULT_MARKER", &marker)
        .current_dir(root.path())
        .args(["--json", "import", "--from"])
        .arg(root.path().join("source"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(marker.exists(), "real read fault was never reached");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["unfinished"], true);
    let imported = result["imported"].as_u64().unwrap();
    assert!(imported > 0 && imported < 35, "{result}");
    let partial = observe(root.path());
    assert_eq!(
        partial["manifest"].as_array().unwrap().len(),
        imported as usize
    );
    assert!(
        !partial["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "b")
    );
    assert!(
        !partial["manifest"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r[0] == "task" && r[1] == "b")
    );
    assert!(!root.path().join("source/.orch-project").exists());
    assert_eq!(snapshot(&root.path().join("source"), true), source);
    assert_eq!(run(root.path(), &["--resume"], 1)["imported"], 35);
    let resumed = observe(root.path());
    fs::rename(root.path().join("data"), root.path().join("resumed-data")).unwrap();
    run(root.path(), &[], 1);
    let fresh = observe(root.path());
    for table in [
        "tasks",
        "agents",
        "goals",
        "messages",
        "context_entries",
        "templates",
        "project_config",
        "config_revisions",
        "manifest",
    ] {
        assert_eq!(resumed[table], fresh[table], "{table}");
    }
    assert_eq!(snapshot(&root.path().join("source"), true), source);
}

#[test]
fn durable_order_includes_entities_beyond_the_thousand_row_report_preview() {
    let root = fixture();
    let ids: Vec<_> = (0..1002).rev().map(|i| format!("key{i:04}")).collect();
    for id in &ids {
        put(
            root.path(),
            &format!("context/{id}.json"),
            entity("context", id).to_string(),
        );
    }
    put(
        root.path(),
        "context/_index.json",
        json!(ids.iter().map(|id| json!({"key":id})).collect::<Vec<_>>()).to_string(),
    );
    let dry = run(root.path(), &["--dry-run"], 0);
    assert_eq!(
        dry["preflight"]["order"]["listed"]
            .as_array()
            .unwrap()
            .len(),
        1000
    );
    assert_eq!(dry["preflight"]["order"]["more"], 2);
    let write = run(root.path(), &[], 1);
    assert_eq!(dry["preflight"], write["preflight"]);
    assert_eq!(write["imported"], 1022);
    let expected: Vec<_> = ids.iter().map(String::as_str).collect();
    assert_family(root.path(), "context", &expected);
}
