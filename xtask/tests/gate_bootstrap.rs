//! Copied bootstrap + stand-in xtask. The legacy full gate is never executed.

#[path = "gate_witness.rs"]
mod witness;

use std::fs;
use std::os::unix::fs::symlink;
use witness::fixture::{
    CARGO_SUBCOMMAND_PY, COMMAND_BUDGET, Capture, HANDSHAKE_BUDGET, Sandbox, backdate, wait_for,
};

fn source(marker: &str) -> String {
    format!(
        r#"fn main() {{
    println!("source_marker={{}}", {marker:?});
    println!("judge_source_id={{}}", std::env::var("XTASK_GATE_JUDGE_ID").unwrap_or_else(|_| "absent".into()));
}}
"#
    )
}

fn bootstrap() -> Sandbox {
    let sandbox = Sandbox::new();
    let script = include_str!("../gate.sh");
    // This is an execution-safety prerequisite, not a passing gate oracle.
    // F0 deliberately retains a full-workspace legacy script. Never run it as
    // a stand-in for the unavailable D27 bootstrap or collect misleading red.
    assert!(
        script.contains("gate-xtask") && script.contains("XTASK_GATE_JUDGE_ID"),
        "F0 unavailable: legacy gate.sh was NOT executed; implemented bootstrap required"
    );
    sandbox.tools();
    sandbox.write(".gitignore", "/target/\n/xtask/tracked-ignored.txt\n");
    sandbox.write(
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = [\"xtask\"]\n",
    );
    sandbox.write(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"xtask\"\nversion = \"0.0.0\"\n",
    );
    sandbox.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.99.1\"\n");
    sandbox.write(".cargo/config.toml", "[env]\nORCHESTRY_GLOBAL_CONFIG_PATH = { value = \"target/test-global/global.yml\", relative = true }\n");
    sandbox.write(
        "xtask/Cargo.toml",
        "[package]\nname = \"xtask\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    sandbox.write("xtask/src/main.rs", source("FIRST"));
    sandbox.write("xtask/gate.sh", script);
    sandbox.commit();
    sandbox
}

fn run(sandbox: &Sandbox) -> Capture {
    sandbox.run(
        sandbox
            .command("/bin/bash")
            .arg("xtask/gate.sh")
            .arg("--plan"),
    )
}

fn stamp(sandbox: &Sandbox) -> std::path::PathBuf {
    sandbox.root.join("target/gate-xtask/judge-id")
}

fn returned_id(output: &Capture) -> &str {
    output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("judge_source_id="))
        .expect("stand-in did not receive judge identity")
}

fn require_first(sandbox: &Sandbox) -> String {
    let output = run(sandbox);
    output.code(0);
    output.contains("source_marker=FIRST");
    let id = returned_id(&output);
    assert_eq!(id.len(), 40);
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(fs::read_to_string(stamp(sandbox)).unwrap().trim(), id);
    assert_eq!(id, working_judge_id(sandbox));
    id.to_owned()
}

fn working_judge_id(sandbox: &Sandbox) -> String {
    let output = sandbox.run(sandbox.command("/usr/bin/git").args([
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ]));
    output.code(0);
    let mut paths: Vec<_> = output
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| {
            path.starts_with("xtask/")
                || [
                    "Cargo.toml",
                    "Cargo.lock",
                    "rust-toolchain.toml",
                    ".cargo/config.toml",
                ]
                .contains(path)
        })
        .collect();
    paths.sort();
    paths.dedup();
    let records = paths
        .iter()
        .map(|path| {
            let mut encoded = String::new();
            if path
                .bytes()
                .all(|b| (0x21..=0x7e).contains(&b) && !b"\"\\".contains(&b))
            {
                encoded.push_str(path);
            } else {
                encoded.push('"');
                for byte in path.bytes() {
                    match byte {
                        b'"' => encoded.push_str("\\\""),
                        b'\\' => encoded.push_str("\\\\"),
                        b'\t' => encoded.push_str("\\t"),
                        b'\n' => encoded.push_str("\\n"),
                        b'\r' => encoded.push_str("\\r"),
                        0x20..=0x7e => encoded.push(char::from(byte)),
                        _ => encoded.push_str(&format!("\\{byte:03o}")),
                    }
                }
                encoded.push('"');
            }
            format!(
                "{} {encoded}\n",
                sandbox.blob(&fs::read(sandbox.root.join(path)).unwrap())
            )
        })
        .collect::<String>();
    sandbox.blob(records.as_bytes())
}

#[test]
fn first_bootstrap_builds_owned_target_and_supplies_completed_identity() {
    let sandbox = bootstrap();
    require_first(&sandbox);
    assert!(sandbox.root.join("target/gate-xtask/debug/xtask").is_file());
}

#[test]
fn backdated_stale_plain_build_is_rebuilt_by_content_bootstrap() {
    let sandbox = bootstrap();
    let old = require_first(&sandbox);
    sandbox.write("xtask/src/main.rs", source("SECOND"));
    backdate(&sandbox.root.join("xtask/src/main.rs"));
    let mut plain = sandbox.command("cargo");
    plain
        .args(["build", "-p", "xtask"])
        .env("CARGO_TARGET_DIR", sandbox.root.join("target/gate-xtask"));
    sandbox.run(&mut plain).code(0);
    let stale =
        sandbox.run(&mut sandbox.command(sandbox.root.join("target/gate-xtask/debug/xtask")));
    stale.code(0);
    stale.contains("source_marker=FIRST"); // Invalid plant fails here.
    fs::write(
        sandbox.root.join("target/gate-xtask/stale-canary"),
        "plant\n",
    )
    .unwrap();
    let output = run(&sandbox);
    output.code(0);
    output.contains("source_marker=SECOND");
    assert_ne!(returned_id(&output), old);
    assert!(!sandbox.root.join("target/gate-xtask/stale-canary").exists());
}

#[test]
fn absent_stamp_resets_judge_target_without_touching_inherited_target() {
    let sandbox = bootstrap();
    require_first(&sandbox);
    fs::remove_file(stamp(&sandbox)).unwrap();
    fs::write(
        sandbox.root.join("target/gate-xtask/stale-canary"),
        "plant\n",
    )
    .unwrap();
    let outside = sandbox.temporary.join("developer-target");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("canary"), "untouched\n").unwrap();
    let mut command = sandbox.command("/bin/bash");
    command
        .args(["xtask/gate.sh", "--plan"])
        .env("CARGO_TARGET_DIR", &outside);
    sandbox.run(&mut command).code(0);
    assert!(!sandbox.root.join("target/gate-xtask/stale-canary").exists());
    assert_eq!(
        fs::read_to_string(outside.join("canary")).unwrap(),
        "untouched\n"
    );
    assert_eq!(fs::read_dir(outside).unwrap().count(), 1);
}

#[test]
fn hostile_input_paths_tracked_ignored_and_untracked_changes_affect_judge_identity() {
    let sandbox = bootstrap();
    let mut previous = require_first(&sandbox);
    for path in [
        "xtask/space name",
        "xtask/tab\tname",
        "xtask/new\nline",
        "xtask/quote\"slash\\",
        "xtask/-leading",
        "xtask/$(touch SHOULD_NOT_EXIST)",
        "xtask/tracked-ignored.txt",
    ] {
        sandbox.write(path, "first contents\n");
        sandbox.git(&["add", "-f", "--", path]);
        let added = run(&sandbox);
        added.code(0);
        assert_eq!(returned_id(&added), working_judge_id(&sandbox));
        assert_ne!(returned_id(&added), previous);
        previous = returned_id(&added).to_owned();
        sandbox.write(path, "changed contents\n");
        backdate(&sandbox.root.join(path));
        let changed = run(&sandbox);
        changed.code(0);
        assert_eq!(returned_id(&changed), working_judge_id(&sandbox));
        assert_ne!(returned_id(&changed), previous);
        previous = returned_id(&changed).to_owned();
    }
    assert!(!sandbox.root.join("SHOULD_NOT_EXIST").exists());
    sandbox.write("xtask/untracked-input", "untracked\n");
    let untracked = run(&sandbox);
    untracked.code(0);
    assert_eq!(returned_id(&untracked), working_judge_id(&sandbox));
    assert_ne!(returned_id(&untracked), previous);
    fs::rename(
        sandbox.root.join("xtask/untracked-input"),
        sandbox.root.join("xtask/renamed-input"),
    )
    .unwrap();
    let renamed = run(&sandbox);
    renamed.code(0);
    assert_eq!(returned_id(&renamed), working_judge_id(&sandbox));
    assert_ne!(returned_id(&renamed), returned_id(&untracked));
    fs::remove_file(sandbox.root.join("xtask/renamed-input")).unwrap();
    let deleted = run(&sandbox);
    deleted.code(0);
    assert_eq!(returned_id(&deleted), working_judge_id(&sandbox));
    assert_ne!(returned_id(&deleted), returned_id(&renamed));
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_judge_input_is_lossless_or_explicitly_refused_without_stamp() {
    use std::os::unix::ffi::OsStringExt;
    let sandbox = bootstrap();
    let old = require_first(&sandbox);
    let path = std::ffi::OsString::from_vec(b"xtask/nonutf-\xff".to_vec());
    sandbox.write(&path, "nonutf\n");
    let output = run(&sandbox);
    if output.status.success() {
        assert_ne!(returned_id(&output), old);
    } else {
        assert!(
            !stamp(&sandbox).exists(),
            "unsupported input left a trusted stamp"
        );
    }
}

#[test]
fn linked_target_parent_child_and_stamp_refuse_without_touching_outside_or_siblings() {
    for component in ["target", "gate-xtask", "judge-id"] {
        let sandbox = bootstrap();
        require_first(&sandbox);
        let outside = sandbox.temporary.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("canary"), "untouched\n").unwrap();
        fs::create_dir_all(sandbox.root.join("sibling")).unwrap();
        fs::write(sandbox.root.join("sibling/canary"), "sibling\n").unwrap();
        let path = match component {
            "target" => sandbox.root.join("target"),
            "gate-xtask" => sandbox.root.join("target/gate-xtask"),
            _ => stamp(&sandbox),
        };
        if path.is_dir() {
            fs::remove_dir_all(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let destination = if component == "judge-id" {
            outside.join("canary")
        } else {
            outside.clone()
        };
        symlink(destination, &path).unwrap();
        let output = run(&sandbox);
        output.code(1);
        output.contains("REFUSED");
        assert_eq!(
            fs::read_to_string(outside.join("canary")).unwrap(),
            "untouched\n"
        );
        assert_eq!(
            fs::read_dir(&outside).unwrap().count(),
            1,
            "refusal wrote outside entries"
        );
        assert_eq!(
            fs::read_to_string(sandbox.root.join("sibling/canary")).unwrap(),
            "sibling\n"
        );
    }
}

#[test]
fn special_judge_target_is_refused_without_blocking_on_open() {
    let sandbox = bootstrap();
    require_first(&sandbox);
    let target = sandbox.root.join("target/gate-xtask");
    fs::remove_dir_all(&target).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(target).unwrap();
    let output = run(&sandbox);
    output.code(1);
    output.contains("REFUSED");
}

#[test]
fn failed_build_removes_trusted_stamp_and_next_launch_rebuilds() {
    let sandbox = bootstrap();
    require_first(&sandbox);
    sandbox.write("xtask/src/main.rs", "this is not Rust\n");
    assert!(!run(&sandbox).status.success());
    assert!(!stamp(&sandbox).exists());
    sandbox.write("xtask/src/main.rs", source("RECOVERED"));
    let recovered = run(&sandbox);
    recovered.code(0);
    recovered.contains("source_marker=RECOVERED");
}

fn park_cargo(sandbox: &Sandbox) {
    let cargo = sandbox.temporary.join("bin/cargo");
    let real = sandbox.cargo_forwarder();
    let ready = sandbox.temporary.join("build-ready");
    let release = sandbox.temporary.join("build-release");
    // A self-bounded process boundary, with file handshakes rather than sleeps.
    // No grandchild starts until released; process-group cleanup covers this worker.
    sandbox.script(
        &cargo,
        &format!(
            r#"#!/usr/bin/python3
import os, sys, time
from pathlib import Path
{parser}
if cargo_subcommand(sys.argv[1:]) != "build":
    os.execv({real:?}, [{real:?}] + sys.argv[1:])
ready = Path({ready:?})
release = Path({release:?})
ready.write_text(str(os.getpid()))
deadline = time.monotonic() + 30
while not release.exists():
    if time.monotonic() > deadline:
        Path(str(ready)+'.expired').write_text('expired')
        sys.exit(97)
    time.sleep(0.01)
os.execv({real:?}, [{real:?}] + sys.argv[1:])
"#,
            parser = CARGO_SUBCOMMAND_PY,
            ready = ready.to_str().unwrap(),
            release = release.to_str().unwrap(),
            real = real.to_str().unwrap()
        ),
    );
}

#[test]
fn source_mutation_during_bootstrap_cannot_publish_a_trusted_stamp() {
    let sandbox = bootstrap();
    require_first(&sandbox);
    park_cargo(&sandbox);
    let mut command = sandbox.command("/bin/bash");
    command.args(["xtask/gate.sh", "--plan"]);
    let child = sandbox.spawn(&mut command);
    wait_for(&sandbox.temporary.join("build-ready"), HANDSHAKE_BUDGET);
    assert!(
        !stamp(&sandbox).exists(),
        "stamp was not removed before build"
    );
    sandbox.write("xtask/src/main.rs", source("MUTATED_DURING_BUILD"));
    fs::write(sandbox.temporary.join("build-release"), "release\n").unwrap();
    let output = child.finish(COMMAND_BUDGET);
    require_parked_worker_exit(&sandbox);
    assert!(
        !output.status.success(),
        "identity race was accepted: {}",
        output.text()
    );
    assert!(!stamp(&sandbox).exists(), "race published a trusted stamp");
}

#[test]
fn interrupted_bootstrap_never_leaves_a_completed_stamp() {
    for signal in ["INT", "TERM", "HUP", "KILL"] {
        let sandbox = bootstrap();
        require_first(&sandbox);
        park_cargo(&sandbox);
        let mut command = sandbox.command("/bin/bash");
        command.args(["xtask/gate.sh", "--plan"]);
        let mut child = sandbox.spawn(&mut command);
        wait_for(&sandbox.temporary.join("build-ready"), HANDSHAKE_BUDGET);
        assert!(!stamp(&sandbox).exists());
        child.signal(signal);
        // A signal-ignoring shell now gets a successful build, and would stamp
        // it. Boundary timeout/build failure can no longer satisfy BOOT-INT.
        fs::write(sandbox.temporary.join("build-release"), "release\n").unwrap();
        let output = child.finish(COMMAND_BUDGET);
        require_parked_worker_exit(&sandbox);
        assert!(!output.status.success());
        assert!(!stamp(&sandbox).exists());
    }
}

#[test]
fn missing_tracked_judge_input_cannot_reuse_a_trusted_success_stamp() {
    let sandbox = bootstrap();
    require_first(&sandbox);
    // Public input refusal works with either PATH or absolute Git. F-owned
    // internal fault tests still owe enumeration/hash subprocess error coverage.
    fs::remove_file(sandbox.root.join("xtask/src/main.rs")).unwrap();
    let output = run(&sandbox);
    output.code(1);
    output.contains("REFUSED");
    assert!(
        !output.stdout.contains("source_marker=FIRST"),
        "missing input still launched judge"
    );
    assert!(
        !stamp(&sandbox).exists(),
        "missing input retained trusted stamp"
    );
}

// BOOT-OWNER foreign UID and BOOT-RACE second-launcher ordering need a reviewed
// ownership/fault/admission observation seam; see the frozen handoff ledger.
// They are neither silently skipped nor replaced with arbitrary private files.

// Normal-development authorization supersedes the historical launch hold.
// Signal/race controls observe the parked process itself and reject its expiry;
// absence of a stamp or EOF alone is not a process-exit oracle.

#[test]
fn unchanged_bootstrap_preserves_target_and_stamp_identity_and_invalid_stamp_resets() {
    let sandbox = bootstrap();
    let first = require_first(&sandbox);
    let canary = sandbox.root.join("target/gate-xtask/unchanged-canary");
    fs::write(&canary, "same judge\n").unwrap();
    let output = run(&sandbox);
    output.code(0);
    assert_eq!(returned_id(&output), first);
    assert_eq!(fs::read_to_string(&canary).unwrap(), "same judge\n");
    fs::write(stamp(&sandbox), "not a trusted identity\n").unwrap();
    let output = run(&sandbox);
    output.code(0);
    assert_eq!(returned_id(&output), first);
    assert_eq!(fs::read_to_string(stamp(&sandbox)).unwrap().trim(), first);
    assert!(!canary.exists());
}

fn require_parked_worker_exit(sandbox: &Sandbox) {
    let ready = sandbox.temporary.join("build-ready");
    let pid = fs::read_to_string(&ready).unwrap().parse::<u32>().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let out =
            sandbox.run(
                sandbox
                    .command("/bin/ps")
                    .args(["-p", &pid.to_string(), "-o", "pid="]),
            );
        if out.status.code() == Some(1) && out.stdout.trim().is_empty() {
            break;
        }
        assert!(out.status.success(), "process query failed: {}", out.text());
        assert!(
            std::time::Instant::now() < deadline,
            "parked bootstrap worker still exists: {}",
            out.text()
        );
        std::thread::park_timeout(witness::fixture::POLL_INTERVAL);
    }
    assert!(
        !sandbox.temporary.join("build-ready.expired").exists(),
        "fixture self-expiry cannot prove interrupt/race protection"
    );
    let mut command = sandbox.command("/usr/bin/python3");
    command.args(["-c", "import fcntl,os,sys; fd=os.open(sys.argv[1],os.O_RDONLY|os.O_NOFOLLOW); fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)"]).arg(sandbox.root.join("target/.gate-xtask.lock"));
    sandbox.run(&mut command).code(0);
}
