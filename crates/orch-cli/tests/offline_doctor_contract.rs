#![cfg(unix)]
//! Offline doctor is discovery-only, even when invoked through the real binary.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};

const PROBES: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "gemini",
    "cursor-agent",
    "agent",
    "pi",
    "grok",
    "agy",
    "bash",
    "git",
];

struct Sandbox {
    root: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    shims: PathBuf,
    calls: PathBuf,
}

impl Sandbox {
    fn new(initialized: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("orch-offline-")
            .tempdir_in("/tmp")
            .unwrap();
        let path = root.path().canonicalize().unwrap();
        assert!(
            !path.ancestors().any(|p| p.join(".orchestry").exists()),
            "fixture must have no project ancestor"
        );
        let sandbox = Self {
            project: path.join("project"),
            home: path.join("home"),
            tmp: path.join("tmp"),
            shims: path.join("shims"),
            calls: path.join("calls.log"),
            root,
        };
        for dir in [
            &sandbox.project,
            &sandbox.home,
            &sandbox.tmp,
            &sandbox.shims,
        ] {
            fs::create_dir(dir).unwrap();
        }
        assert!(sandbox.tmp.as_os_str().len() <= 64);
        fs::write(&sandbox.calls, "").unwrap();
        for probe in PROBES.iter().filter(|&&probe| probe != "git") {
            sandbox.install_shim(probe);
        }
        if initialized {
            // Only setup can resolve the real git; all adapter probes are blocked.
            let output = sandbox
                .orch()
                .args(["init", "--adapter", "shell", "--name", "offline"])
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(sandbox.project.join(".orchestry/config.yml").is_file());
        }
        sandbox.install_shim("git");
        assert_eq!(fs::read_to_string(&sandbox.calls).unwrap(), "");
        sandbox
    }

    fn install_shim(&self, name: &str) {
        let path = self.shims.join(name);
        fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"${0##*/}\" >> \"$PROBE_CALLS\"\nexit 127\n",
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("TMPDIR", &self.tmp)
            .env("ORCHESTRY_GLOBAL_CONFIG_PATH", self.home.join("global.yml"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.shims.display()))
            .env("PROBE_CALLS", &self.calls)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("NO_COLOR", "1")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::null());
        command
    }

    fn orch(&self) -> Command {
        self.command(assert_cmd::cargo::cargo_bin!("orch"))
    }

    fn doctor(&self, json: bool) -> Output {
        self.assert_no_runtime();
        let before = snapshot(self.root.path());
        let mut command = self.orch();
        if json {
            command.arg("--json");
        }
        let output = command.arg("doctor").output().unwrap();
        assert_eq!(
            fs::read_to_string(&self.calls).unwrap(),
            "",
            "offline doctor started an adapter or git child: {output:?}"
        );
        assert_eq!(
            snapshot(self.root.path()),
            before,
            "doctor changed the private directory tree"
        );
        self.assert_no_runtime();
        output
    }

    fn assert_no_runtime(&self) {
        for path in ["daemon.json", "run", "orch.sock", "daemon.sock"] {
            assert!(
                !self.project.join(".orchestry").join(path).exists(),
                "doctor must not create {path}"
            );
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Contents {
    Directory,
    File([u8; 32]),
    Symlink(PathBuf),
    Other,
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, Contents)> {
    fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, (u32, Contents)>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let contents = if metadata.is_dir() {
            Contents::Directory
        } else if metadata.is_file() {
            Contents::File(Sha256::digest(fs::read(path).unwrap()).into())
        } else if metadata.file_type().is_symlink() {
            Contents::Symlink(fs::read_link(path).unwrap())
        } else {
            Contents::Other
        };
        entries.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            (metadata.permissions().mode(), contents),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

#[test]
fn doctor_without_a_project_exits_three_without_children_or_tree_changes() {
    let sandbox = Sandbox::new(false);
    let output = sandbox.doctor(false);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Not initialized"), "{stderr}");
    assert!(stderr.contains("orch init"), "{stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("discovery only"), "{stdout}");
    assert!(
        stdout.contains("project discovery: unavailable"),
        "{stdout}"
    );
    assert!(!stdout.contains("adapters ready"), "{stdout}");
}

#[test]
fn doctor_without_a_daemon_exits_seven_without_children_or_tree_changes() {
    let sandbox = Sandbox::new(true);
    let output = sandbox.doctor(false);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("No orch daemon is running for"), "{stderr}");
    assert!(
        stderr.contains(sandbox.project.to_str().unwrap()),
        "{stderr}"
    );
    assert!(stderr.contains("orch daemon start"), "{stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("discovery only"), "{stdout}");
    assert!(!stdout.contains("adapters ready"), "{stdout}");
}

#[test]
fn json_doctor_without_a_project_exits_three_without_children_or_tree_changes() {
    let output = Sandbox::new(false).doctor(true);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("adapters ready")
    );
}

#[test]
fn json_doctor_without_a_daemon_exits_seven_without_children_or_tree_changes() {
    let output = Sandbox::new(true).doctor(true);
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("adapters ready")
    );
}

#[test]
fn logging_shims_block_and_record_every_adapter_and_git_probe() {
    let sandbox = Sandbox::new(false);
    for probe in PROBES {
        let output = sandbox.command(probe).arg("--version").output().unwrap();
        assert_eq!(output.status.code(), Some(127), "{probe}: {output:?}");
    }
    assert_eq!(
        fs::read_to_string(&sandbox.calls).unwrap(),
        PROBES
            .iter()
            .map(|name| format!("{name}\n"))
            .collect::<String>()
    );
}

#[test]
fn init_in_an_empty_project_reaches_the_git_logging_shim() {
    let sandbox = Sandbox::new(false);
    let output = sandbox
        .orch()
        .args(["init", "--adapter", "shell", "--name", "control"])
        .output()
        .unwrap();
    let calls = fs::read_to_string(&sandbox.calls).unwrap();
    assert!(
        calls.lines().any(|name| name == "git"),
        "orch init did not reach the git logging shim: calls={calls:?}, output={output:?}"
    );
    sandbox.assert_no_runtime();
}
