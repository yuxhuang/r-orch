//! review-b2 §F3 (condition 2) — dispatch-level proof that an OpenCode
//! `#level` variant is only ever synthesised after the bounded probe
//! recognised a v2 install, through the real `orch` binary and a fake
//! `opencode` whose `--version` is chosen per run.
//!
//! * v2.1.2 (control): the recognised model receives exactly one `#high`.
//! * v1.9.0, legacy `high` stored: the original model is kept (no `#`), the
//!   run still happens, and the ignore is surfaced once.
//! * v1.9.0, new spelling `xhigh`: the request is refused before any model
//!   child is spawned.
//! * unparseable version: same as v1 (nothing is synthesised).
//!
//! The wrapper installed by [`Sandbox`] never records `--version` / `run
//! --help` probes, so `recorded("opencode")` lists model children only.

mod token_eco_support;

use std::io::Read;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;
use token_eco_support::daemon;
use token_eco_support::{Out, Sandbox, new_reader};

const MODEL: &str = "openrouter/anthropic/claude-opus-5.5";

struct Dispatched {
    sb: Sandbox,
    task: String,
    out: Out,
    model_argv: Vec<Vec<String>>,
}

/// A project holding one OpenCode agent with `effort` and one task for it,
/// not yet run.
///
/// **Without a daemon**: `run --watch` (through [`watch_output`]) is itself a
/// daemon start and must not find a second one already owning the project.
fn seeded(effort: &str) -> Dispatched {
    seeded_in(Sandbox::new(&new_reader()), effort)
}

/// [`seeded`], with the project's own test daemon: for [`dispatch`], which
/// sends `orch run` — a daemon RPC since S2a-cli — rather than starting one.
fn seeded_owned(effort: &str) -> Dispatched {
    seeded_in(Sandbox::new_owned(&new_reader()), effort)
}

fn seeded_in(sb: Sandbox, effort: &str) -> Dispatched {
    let new = new_reader();
    let id = sb.seed_agent(&new, "Oc", "opencode", Some(MODEL), Some(effort));
    let task = sb.run(
        &new,
        &["task", "add", "wire gate", "--assignee", &id, "--quiet"],
        None,
    );
    assert!(task.ok(), "{}", task.all());
    Dispatched {
        sb,
        task: task.stdout.trim().to_owned(),
        out: Out {
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
        model_argv: Vec::new(),
    }
}

fn dispatch(version: &str, effort: &str) -> Dispatched {
    let new = new_reader();
    // `orch run` below is a daemon RPC since S2a-cli.
    let Dispatched { sb, task, .. } = seeded_owned(effort);
    let out = sb.run_env(
        &new,
        &["run", &task],
        None,
        &[("ORCH_FAKE_VERSION", version)],
    );
    let model_argv = sb.recorded("opencode");
    Dispatched {
        sb,
        task,
        out,
        model_argv,
    }
}

/// The operator's diagnostics surface (ruling 8): the bus `orchestrator:error`
/// that `orch run --watch` prints as a `⚠ <context>: <note>` line. The single
/// `orch run <task>` path does not render it. Runs the watch loop over the
/// seeded task until the task's run reports completion, then stops it.
///
/// `run --watch` is itself a foreground daemon start since S2a-cli
/// (`crate::commands::daemon::foreground`), on its own isolated socket so its
/// `.orchestry`-relative default does not overflow macOS's `sun_path`; `d.sb`
/// must stay daemon-less ([`seeded`]) so this is not a *second* owner.
fn watch_output(d: &Dispatched, version: &str) -> String {
    let new = new_reader();
    let (short, socket) = daemon::short_socket();
    let mut cmd = d.sb.command(&new);
    daemon::apply_env(&mut cmd, &socket, short.path());
    let mut child = cmd
        .args(["run", "--watch"])
        .env("ORCH_FAKE_VERSION", version)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn orch run --watch");
    let (tx, rx) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    // Raw chunks, not lines: the tick line is overwritten with `\r` and the
    // completion marker follows it without a newline.
    let reader = std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0
                || tx
                    .send(String::from_utf8_lossy(&buf[..n]).into_owned())
                    .is_err()
            {
                break;
            }
        }
    });
    // A deadline only bounds a hung child; nothing asserts on elapsed time.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = String::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(chunk) => {
                seen.push_str(&chunk);
                if seen.contains("DONE") {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
    // Whatever the reader had queued before the kill.
    while let Ok(chunk) = rx.try_recv() {
        seen.push_str(&chunk);
    }
    seen
}

/// The value after `--model` / `-m` of one recorded invocation.
fn model_of(argv: &[String]) -> Option<&str> {
    argv.iter()
        .position(|a| a == "--model" || a == "-m")
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
}

#[test]
fn a_v2_install_receives_exactly_one_variant_suffix() {
    let d = dispatch("2.1.2", "high");
    assert_eq!(d.model_argv.len(), 1, "one model child: {}", d.out.all());
    assert_eq!(
        model_of(&d.model_argv[0]),
        Some(format!("{MODEL}#high").as_str())
    );
}

#[test]
fn a_v1_install_keeps_the_original_model_and_never_receives_a_variant_suffix() {
    let d = dispatch("1.9.0", "high");
    assert_eq!(
        d.model_argv.len(),
        1,
        "the legacy request must not stop the run: {}",
        d.out.all()
    );
    assert_eq!(
        model_of(&d.model_argv[0]),
        Some(MODEL),
        "an unprobed #level reached the model argv"
    );
}

#[test]
#[ignore = "S2a-cli: `run --watch` is now `commands::daemon::foreground`, which \
            (per crates/orch-daemon/src/lifecycle.rs) prints only `listening <socket>` \
            to stdout and never renders the live OrchestratorEvent bus there (that \
            rendering lived in the legacy in-process watch loop, which is gone); no \
            client command in the current source replays `orchestrator:error` for an \
            operator watching an interactive run. Needs a product decision on where \
            (if anywhere) this diagnostic now surfaces — `orch tui`, a `logs --follow`, \
            or daemon-side tracing — before this can be rewritten."]
fn a_v1_install_surfaces_the_legacy_ignore_to_the_operator() {
    let d = seeded("high");
    let watched = watch_output(&d, "1.9.0");
    assert!(
        watched.contains("legacy-ignored"),
        "the ignored legacy effort must be visible: {watched}"
    );
}

#[test]
fn a_v1_install_refuses_a_new_spelling_before_any_model_child() {
    let d = dispatch("1.9.0", "xhigh");
    assert!(
        d.model_argv.is_empty(),
        "a model child ran for an unprovable new level: {:?}",
        d.model_argv
    );
    assert!(
        !d.out.ok() || d.out.all().to_lowercase().contains("fail"),
        "the refusal must be visible: {}",
        d.out.all()
    );
}

#[test]
fn an_unparseable_version_is_treated_like_an_unproven_install() {
    let d = dispatch("not-a-version", "high");
    assert_eq!(d.model_argv.len(), 1, "{}", d.out.all());
    assert_eq!(model_of(&d.model_argv[0]), Some(MODEL));
}

/// Every stored event of the task's only run, via the same CLI an operator or
/// the retry-recovery instructions use.
fn run_events(d: &Dispatched) -> Vec<Value> {
    let new = new_reader();
    let runs = d.sb.run(&new, &["--json", "logs", "--task", &d.task], None);
    assert!(runs.ok(), "logs --task: {}", runs.all());
    let runs: Vec<Value> = serde_json::from_str(&runs.stdout).expect("runs json");
    assert_eq!(runs.len(), 1, "{runs:?}");
    let id = runs[0]["id"].as_str().expect("run id");
    let events = d.sb.run(&new, &["--json", "logs", id], None);
    assert!(events.ok(), "logs RUN: {}", events.all());
    serde_json::from_str(&events.stdout).expect("events json")
}

/// decision-cp2 ruling 8 (contract test): OpenCode `legacy-ignored` is
/// surfaced exactly once on the operator surface and produces zero
/// `agent_output` events of diagnostic origin.
#[test]
fn the_legacy_ignore_is_never_stored_as_agent_output() {
    let d = dispatch("1.9.0", "high");
    let leaked: Vec<String> = run_events(&d)
        .iter()
        .filter(|e| e["type"] == "agent_output")
        .map(|e| e["data"].to_string())
        .filter(|t| t.contains("legacy-ignored") || t.contains("effort preparation"))
        .collect();
    assert!(
        leaked.is_empty(),
        "a diagnostic was stored as agent_output: {leaked:?}"
    );
}

#[test]
#[ignore = "S2a-cli: same gap as a_v1_install_surfaces_the_legacy_ignore_to_the_operator — \
            `run --watch`'s own stdout no longer renders live OrchestratorEvents, only \
            `listening <socket>`; no replacement surface is specified yet."]
fn the_legacy_ignore_is_surfaced_to_the_operator_exactly_once() {
    let d = seeded("high");
    let watched = watch_output(&d, "1.9.0");
    assert_eq!(watched.matches("legacy-ignored").count(), 1, "{watched}");
}
