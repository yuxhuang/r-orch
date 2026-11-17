//! Miniature diagnostic controls, never native/CI acceptance evidence.
#[path = "gate_witness.rs"]
mod witness;

use witness::fixture::Sandbox;

#[test]
fn receipt_interface_is_explicit_and_scoped() {
    let s = Sandbox::new();
    s.run(&mut s.gate(&["pr", "--part", "suite", "--receipt", "receipt", "--plan"]))
        .code(0);
    s.run(&mut s.gate(&[
        "evidence",
        "verify-receipt",
        "missing",
        "--commit",
        &"a".repeat(40),
    ]))
    .code(1);
    for args in [
        vec!["full", "--part", "suite", "--receipt", "receipt", "--plan"],
        vec!["pr", "--receipt", "receipt", "--plan"],
        vec!["pr", "--part", "static", "--receipt", "receipt", "--plan"],
        vec!["fast", "--receipt", "receipt", "--plan"],
        vec![
            "pr",
            "--part",
            "suite",
            "--receipt",
            "receipt",
            "--evidence",
            "evidence",
            "--plan",
        ],
    ] {
        s.run(&mut s.gate(&args)).code(2);
    }
    assert!(!s.root.join("target").exists());
    assert!(!s.root.join("receipt").exists());
}

use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};
use witness::fixture::{Capture, field, private_diagnostic, rust_literal, wait_for};

fn miniature(body: &str) -> Sandbox {
    let s = Sandbox::new();
    s.mini(body, true);
    s.write(
        "xtask/gate.sh",
        "#!/bin/sh\n# synthetic judge input, never executed\nexit 2\n",
    );
    s.commit();
    block_all_agents(&s);
    s
}
fn block_all_agents(s: &Sandbox) {
    for name in "agent agy aider amp claude codex continue copilot cursor cursor-agent droid gemini goose grok opencode pi qwen".split_whitespace() {
        s.script(&s.temporary.join("bin").join(name), "#!/bin/sh\nexit 127\n");
    }
}

fn execute(s: &Sandbox, destination: &Path) -> Capture {
    s.run(
        s.gate(&["pr", "--part", "suite", "--lease-wait", "0", "--receipt"])
            .arg(destination),
    )
}
fn verify(s: &Sandbox, destination: &Path) -> Capture {
    s.run(
        s.gate(&["evidence", "verify-receipt"])
            .arg(destination)
            .args(["--commit", &s.git(&["rev-parse", "HEAD"])]),
    )
}
fn retain(s: &Sandbox, label: &str, out: &Capture, destination: &Path) {
    if let Some(root) = std::env::var_os("XTASK_F2_PRIVATE_ROOT") {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::path::PathBuf::from(root).join(format!(
            "{label}-{}",
            s.temporary.file_name().unwrap().to_string_lossy()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        private_diagnostic(&root.join("stdout"), &out.stdout_bytes).unwrap();
        private_diagnostic(&root.join("stderr"), &out.stderr_bytes).unwrap();
        private_diagnostic(
            &root.join("status"),
            format!("{:?}\n", out.status).as_bytes(),
        )
        .unwrap();
        if let Ok(result) = fs::read(s.root.join("target/gate-result.txt")) {
            private_diagnostic(&root.join("result.txt"), &result).unwrap();
        }
        if destination.is_dir() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join("receipt"))
                .unwrap();
            for name in [
                "MANIFEST",
                "result.txt",
                "inventory.tsv",
                "tools.txt",
                "environment.txt",
            ] {
                if let Ok(bytes) = fs::read(destination.join(name)) {
                    private_diagnostic(&root.join("receipt").join(name), &bytes).unwrap();
                }
            }
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join("receipt/steps"))
                .unwrap();
            for name in ["build", "test", "home-hygiene", "no-real-cli", "real-home"] {
                if let Ok(bytes) = fs::read(destination.join(format!("steps/{name}.log"))) {
                    private_diagnostic(&root.join(format!("receipt/steps/{name}.log")), &bytes)
                        .unwrap();
                }
            }
        }
    }
}

#[test]
fn miniature_receipt_retains_named_inventory_but_safety_failures_stay_nonzero() {
    let s = miniature(
        "/// ```\n/// assert_eq!(2 + 2, 4);\n/// ```\npub fn example() {}\n#[test] fn sample() {}\n",
    );
    let dest = s.temporary.join("receipt");
    let out = execute(&s, &dest);
    retain(&s, "success", &out, &dest);
    // Private lease override and unstamped direct judge are intentionally unsafe
    // for acceptance. A successful miniature must not hide those predicates.
    out.code(3);
    assert_eq!(field(&s.result(), "step-test-exit"), "0");
    let inv = fs::read_to_string(dest.join("inventory.tsv")).unwrap();
    assert!(
        inv.contains("sample\tok") && inv.lines().any(|line| line.starts_with("doc\t")),
        "{inv}"
    );
    let verified = verify(&s, &dest);
    verified.code(0);
    verified.contains("completeness=complete; outcome=passed; safety=failed-or-unestablished");
    s.run(
        s.gate(&["evidence", "verify"])
            .arg(&dest)
            .args(["--commit", &s.git(&["rev-parse", "HEAD"])]),
    )
    .code(1);
}

#[test]
fn failed_suite_receipt_redacts_secrets_and_never_streams_workflow_commands() {
    let s = miniature(
        r#"#[test] fn intentional_failure() {
        assert!(std::process::Command::new("/usr/bin/printf").args(["%s\n", &std::env::var("SYNTHETIC_API_KEY").unwrap(), "::error::injected\x1b[31m"]).status().unwrap().success());
        panic!("original diagnostic retained");
    }"#,
    );
    let dest = s.temporary.join("receipt");
    let out = s.run(
        s.gate(&["pr", "--part", "suite", "--receipt"])
            .arg(&dest)
            .env("SYNTHETIC_API_KEY", "receipt-secret-canary"),
    );
    retain(&s, "failed", &out, &dest);
    out.code(1);
    assert_eq!(field(&s.result(), "step-test-exit"), "101");
    assert!(
        fs::read_to_string(dest.join("steps/test.log"))
            .unwrap()
            .contains("original diagnostic retained")
    );
    assert!(!out.text().contains("receipt-secret-canary"));
    assert!(!out.text().lines().any(|line| line.starts_with("::")));
    assert!(!out.text().contains('\x1b'));
    for name in ["result.txt", "steps/build.log", "steps/test.log"] {
        let bytes = fs::read_to_string(dest.join(name)).unwrap();
        assert!(!bytes.contains("receipt-secret-canary") && !bytes.contains('\x1b'));
    }
    let verified = verify(&s, &dest);
    verified.code(0);
    verified.contains("outcome=failed");
    verified.contains("acceptance=not-granted");
}

#[test]
fn requested_export_failure_cannot_be_success_and_keeps_original_run() {
    let s = miniature("#[test] fn sample() {}\n");
    let dest = s.temporary.join("receipt");
    fs::create_dir(&dest).unwrap();
    let out = execute(&s, &dest);
    retain(&s, "export-failure", &out, &dest);
    out.code(1);
    out.contains("destination already exists");
    assert_eq!(field(&s.result(), "step-test-exit"), "0");
    assert!(!dest.join("MANIFEST").exists());
    assert!(
        Path::new(field(&s.result(), "run-dir"))
            .join("complete")
            .is_file()
    );
}

#[test]
fn handled_interruption_preserves_progress_and_incomplete_diagnostic_receipt() {
    interruption_case(false);
}

#[test]
fn interruption_keeps_signal_status_when_requested_export_also_fails() {
    interruption_case(true);
}

fn interruption_case(export_collision: bool) {
    let s = Sandbox::new();
    let ready = s.temporary.join("ready");
    s.mini("#[test] fn sample() {}", true);
    s.write("xtask/gate.sh", "#!/bin/sh\n# synthetic input\nexit 2\n");
    s.commit();
    block_all_agents(&s);
    let forward = s.cargo_forwarder();
    // Replace only the execution boundary with a direct inert worker. Killing
    // real Cargo alone can leave a libtest grandchild: that is not this test's
    // cleanup authority. Builds/maps still use the original pinned tool.
    let worker = s.temporary.join("test-worker.py");
    s.script(&worker, &format!("#!/usr/bin/python3\nimport pathlib, signal\npathlib.Path({}).write_text('ready')\nsignal.pause()\n", rust_literal(&ready)));
    s.script(
        &s.temporary.join("bin/cargo"),
        &format!(
            r#"#!/usr/bin/python3
import os, sys
{}
if cargo_subcommand(sys.argv[1:]) == 'test' and '--no-run' not in sys.argv:
    os.execv({}, [{}])
os.execv({}, [{}] + sys.argv[1:])
"#,
            witness::fixture::CARGO_SUBCOMMAND_PY,
            rust_literal(&worker),
            rust_literal(&worker),
            rust_literal(&forward),
            rust_literal(&forward)
        ),
    );
    let dest = s.temporary.join("receipt");
    if export_collision {
        fs::create_dir(&dest).unwrap();
    }
    let mut child = s.spawn(s.gate(&["pr", "--part", "suite", "--receipt"]).arg(&dest));
    wait_for(&ready, Duration::from_secs(30));
    let progress = child.diagnostic();
    assert!(progress.contains("test phase entered; final receipt pending"));
    child.signal("TERM");
    let out = child.finish(Duration::from_secs(30));
    retain(&s, "interrupted", &out, &dest);
    out.code(143);
    if export_collision {
        assert!(!dest.join("MANIFEST").exists());
        out.contains("final export failed; required receipt unavailable");
    } else {
        let verified = verify(&s, &dest);
        verified.code(0);
        verified.contains("completeness=incomplete; outcome=interrupted");
        verified.contains("acceptance=not-granted");
    }
}

#[test]
fn abrupt_loss_before_admission_leaves_pending_progress_without_final_artifact() {
    let s = miniature("#[test] fn sample() {}\n");
    // Hold only this fixture's admission turnstile. No target/cargo child can
    // start; runner loss therefore cannot create a target writer to clean up.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(s.temporary.join("lease/r-orch-gate.turnstile"))
        .unwrap();
    lock.try_lock().unwrap();
    let dest = s.temporary.join("receipt");
    let mut child = s.spawn(
        s.gate(&["pr", "--part", "suite", "--lease-wait", "1", "--receipt"])
            .arg(&dest),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while !child
        .diagnostic()
        .contains("loss before export means incomplete diagnostics")
    {
        assert!(Instant::now() < deadline);
        std::thread::park_timeout(Duration::from_millis(10));
    }
    child.signal("KILL");
    let out = child.finish(Duration::from_secs(30));
    retain(&s, "lost", &out, &dest);
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.status.signal(), Some(9));
    assert!(!dest.exists());
    assert!(s.result().starts_with("verdict: IN PROGRESS"));
    assert!(!s.root.join("target/gate-warm").exists());
    // Abrupt loss leaves intentionally incomplete resources retained; no target
    // deletion, lock theft or recursive cleanup is used to manufacture success.
    private_diagnostic(
        &s.temporary.join("cleanup-unresolved"),
        b"abrupt loss; incomplete diagnostic fixture retained\n",
    )
    .unwrap();
    drop(lock); // close only
}

#[test]
fn contention_keeps_status_75_when_requested_export_also_fails() {
    let s = miniature("#[test] fn sample() {}\n");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(s.temporary.join("lease/r-orch-gate.lease"))
        .unwrap();
    lock.try_lock().unwrap();
    let dest = s.temporary.join("receipt");
    fs::create_dir(&dest).unwrap();
    let out = execute(&s, &dest);
    retain(&s, "contended-export-failure", &out, &dest);
    out.code(75);
    assert!(!dest.join("MANIFEST").exists());
    assert!(!s.root.join("target/gate-warm").exists());
    drop(lock); // close only
}
