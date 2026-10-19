//! `orch task add --attach`: the advertised flag reaches the task service, so
//! the files are copied under `.orchestry/attachments/<task id>/` and the task
//! records their basenames.
//!
//! Every case drives the real binary in a throwaway project with a throwaway
//! `HOME`, `PATH=/usr/bin:/bin` and only the `shell` adapter configured; no
//! scheduler runs, so nothing is dispatched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::prelude::*;
use serde_json::Value;

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("orch-task-attach-")
            .tempdir()
            .expect("tempdir");
        assert!(
            !dir.path()
                .ancestors()
                .any(|d| d.join(".orchestry").is_dir()),
            "the tmpdir has an .orchestry ancestor; point TMPDIR outside any ORCH project"
        );
        for sub in ["home", "p", "files"] {
            std::fs::create_dir_all(dir.path().join(sub)).expect("scratch dir");
        }
        let project = Project { dir };
        success(
            project
                .orch()
                .args(["init", "--name", "attach", "--adapter", "shell"]),
        );
        project
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    /// A source file outside the project, so a copy is distinguishable from
    /// the original.
    fn source(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.path().join("files").join(name);
        std::fs::write(&path, bytes).expect("write source file");
        path
    }

    fn orch(&self) -> Command {
        let mut command = Command::cargo_bin("orch").expect("the orch binary");
        command
            .current_dir(self.root())
            .env("HOME", self.dir.path().join("home"))
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.dir.path().join("home/global.yml"),
            )
            .env("NO_COLOR", "1")
            .env("PATH", "/usr/bin:/bin")
            .env_remove("RUST_LOG")
            .env_remove("ORCHESTRY_DEBUG");
        command
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = success(self.orch().args(args).arg("--json"));
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is not JSON ({e}):\n{}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn stored_attachment(&self, task_id: &str, name: &str) -> PathBuf {
        self.root()
            .join(".orchestry/attachments")
            .join(task_id)
            .join(name)
    }

    fn task_yaml(&self, task_id: &str) -> Value {
        let path = self.root().join(format!(".orchestry/tasks/{task_id}.yml"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_yaml_ng::from_str(&text).expect("task file is YAML")
    }

    fn task_count(&self) -> usize {
        self.json(&["task", "list"])
            .as_array()
            .expect("task list is an array")
            .len()
    }
}

fn success(command: &mut Command) -> Output {
    let output = command.output().expect("run orch");
    assert!(
        output.status.success(),
        "{command:?} exited {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn arg(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 temp path")
}

fn names(value: &Value) -> Vec<&str> {
    value["attachments"]
        .as_array()
        .unwrap_or_else(|| panic!("no attachments array in {value:#}"))
        .iter()
        .map(|name| name.as_str().expect("attachment name is a string"))
        .collect()
}

#[test]
fn an_absolute_existing_file_is_copied_and_referenced_by_the_task() {
    let project = Project::new();
    let bytes = b"owned attachment fixture\n\x00\xff binary tail";
    let source = project.source("attachment.txt", bytes);

    let created = project.json(&["task", "add", "with file", "--attach", arg(&source)]);
    let id = created["id"].as_str().expect("an id").to_owned();

    assert_eq!(names(&created), ["attachment.txt"], "add --json");
    assert_eq!(
        names(&project.json(&["task", "show", &id])),
        ["attachment.txt"],
        "show --json"
    );
    assert_eq!(
        names(&project.task_yaml(&id)),
        ["attachment.txt"],
        "task file"
    );
    assert_eq!(
        std::fs::read(project.stored_attachment(&id, "attachment.txt")).expect("stored copy"),
        bytes
    );
    // A copy, not a move.
    assert_eq!(std::fs::read(&source).expect("source kept"), bytes);
}

#[test]
fn several_comma_separated_paths_are_all_attached_in_order() {
    let project = Project::new();
    let first = project.source("spec.md", b"# spec\n");
    // Relative paths resolve against the directory the command runs in.
    std::fs::write(project.root().join("shot.png"), b"\x89PNG fake").expect("write");

    let created = project.json(&[
        "task",
        "add",
        "two files",
        "--attach",
        &format!("{} , shot.png", arg(&first)),
    ]);
    let id = created["id"].as_str().expect("an id");

    assert_eq!(names(&created), ["spec.md", "shot.png"]);
    assert_eq!(
        std::fs::read(project.stored_attachment(id, "spec.md")).expect("spec copy"),
        b"# spec\n"
    );
    assert_eq!(
        std::fs::read(project.stored_attachment(id, "shot.png")).expect("png copy"),
        b"\x89PNG fake"
    );
}

#[test]
fn a_missing_file_fails_with_exit_2_and_creates_no_task() {
    let project = Project::new();
    let present = project.source("present.txt", b"here");
    let missing = project.dir.path().join("files/missing.txt");

    let output = project
        .orch()
        .args([
            "task",
            "add",
            "half missing",
            "--attach",
            &format!("{},{}", arg(&present), arg(&missing)),
        ])
        .output()
        .expect("run orch");

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("Attachment file not found: {}", missing.display())),
        "stderr: {stderr}"
    );
    assert_eq!(project.task_count(), 0, "no task is saved");
    let attachments = project.root().join(".orchestry/attachments");
    let leftovers: Vec<_> = std::fs::read_dir(&attachments)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "nothing stored: {leftovers:?}");
}

#[test]
fn without_attach_the_task_has_no_attachments_field() {
    let project = Project::new();
    let created = project.json(&["task", "add", "plain"]);
    assert!(created.get("attachments").is_none(), "{created:#}");
}
