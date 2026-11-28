//! Synthetic command-level contract for the bounded, unfinished first caller.
use std::path::Path;
use std::process::{Command, Output};

// This fixture has no config.yml. Frozen preflight defaults contribute 18
// settings: project (2), agent (7), task (2), scheduling (4), prompt (1),
// peer (2), including optional nulls. The run cap replaces its scheduling key;
// the inert legacy.config row adds one, and the fixture has one template.
const DEFAULT_CONFIG_ROWS: usize = 19;
const FIXTURE_ROWS: usize = DEFAULT_CONFIG_ROWS + 1;

fn run(root: &Path, args: &[&str]) -> Output {
    run_at_home(root, args, &root.join("data"))
}
fn run_at_home(root: &Path, args: &[&str], data_home: &Path) -> Output {
    command_at_home(root, args, data_home).output().unwrap()
}
fn command_at_home(root: &Path, args: &[&str], data_home: &Path) -> Command {
    let source = root.join("source");
    let mut command = Command::new(env!("CARGO_BIN_EXE_orch"));
    command
        .env_clear()
        .env("HOME", root.join("home"))
        .env("TMPDIR", root)
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("XDG_CACHE_HOME", root.join("xdg"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", root.join("global.yml"))
        .env("ORCH_DATA_HOME", data_home)
        .env("ORCH_SOCKET", root.join("socket"))
        .current_dir(root)
        .args(["--json", "import", "--from"])
        .arg(source)
        .args(args);
    command
}
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("source/.orchestry/templates")).unwrap();
    std::fs::write(
        root.path().join("source/.orchestry/templates/.md"),
        "synthetic body",
    )
    .unwrap();
    root
}
#[test]
fn dry_run_is_reachable_without_a_daemon_and_creates_nothing() {
    let root = fixture();
    let output = run(root.path(), &["--dry-run"]);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["preflight"]["counts"]["templates"], 1);
    assert_eq!(value["imported"], 0);
    assert!(!root.path().join("data").exists());
    assert!(!root.path().join("source/.orch-project").exists());
    assert!(
        !root
            .path()
            .join("source/.orchestry/.daemon-owner.lock")
            .exists()
    );
}
#[test]
fn non_utf8_template_is_prewrite_refusal_for_both_modes() {
    let root = fixture();
    std::fs::write(root.path().join("source/.orchestry/templates/.md"), [0xff]).unwrap();
    for args in [&["--dry-run"][..], &[][..]] {
        let output = run(root.path(), args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["preflight"]["refusals"]["listed"][0]["rule"], "utf8");
        assert!(!root.path().join("data").exists());
    }
}
#[test]
fn native_template_import_and_resume_are_explicitly_unfinished() {
    let root = fixture();
    for args in [&[][..], &["--resume"][..]] {
        let output = run(root.path(), args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["imported"], FIXTURE_ROWS);
        assert_eq!(value["unfinished"], true);
        assert!(!root.path().join("source/.orch-project").exists());
        assert_eq!(
            std::fs::read(root.path().join("source/.orchestry/templates/.md")).unwrap(),
            b"synthetic body"
        );
    }
}

#[test]
fn refused_resume_and_dry_run_leave_existing_destination_bytes_unchanged() {
    let root = fixture();
    let first = run(root.path(), &[]);
    assert_eq!(first.status.code(), Some(1), "{first:?}");
    let data = std::fs::read_dir(root.path().join("data"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("orch.db").is_file())
        .unwrap();
    let lock = data.join("orch.lock");
    let bytes = std::fs::read(&lock).unwrap();
    std::fs::write(root.path().join("source/.orchestry/templates/.md"), [0xff]).unwrap();
    for args in [&["--resume", "--dry-run"][..], &["--resume"][..]] {
        let output = run(root.path(), args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert_eq!(
            std::fs::read(&lock).unwrap(),
            bytes,
            "preflight must not rewrite existing holder bytes"
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value["preflight"]["refusals"]["listed"][0]["rule"], "utf8",
            "resume must run preflight under a read-only destination guard"
        );
    }
}

#[test]
fn held_legacy_guard_refuses_without_changing_it() {
    use nix::fcntl::{Flock, FlockArg};
    use std::os::unix::fs::MetadataExt;
    let root = fixture();
    let path = root.path().join("source/.orchestry/.daemon-owner.lock");
    std::fs::write(&path, "legacy holder\n").unwrap();
    let before = std::fs::metadata(&path).unwrap();
    let _guard = Flock::lock(
        std::fs::File::open(&path).unwrap(),
        FlockArg::LockExclusiveNonblock,
    )
    .unwrap();
    let output = run(root.path(), &["--dry-run"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(std::fs::read(&path).unwrap(), b"legacy holder\n");
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.ctime(),
            before.ctime_nsec(),
            before.mode()
        ),
        (
            after.dev(),
            after.ino(),
            after.ctime(),
            after.ctime_nsec(),
            after.mode()
        )
    );
    assert!(!root.path().join("data").exists());
}

#[test]
fn absent_guard_fallback_refuses_a_live_socket_without_creating_a_lock() {
    let root = fixture();
    let _listener = std::os::unix::net::UnixListener::bind(root.path().join("socket")).unwrap();
    let output = run(root.path(), &["--dry-run"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        !root
            .path()
            .join("source/.orchestry/.daemon-owner.lock")
            .exists()
    );
    assert!(!root.path().join("data").exists());
}

#[test]
fn whole_source_refusals_match_for_dry_run_and_writer() {
    let root = fixture();
    std::fs::write(root.path().join("source/.orchestry/config.yml"), "null").unwrap();
    let dry = run(root.path(), &["--dry-run"]);
    let writer = run(root.path(), &[]);
    assert_eq!(dry.status.code(), Some(2), "{dry:?}");
    assert_eq!(writer.status.code(), Some(2), "{writer:?}");
    let dry: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    let writer: serde_json::Value = serde_json::from_slice(&writer.stdout).unwrap();
    assert_eq!(dry["preflight"], writer["preflight"]);
    assert_eq!(
        dry["preflight"]["refusals"]["listed"][0]["path"],
        ".orchestry/config.yml"
    );
    assert!(!root.path().join("data").exists());
}

#[test]
fn missing_destination_below_a_source_alias_is_refused_before_creation() {
    let root = fixture();
    std::fs::create_dir(root.path().join("source/ordinary")).unwrap();
    std::os::unix::fs::symlink(root.path().join("source"), root.path().join("alias")).unwrap();
    let output = run_at_home(
        root.path(),
        &[],
        &root.path().join("alias/ordinary/new-data-home"),
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!root.path().join("source/ordinary/new-data-home").exists());
}

// Capture bytes without changing file atime. Directory listings are completed
// before their metadata is recorded, so the observer's own read is not a delta.
#[cfg(target_os = "linux")]
fn snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, (Vec<u8>, Vec<i128>)> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    fn walk(
        base: &Path,
        path: &Path,
        out: &mut std::collections::BTreeMap<std::path::PathBuf, (Vec<u8>, Vec<i128>)>,
    ) {
        let mut bytes = Vec::new();
        if path.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                walk(base, &entry.unwrap().path(), out);
            }
        } else {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOATIME | nix::libc::O_NOFOLLOW)
                .open(path)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
        }
        let m = std::fs::symlink_metadata(path).unwrap();
        let meta = vec![
            m.dev() as i128,
            m.ino() as i128,
            m.mode() as i128,
            m.nlink() as i128,
            m.uid() as i128,
            m.gid() as i128,
            m.size() as i128,
            m.atime() as i128,
            m.atime_nsec() as i128,
            m.mtime() as i128,
            m.mtime_nsec() as i128,
            m.ctime() as i128,
            m.ctime_nsec() as i128,
        ];
        out.insert(path.strip_prefix(base).unwrap().to_owned(), (bytes, meta));
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
#[cfg(target_os = "linux")]
fn existing_dry_run_and_refusal_preserve_all_destination_bytes_metadata_and_inventory() {
    let root = fixture();
    assert_eq!(run(root.path(), &[]).status.code(), Some(1));
    let before = snapshot(&root.path().join("data"));
    assert_eq!(
        run(root.path(), &["--resume", "--dry-run"]).status.code(),
        Some(0)
    );
    assert_eq!(snapshot(&root.path().join("data")), before);
    std::fs::write(root.path().join("source/.orchestry/templates/.md"), [0xff]).unwrap();
    for args in [&["--resume", "--dry-run"][..], &["--resume"][..]] {
        assert_eq!(run(root.path(), args).status.code(), Some(2));
        assert_eq!(snapshot(&root.path().join("data")), before);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn source_bytes_inodes_modes_and_write_times_survive_dry_run_import_resume_and_refusal() {
    let root = fixture();
    std::fs::write(
        root.path().join("source/.orchestry/.daemon-owner.lock"),
        "synthetic legacy\n",
    )
    .unwrap();
    fn source_snapshot(
        root: &Path,
    ) -> std::collections::BTreeMap<std::path::PathBuf, (Vec<u8>, Vec<i128>)> {
        let mut result = snapshot(root);
        // Reading a source can advance kernel atime; never restore it by writing.
        for (_, times) in result.values_mut() {
            times[7] = 0;
            times[8] = 0;
        }
        result
    }
    let before = source_snapshot(&root.path().join("source"));
    for args in [&["--dry-run"][..], &[][..], &["--resume"][..]] {
        let output = run(root.path(), args);
        assert_eq!(
            output.status.code(),
            Some(if args.contains(&"--dry-run") { 0 } else { 1 }),
            "{output:?}"
        );
        assert_eq!(source_snapshot(&root.path().join("source")), before);
    }
    std::fs::write(root.path().join("source/.orchestry/config.yml"), "null").unwrap();
    let injected = source_snapshot(&root.path().join("source"));
    for args in [&["--resume", "--dry-run"][..], &["--resume"][..]] {
        assert_eq!(run(root.path(), args).status.code(), Some(2));
        assert_eq!(source_snapshot(&root.path().join("source")), injected);
    }
}

#[test]
fn missing_data_home_parents_are_created_only_after_preflight() {
    let root = fixture();
    let home = root.path().join("missing/private/daemons");
    assert_eq!(
        run_at_home(root.path(), &["--dry-run"], &home)
            .status
            .code(),
        Some(0)
    );
    assert!(!root.path().join("missing").exists());
    let output = run_at_home(root.path(), &[], &home);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(home.is_dir());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["imported"], FIXTURE_ROWS);
}

fn database(root: &Path) -> std::path::PathBuf {
    std::fs::read_dir(root.join("data"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.join("orch.db").is_file())
        .unwrap()
        .join("orch.db")
}
// Native SQL assertions and the real lock blocker use Linux's Python sqlite3
// without adding a CLI dependency or exposing a production database handle.
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
fn sql(root: &Path, script: &str) -> serde_json::Value {
    let output = python(root)
        .args(["-c", script])
        .arg(database(root))
        .output()
        .expect("Linux importer tests require Python 3 with sqlite3");
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}
fn logical(root: &Path) -> serde_json::Value {
    sql(
        root,
        r#"import sqlite3,sys,json
c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True)
print(json.dumps({
 'templates':c.execute('SELECT name,body,position,revision FROM templates ORDER BY position').fetchall(),
 'manifest':c.execute('SELECT entity,entity_id,source_sha256 FROM import_manifest ORDER BY entity_id').fetchall(),
 'progress':c.execute("SELECT key,value,revision FROM daemon_config WHERE key LIKE 'import:%' ORDER BY key").fetchall()}))
c.close()"#,
    )
}
#[test]
fn native_command_resume_checks_all_retained_hashes_before_admitting_new_rows() {
    let root = fixture();
    let templates = root.path().join("source/.orchestry/templates");
    std::fs::rename(templates.join(".md"), templates.join("z.md")).unwrap();
    assert_eq!(run(root.path(), &[]).status.code(), Some(1));
    let first = logical(root.path());
    assert_eq!(
        first["templates"],
        serde_json::json!([["z", "synthetic body", 1, 1]])
    );
    let manifest = first["manifest"].as_array().unwrap();
    assert_eq!(manifest.len(), FIXTURE_ROWS);
    assert_eq!(
        manifest.iter().filter(|row| row[0] == "config").count(),
        DEFAULT_CONFIG_ROWS
    );
    let template = manifest.iter().find(|row| row[0] == "template").unwrap();
    assert_eq!(template[1], "z");
    assert_eq!(template[2].as_str().unwrap().len(), 64);
    let progress: serde_json::Value =
        serde_json::from_str(first["progress"][0][1].as_str().unwrap()).unwrap();
    assert_eq!(progress["status"], "unfinished");
    assert_eq!(
        progress["source"],
        root.path().join("source").to_str().unwrap()
    );
    assert_eq!(run(root.path(), &["--resume"]).status.code(), Some(1));
    let resumed = logical(root.path());
    assert_eq!(resumed["templates"], first["templates"]);
    assert_eq!(resumed["manifest"], first["manifest"]);
    // A new earlier name must not commit before a changed/missing retained one.
    std::fs::write(templates.join("a.md"), "new row").unwrap();
    std::fs::write(templates.join("z.md"), "changed body").unwrap();
    assert_eq!(run(root.path(), &["--resume"]).status.code(), Some(1));
    assert_eq!(logical(root.path()), resumed);
    std::fs::remove_file(templates.join("z.md")).unwrap();
    assert_eq!(run(root.path(), &["--resume"]).status.code(), Some(1));
    assert_eq!(logical(root.path()), resumed);
    std::fs::write(templates.join("z.md"), "synthetic body").unwrap();
    assert_eq!(run(root.path(), &["--resume"]).status.code(), Some(1));
    let restored = logical(root.path());
    assert_eq!(restored["templates"].as_array().unwrap().len(), 2);
    assert_eq!(
        restored["manifest"].as_array().unwrap().len(),
        FIXTURE_ROWS + 1
    );
    assert_eq!(restored["templates"][0], first["templates"][0]);
    assert!(!root.path().join("source/.orch-project").exists());
}

#[test]
fn native_command_conflict_rolls_back_entity_manifest_and_progress_together() {
    let root = fixture();
    assert_eq!(run(root.path(), &[]).status.code(), Some(1));
    sql(
        root.path(),
        r#"import sqlite3,sys,json
c=sqlite3.connect(sys.argv[1]);c.execute("INSERT INTO templates(project_id,name,body,position,revision) SELECT project_id,'a','external row',1,1 FROM templates LIMIT 1");c.commit();c.close();print('null')"#,
    );
    std::fs::write(
        root.path().join("source/.orchestry/templates/a.md"),
        "source row",
    )
    .unwrap();
    let before = logical(root.path());
    let output = run(root.path(), &["--resume"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let after = logical(root.path());
    // The equal retained empty-name row can advance progress in an earlier
    // batch. The conflicting batch must add no row or manifest of its own.
    assert_eq!(after["templates"], before["templates"]);
    assert_eq!(after["manifest"], before["manifest"]);
    assert_eq!(after["progress"][0][1], before["progress"][0][1]);
    assert_eq!(
        after["progress"][0][2].as_i64().unwrap(),
        before["progress"][0][2].as_i64().unwrap() + 1
    );
}

#[test]
#[cfg(target_os = "linux")]
fn interrupted_project_record_is_not_reinterpreted_as_an_absent_destination() {
    let root = fixture();
    assert_eq!(run(root.path(), &[]).status.code(), Some(1));
    let home = root.path().join("data");
    let record = home.join("projects");
    std::fs::create_dir(&record).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o700)).unwrap();
    use sha2::{Digest, Sha256};
    let project = orch_infra::data_dir::paths::default_name(&root.path().join("source")).unwrap();
    let hash = format!("{:x}", Sha256::digest(project.as_bytes()));
    let record_file = record.join(hash);
    std::fs::write(&record_file, "interrupted").unwrap();
    std::fs::set_permissions(&record_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let before = snapshot(&home);
    for args in [&["--resume", "--dry-run"][..], &["--resume"][..]] {
        let output = run(root.path(), args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        let after = snapshot(&home);
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>()
        );
        for (path, (bytes, metadata)) in &before {
            let (actual_bytes, actual_metadata) = &after[path];
            assert!(actual_bytes == bytes, "bytes changed at {path:?}");
            assert_eq!(actual_metadata, metadata, "metadata changed at {path:?}");
        }
    }
}

#[test]
fn termination_drains_a_blocked_native_operation_before_releasing_both_guards() {
    use nix::fcntl::{Flock, FlockArg};
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = fixture();
    let legacy = root.path().join("source/.orchestry/.daemon-owner.lock");
    std::fs::write(&legacy, "synthetic holder").unwrap();
    assert_eq!(run(root.path(), &[]).status.code(), Some(1));
    let db = database(root.path());
    let lock = db.parent().unwrap().join("orch.lock");
    let mut blocker = Process(
        python(root.path())
            .args([
                "-u",
                "-c",
                r#"import sqlite3,sys
c=sqlite3.connect(sys.argv[1]);c.execute('BEGIN IMMEDIATE');print('locked',flush=True)
sys.stdin.readline();c.rollback();c.close()"#,
            ])
            .arg(&db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Linux importer tests require Python 3 with sqlite3"),
    );
    let mut ready = String::new();
    BufReader::new(blocker.0.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready.trim(), "locked");
    let mut child = Process(
        command_at_home(root.path(), &["--resume"], &root.path().join("data"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let held = |path: &Path| {
        Flock::lock(
            std::fs::File::open(path).unwrap(),
            FlockArg::LockExclusiveNonblock,
        )
        .is_err()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let child_pid = child.0.id();
    let published = || {
        std::fs::read(&lock)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v| v["pid"].as_u64() == Some(child_pid as u64))
    };
    while !published() {
        assert!(
            Instant::now() < deadline,
            "caller never acquired destination guard"
        );
        if let Some(status) = child.0.try_wait().unwrap() {
            use std::io::Read;
            let mut text = String::new();
            child
                .0
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            panic!("caller exited before native operation: {status}: {text}");
        }
        std::thread::yield_now();
    }
    assert!(held(&legacy));
    kill(Pid::from_raw(child.0.id() as i32), Signal::SIGTERM).unwrap();
    assert!(held(&lock));
    assert!(held(&legacy));
    assert!(child.0.try_wait().unwrap().is_none());
    // Releasing the real SQLite write lock permits native startup/admission to
    // finish; the CLI must await that operation and native close after SIGTERM.
    blocker
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(b"release\n")
        .unwrap();
    assert!(blocker.0.wait().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "cancelled caller did not drain");
        std::thread::yield_now();
    };
    assert_eq!(status.code(), Some(1));
    assert!(!held(&lock));
    assert!(!held(&legacy));
    assert!(!root.path().join("source/.orch-project").exists());
    let rows = logical(root.path());
    assert_eq!(rows["templates"].as_array().unwrap().len(), 1);
    assert_eq!(rows["manifest"].as_array().unwrap().len(), FIXTURE_ROWS);
}

#[test]
#[cfg(target_os = "linux")]
fn explicit_destination_never_initializes_an_unused_home_in_the_source() {
    let root = fixture();
    let source = root.path().join("source");
    let home = source.join(".orchestry/unused-data-home");
    let destination = root.path().join("explicit-destination");
    let observe = || {
        let mut rows = snapshot(&source);
        for (_, metadata) in rows.values_mut() {
            metadata[7] = 0;
            metadata[8] = 0;
        }
        rows
    };
    let before = observe();
    for args in [&["--dry-run"][..], &[][..], &["--resume"][..]] {
        let output = command_at_home(root.path(), args, &home)
            .env("ORCH_DATA_DIR", &destination)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if args.contains(&"--dry-run") { 0 } else { 1 }),
            "{output:?}"
        );
        assert!(
            !home.exists(),
            "an explicit destination must not initialize an unused home"
        );
        assert_eq!(observe(), before);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn refused_destination_symlink_preserves_even_its_read_time() {
    use std::os::unix::fs::MetadataExt;
    let root = fixture();
    assert_eq!(run(root.path(), &[]).status.code(), Some(1));
    let destination = database(root.path()).parent().unwrap().to_owned();
    let alias = root.path().join("destination-link");
    std::os::unix::fs::symlink(&destination, &alias).unwrap();
    let output = python(root.path())
        .args([
            "-c",
            "import os,sys; os.utime(sys.argv[1],ns=(1,1),follow_symlinks=False)",
        ])
        .arg(&alias)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let observe = || {
        let m = std::fs::symlink_metadata(&alias).unwrap();
        (
            m.dev(),
            m.ino(),
            m.mode(),
            m.atime(),
            m.atime_nsec(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    let before = observe();
    let output = command_at_home(
        root.path(),
        &["--resume", "--dry-run"],
        &root.path().join("data"),
    )
    .env("ORCH_DATA_DIR", &alias)
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(observe(), before);
}

#[test]
fn existing_destination_with_an_aliased_parent_resumes_on_the_native_writer() {
    let root = fixture();
    std::fs::create_dir_all(root.path().join("real/home")).unwrap();
    std::os::unix::fs::symlink(root.path().join("real"), root.path().join("alias")).unwrap();
    let destination = root.path().join("alias/home/destination");
    for args in [&[][..], &["--resume"][..]] {
        let output = command_at_home(root.path(), args, &root.path().join("data-home"))
            .env("ORCH_DATA_DIR", &destination)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["imported"], FIXTURE_ROWS, "{value}");
        assert_eq!(value["unfinished"], true);
    }
    let output = python(root.path()).args(["-c", "import sqlite3,sys,json; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); print(json.dumps([c.execute('SELECT count(*),min(revision),max(revision) FROM templates').fetchone(),c.execute('SELECT count(*) FROM import_manifest').fetchone()])); c.close()"])
        .arg(root.path().join("real/home/destination/orch.db")).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([[1, 1, 1], [FIXTURE_ROWS]])
    );
}

#[test]
fn missing_explicit_parent_is_a_prewrite_refusal_without_initializing_home() {
    let root = fixture();
    let destination = root.path().join("missing-parent/destination");
    let output = command_at_home(root.path(), &[], &root.path().join("unused-home"))
        .env("ORCH_DATA_DIR", &destination)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["unfinished"], false);
    assert_eq!(value["imported"], 0);
    assert!(!destination.exists());
    assert!(!root.path().join("unused-home").exists());
}
