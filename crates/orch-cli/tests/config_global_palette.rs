//! `orch config global set|get palette` accepts every built-in palette.
//!
//! Driven through the real binary, each test with its own temp project and
//! its own `global.yml` (never the shared `target/test-global` file, which
//! parallel tests would race on).
//!
//! `config global set|get|show` are daemon RPCs since S2a-cli (the global
//! config store is read and written from inside the daemon process), so
//! `project()` starts an explicit test daemon on an isolated socket right
//! after `orch init` (see `tests/common/daemon.rs`) and `Project::drop` stops
//! it, process-table checked.

use std::path::Path;
use std::process::{Command, Output};

use assert_cmd::prelude::*;
use orch_domain::TUI_PALETTE_NAMES;

#[path = "common/daemon.rs"]
mod daemon;
use daemon::TestDaemon;

struct Project {
    // Dropped first: the daemon stops before its project directory goes.
    daemon: Option<TestDaemon>,
    root: tempfile::TempDir,
}

impl Project {
    fn path(&self) -> &Path {
        self.root.path()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        daemon::unregister_project(&self.path().join("project"));
    }
}

fn orch(root: &Path) -> Command {
    let mut command = Command::cargo_bin("orch").expect("the orch binary");
    command
        .current_dir(root.join("project"))
        .env("HOME", root.join("home"))
        .env("ORCHESTRY_GLOBAL_CONFIG_PATH", root.join("home/global.yml"))
        .env("NO_COLOR", "1")
        .env("PATH", "/usr/bin:/bin")
        .env_remove("RUST_LOG")
        .env_remove("ORCHESTRY_DEBUG");
    daemon::apply_for(&root.join("project"), &mut command);
    command
}

fn project() -> Project {
    let root = tempfile::Builder::new()
        .prefix("orch-palette-")
        .tempdir()
        .expect("tempdir");
    std::fs::create_dir_all(root.path().join("project")).expect("project dir");
    std::fs::create_dir_all(root.path().join("home")).expect("home dir");
    orch(root.path())
        .args(["init", "--name", "p", "--adapter", "shell"])
        .assert()
        .success();
    let project_root = root.path().join("project");
    let daemon = TestDaemon::start(&project_root, &root.path().join("home"));
    daemon::register_project(&project_root, &daemon);
    Project {
        daemon: Some(daemon),
        root,
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn every_built_in_palette_can_be_set_and_read_back() {
    let root = project();
    for palette in TUI_PALETTE_NAMES {
        let name = palette.as_str();
        // `set` lowercases its input, so the upper-case spelling is accepted
        // and stored lower-case.
        let output = orch(root.path())
            .args(["config", "global", "set", "palette", &name.to_uppercase()])
            .output()
            .expect("run set");
        assert!(output.status.success(), "{name}: {}", text(&output));
        assert!(
            text(&output).contains(&format!("palette = {name}")),
            "{name}: {}",
            text(&output)
        );

        let output = orch(root.path())
            .args(["config", "global", "get", "palette", "--json"])
            .output()
            .expect("run get");
        assert!(output.status.success(), "{name}: {}", text(&output));
        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("get --json prints JSON");
        assert_eq!(json["value"], name, "{name}");

        let file =
            std::fs::read_to_string(root.path().join("home/global.yml")).expect("global.yml");
        assert!(file.contains(&format!("palette: {name}")), "{name}: {file}");
    }
}

#[test]
fn an_unknown_palette_is_refused_with_the_full_list_of_names() {
    let root = project();
    let output = orch(root.path())
        .args(["config", "global", "set", "palette", "rose_pine"])
        .output()
        .expect("run set");
    // `config global set` exits 0 even when it rejects the value.
    assert!(output.status.success(), "{}", text(&output));
    let names: Vec<&str> = TUI_PALETTE_NAMES.iter().map(|p| p.as_str()).collect();
    let expected = format!("Invalid value \"rose_pine\". Valid: {}", names.join(", "));
    assert!(text(&output).contains(&expected), "{}", text(&output));
    assert!(
        text(&output).contains("catppuccin_mocha") && text(&output).contains("tokyo_night"),
        "{}",
        text(&output)
    );
}
