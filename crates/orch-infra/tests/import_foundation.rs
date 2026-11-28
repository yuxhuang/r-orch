//! Synthetic witnesses for the foundation shared by offline import and startup.
use orch_infra::data_dir::DataDirError;
use orch_infra::data_dir::confined::{ConfinedDir, ConfinedError, FileAccess};
use orch_infra::data_dir::paths::{read_project_record, write_project_record};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Canary(Child);

impl Canary {
    fn finish(&mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "foundation child timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut text = Vec::new();
        std::io::Read::read_to_end(&mut self.0.stdout.take().unwrap(), &mut text).unwrap();
        Output {
            status: self.0.wait().unwrap(),
            stdout: text,
            stderr: Vec::new(),
        }
    }
}

impl Drop for Canary {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("HOME", root)
        .env("TMPDIR", root)
        .env("XDG_RUNTIME_DIR", root)
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", root.join("global.yml"))
        .env("ORCH_DATA_HOME", root.join("daemons"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    command
}

#[cfg(target_os = "linux")]
fn controlling_terminal() -> i64 {
    let stat = fs::read_to_string("/proc/self/stat").unwrap();
    // comm is parenthesized and may contain spaces; tty_nr is field 7.
    stat.rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(4)
        .unwrap()
        .parse()
        .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "re-executed by descriptor_opens_never_acquire_a_controlling_terminal"]
fn terminal_open_child() {
    let terminal = std::env::var_os("FOUNDATION_TERMINAL").expect("terminal child not configured");
    let terminal = std::path::Path::new(&terminal);
    assert_eq!(
        nix::unistd::getsid(None).unwrap().as_raw(),
        std::process::id() as i32
    );
    assert_eq!(controlling_terminal(), 0);
    let result = match std::env::var("FOUNDATION_OPEN").unwrap().as_str() {
        "root" => ConfinedDir::open(terminal).map(|_| ()),
        "directory" => ConfinedDir::open(terminal.parent().unwrap())
            .unwrap()
            .open_dir(terminal.file_name().unwrap())
            .map(|_| ()),
        "file" => ConfinedDir::open(terminal.parent().unwrap())
            .unwrap()
            .open_file(terminal.file_name().unwrap(), FileAccess::Untrusted)
            .map(|_| ()),
        other => panic!("unknown open case: {other}"),
    };
    assert!(
        matches!(result, Err(ConfinedError::WrongType(_))),
        "{result:?}"
    );
    assert_eq!(
        controlling_terminal(),
        0,
        "refused device became controlling terminal"
    );
    println!("foundation-terminal-checked");
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_opens_never_acquire_a_controlling_terminal() {
    let root = tempfile::tempdir().unwrap();
    for case in ["root", "directory", "file"] {
        // Python supplies a synthetic pty and a new session without unsafe code
        // or an extra dependency. Keep its master open across exec, so the slave
        // is live when the real confined operation opens and refuses it.
        let child = command(root.path());
        let mut launcher = Command::new("python3");
        launcher.args(["-c", "import os, pty, sys\nm,s=pty.openpty()\nos.environ['FOUNDATION_TERMINAL']=os.ttyname(s)\nos.close(s)\nos.set_inheritable(m,True)\nos.setsid()\nos.execv(sys.argv[1],sys.argv[1:])"])
            .arg(child.get_program())
            .args(["--exact", "terminal_open_child", "--ignored", "--nocapture", "--test-threads=1"])
            .env_clear()
            .envs(child.get_envs().filter_map(|(k,v)| v.map(|v| (k,v))))
            .env("FOUNDATION_OPEN", case)
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let output = Canary(launcher.spawn().unwrap()).finish();
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{case}: {text}");
        assert!(
            text.contains("foundation-terminal-checked"),
            "{case}: {text}"
        );
        assert!(text.contains("1 passed; 0 failed"), "{case}: {text}");
    }
}

#[test]
#[ignore = "re-executed with a deadline by importer_special_file_refusals_are_bounded_and_typed"]
fn special_file_child() {
    let root = std::env::var_os("FOUNDATION_ROOT").expect("special-file child not configured");
    let root = std::path::Path::new(&root);
    let tasks = root.join(".orchestry/tasks");
    fs::create_dir_all(&tasks).unwrap();
    nix::unistd::mkfifo(&tasks.join("tsk_fifo.yml"), nix::sys::stat::Mode::S_IRUSR).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(tasks.join("tsk_socket.yml")).unwrap();
    fs::write(root.join("source-canary"), b"untouched").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let report = runtime
        .block_on(orch_infra::import::preflight::preflight(
            root.to_owned(),
            Default::default(),
            std::sync::Arc::new(orch_infra::import::preflight::SystemProbe),
        ))
        .unwrap();
    let refusals: Vec<_> = report
        .refusals
        .listed
        .iter()
        .map(|finding| (finding.path.as_str(), finding.rule))
        .collect();
    assert_eq!(
        refusals,
        [
            ("tasks/tsk_fifo.yml", "file_kind"),
            ("tasks/tsk_socket.yml", "file_kind")
        ]
    );
    assert_eq!(report.refusals.total(), 2);
    assert_eq!(fs::read(root.join("source-canary")).unwrap(), b"untouched");
    assert!(!root.join(".orch-project").exists());
    println!("foundation-special-files-checked");
}

#[test]
fn importer_special_file_refusals_are_bounded_and_typed() {
    let root = tempfile::tempdir().unwrap();
    let output = Canary(
        command(root.path())
            .args(["--exact", "special_file_child", "--ignored", "--nocapture"])
            .env("FOUNDATION_ROOT", root.path())
            .spawn()
            .unwrap(),
    )
    .finish();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{text}");
    assert!(text.contains("foundation-special-files-checked"), "{text}");
    assert!(text.contains("1 passed; 0 failed"), "{text}");
}

#[test]
#[ignore = "killed after publishing an incomplete record by interrupted_project_records_refuse_without_retargeting"]
fn project_record_child() {
    let root = std::env::var_os("FOUNDATION_ROOT").expect("record child not configured");
    let root = std::path::Path::new(&root);
    let home = ConfinedDir::create(&root.join("home")).unwrap();
    let projects = home.create_dir("projects").unwrap();
    let mut record = projects
        .create_file("9fa0423cd44dfbc56bc5b71dea05c66fb0f1f2f0d884eed107e15cfdbce760c7")
        .unwrap();
    let prefix = std::env::var("FOUNDATION_PREFIX").unwrap();
    record.write_all(prefix.as_bytes()).unwrap();
    record.sync_all().unwrap();
    projects.sync().unwrap();
    fs::write(root.join("ready"), b"foundation-record-written").unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn interrupted_project_records_refuse_without_retargeting() {
    for partial in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("config.yml"), b"project: {name: synthetic}\n").unwrap();
        let source_before = fs::read(source.join("config.yml")).unwrap();
        let data = root.path().join("data");
        let other = root.path().join("other");
        for path in [&data, &other] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(path.join("canary"), b"untouched").unwrap();
        }
        // A crash after exclusive creation or after the path but before its
        // mandatory newline. The child leaves the same inode and partial bytes.
        let prefix = if partial { data.to_str().unwrap() } else { "" };
        let mut child = Canary(
            command(root.path())
                .args([
                    "--exact",
                    "project_record_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("FOUNDATION_ROOT", root.path())
                .env("FOUNDATION_PREFIX", prefix)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.path().join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "record child exited before boundary"
            );
            assert!(
                Instant::now() < deadline,
                "record child did not reach boundary"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::read(root.path().join("ready")).unwrap(),
            b"foundation-record-written"
        );
        child.0.kill().unwrap();
        assert_eq!(child.0.wait().unwrap().signal(), Some(9));
        let home = root.path().join("home");
        let record =
            home.join("projects/9fa0423cd44dfbc56bc5b71dea05c66fb0f1f2f0d884eed107e15cfdbce760c7");
        let before = fs::metadata(&record).unwrap();
        for result in [
            read_project_record(&home, "prj_test1").map(|_| ()),
            write_project_record(&home, "prj_test1", &data),
            write_project_record(&home, "prj_test1", &other),
        ] {
            let error = result.unwrap_err();
            assert!(
                matches!(error, DataDirError::InvalidLocation(_)),
                "{error:?}"
            );
            assert!(
                error
                    .to_string()
                    .contains("project record must hold one absolute path and a newline"),
                "{error}"
            );
        }
        assert_eq!(fs::read(&record).unwrap(), prefix.as_bytes());
        let after = fs::metadata(&record).unwrap();
        assert_eq!(
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.mtime(),
                after.mtime_nsec()
            ),
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.mtime(),
                before.mtime_nsec()
            )
        );
        assert_eq!(fs::read(source.join("config.yml")).unwrap(), source_before);
        assert_eq!(fs::read(data.join("canary")).unwrap(), b"untouched");
        assert_eq!(fs::read(other.join("canary")).unwrap(), b"untouched");
        assert!(!source.join(".orch-project").exists());
        assert_eq!(fs::read_dir(home.join("projects")).unwrap().count(), 1);
    }
}
