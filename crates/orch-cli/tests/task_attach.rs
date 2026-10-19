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
fn a_directory_source_fails_with_exit_2_without_saving_or_copying_anything() {
    let project = Project::new();
    let present = project.source("present.txt", b"present");
    let directory = project.dir.path().join("files/directory");
    std::fs::create_dir(&directory).unwrap();

    let output = project
        .orch()
        .args([
            "task",
            "add",
            "not a regular file",
            "--attach",
            &format!("{},{}", arg(&present), arg(&directory)),
        ])
        .output()
        .expect("run orch");

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(arg(&directory)), "stderr: {stderr}");
    assert_eq!(project.task_count(), 0);
    let attachments = project.root().join(".orchestry/attachments");
    assert!(
        !attachments.exists() || std::fs::read_dir(&attachments).unwrap().next().is_none(),
        "validation must not leave attachment files or task directories"
    );
}

#[test]
fn duplicate_basenames_fail_with_exit_2_without_saving_or_overwriting_files() {
    let project = Project::new();
    let first = project.source("same.txt", b"first");
    let other = project.dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let second = other.join("same.txt");
    std::fs::write(&second, b"second").unwrap();

    let output = project
        .orch()
        .args([
            "task",
            "add",
            "colliding names",
            "--attach",
            &format!("{},{}", arg(&first), arg(&second)),
        ])
        .output()
        .expect("run orch");

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("same.txt"), "stderr: {stderr}");
    assert_eq!(project.task_count(), 0);
    let attachments = project.root().join(".orchestry/attachments");
    assert!(
        !attachments.exists() || std::fs::read_dir(&attachments).unwrap().next().is_none(),
        "a duplicate must be rejected before any files are stored"
    );
    assert_eq!(std::fs::read(&first).unwrap(), b"first");
    assert_eq!(std::fs::read(&second).unwrap(), b"second");
}

#[test]
fn case_variant_basenames_fail_with_exit_2_without_saving_or_copying_anything() {
    let project = Project::new();
    let first = project.source("Report.txt", b"first");
    let other = project.dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let second = other.join("report.txt");
    std::fs::write(&second, b"second").unwrap();

    let output = project
        .orch()
        .args([
            "task",
            "add",
            "case variant names",
            "--attach",
            &format!("{},{}", arg(&first), arg(&second)),
        ])
        .output()
        .expect("run orch");

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "Duplicate attachment name: report.txt ({}, {})",
            first.display(),
            second.display()
        )),
        "stderr: {stderr}"
    );
    assert_eq!(project.task_count(), 0);
    assert!(
        !project.root().join(".orchestry/attachments").exists(),
        "case variants must be rejected before creating the attachment directory"
    );
    assert_eq!(std::fs::read(&first).unwrap(), b"first");
    assert_eq!(std::fs::read(&second).unwrap(), b"second");
}

#[test]
#[cfg(unix)]
fn a_fifo_source_is_rejected_with_exit_2_without_waiting_for_a_writer() {
    let project = Project::new();
    let fifo = project.dir.path().join("files/source.fifo");
    success(
        Command::new("/usr/bin/mkfifo")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg(&fifo),
    );

    let mut command = assert_cmd::Command::from_std(project.orch());
    // Kill and reap the child if regular-file validation regresses: opening
    // this FIFO for copying would otherwise wait forever for a writer.
    let output = command
        .timeout(std::time::Duration::from_secs(5))
        .args(["task", "add", "fifo", "--attach", arg(&fifo)])
        .output()
        .expect("run bounded orch");

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(arg(&fifo)), "stderr: {stderr}");
    assert_eq!(project.task_count(), 0);
    let attachments = project.root().join(".orchestry/attachments");
    assert!(
        !attachments.exists() || std::fs::read_dir(&attachments).unwrap().next().is_none(),
        "a FIFO must be rejected before creating an attachment directory"
    );
}

// The CLI deliberately exposes attachments as read-only on task update.
// Exercise append through the engine, with real CLI creation and observation.
#[tokio::test]
async fn an_append_collision_returns_exit_2_and_preserves_cli_visible_task_and_bytes() {
    let project = Project::new();
    let original = project.source("same.txt", b"original");
    let created = project.json(&["task", "add", "original", "--attach", arg(&original)]);
    let id = created["id"].as_str().unwrap();
    let before = std::fs::read(project.root().join(format!(".orchestry/tasks/{id}.yml"))).unwrap();
    std::fs::write(&original, b"replacement").unwrap();
    let engine = orch_engine::build_light(project.root()).await.unwrap();
    let err = engine
        .task_service
        .update(
            id,
            orch_engine::task_service::UpdateTaskFields {
                title: Some("must not be saved".into()),
                attachments: Some(vec![original]),
                ..Default::default()
            },
        )
        .await
        .expect_err("append collision must be rejected");
    assert_eq!(err.exit_code(), 2);
    assert_eq!(err.to_string(), "Attachment already exists: same.txt");
    assert_eq!(
        std::fs::read(project.stored_attachment(id, "same.txt")).unwrap(),
        b"original"
    );
    assert_eq!(
        std::fs::read(project.root().join(format!(".orchestry/tasks/{id}.yml"))).unwrap(),
        before
    );
    assert_eq!(project.json(&["task", "show", id]), created);
}

#[test]
fn without_attach_the_task_has_no_attachments_field() {
    let project = Project::new();
    let created = project.json(&["task", "add", "plain"]);
    assert!(created.get("attachments").is_none(), "{created:#}");
}
