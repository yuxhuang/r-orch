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

use std::os::unix::process::CommandExt;
use std::process::{Child, Stdio};
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
    let mut d = seeded_owned(effort, "2.1.2");
    let configured = d.sb.run(
        &new_reader(),
        &["config", "set", "scheduling.poll_interval_ms", "100"],
        None,
    );
    assert!(configured.ok(), "{}", configured.all());
    d.sb.stop_daemon();
    d
}

/// [`seeded`], with the project's own test daemon: for [`dispatch`], which
/// sends `orch run` — a daemon RPC since S2a-cli — rather than starting one.
fn seeded_owned(effort: &str, version: &str) -> Dispatched {
    seeded_in(
        Sandbox::new_owned_with_version(&new_reader(), version),
        effort,
    )
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
    let Dispatched { sb, task, .. } = seeded_owned(effort, version);
    // The agent CLI is spawned and probed by the daemon, which was started
    // with `version` in its environment.
    let out = sb.run(&new, &["run", &task], None);
    let model_argv = sb.recorded("opencode");
    Dispatched {
        sb,
        task,
        out,
        model_argv,
    }
}

/// Reap the foreground owner even when a fixture assertion fails.
struct Foreground {
    child: Child,
    reaped: bool,
}

impl Drop for Foreground {
    fn drop(&mut self) {
        if !self.reaped {
            let pid = nix::unistd::Pid::from_raw(self.child.id().try_into().unwrap());
            let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

/// `run --watch` is a foreground daemon. Capture its stderr until the seeded
/// task is done, then stop it cleanly so the count includes shutdown output.
fn watch_output(d: &Dispatched, version: &str) -> String {
    daemon_output(d, version, &["run", "--watch"])
}

fn daemon_output(d: &Dispatched, version: &str, args: &[&str]) -> String {
    let new = new_reader();
    let (short, socket) = daemon::short_socket();
    let stderr_path = short.path().join("foreground.err");
    let mut cmd = d.sb.command(&new);
    daemon::apply_env(&mut cmd, &socket, short.path());
    let child = cmd
        .args(args)
        .env("ORCH_FAKE_VERSION", version)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn daemon owner");
    let mut foreground = Foreground {
        child,
        reaped: false,
    };
    let detached = args.contains(&"--detach");
    if detached {
        let status = foreground.child.wait().unwrap();
        foreground.reaped = true;
        assert!(
            status.success(),
            "detached start failed: {}",
            std::fs::read_to_string(&stderr_path).unwrap()
        );
    }
    // Query through the same isolated endpoint; no direct state-file reads.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut done = false;
    while Instant::now() < deadline {
        if !detached && foreground.child.try_wait().unwrap().is_some() {
            break;
        }
        let mut query = d.sb.command(&new);
        daemon::apply_env(&mut query, &socket, short.path());
        let out = query
            .args(["--json", "task", "show", &d.task])
            .output()
            .unwrap();
        if out.status.success()
            && serde_json::from_slice::<Value>(&out.stdout).unwrap()["status"] == "done"
        {
            done = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut stop = d.sb.command(&new);
    daemon::apply_env(&mut stop, &socket, short.path());
    let stopped = stop.args(["daemon", "stop"]).output().unwrap();
    if !stopped.status.success() {
        let _ = foreground.child.kill();
    }
    let status = foreground.child.wait().unwrap();
    foreground.reaped = true;
    let seen = if detached {
        std::fs::read_to_string(d.sb.root().join("daemon.log")).unwrap()
    } else {
        std::fs::read_to_string(stderr_path).unwrap()
    };
    assert!(done, "foreground task never completed: {status}: {seen}");
    assert!(stopped.status.success(), "stop failed: {stopped:?}");
    assert!(status.success(), "foreground exit {status}: {seen}");
    assert_eq!(d.sb.recorded("opencode").len(), 1, "one model child");
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
fn the_legacy_ignore_is_surfaced_to_the_operator_exactly_once() {
    let d = seeded("high");
    let watched = watch_output(&d, "1.9.0");
    assert_eq!(watched.matches("legacy-ignored").count(), 1, "{watched}");
}

#[test]
fn serve_and_explicit_start_surface_the_diagnostic_once() {
    for args in [&["serve"][..], &["daemon", "start", "--dispatch", "on"][..]] {
        let d = seeded("high");
        let watched = daemon_output(&d, "1.9.0", args);
        assert_eq!(
            watched.matches("legacy-ignored").count(),
            1,
            "{args:?}: {watched}"
        );
    }
}

#[test]
fn detached_start_writes_the_diagnostic_once_to_daemon_log() {
    let d = seeded("high");
    let logged = daemon_output(
        &d,
        "1.9.0",
        &["daemon", "start", "--detach", "--dispatch", "on"],
    );
    assert_eq!(logged.matches("legacy-ignored").count(), 1, "{logged}");
}

#[test]
fn startup_recovery_diagnostic_is_visible_with_dispatch_off_without_raw_payload() {
    let sb = Sandbox::new(&new_reader());
    let context = sb.root().join("context");
    std::fs::create_dir_all(&context).unwrap();
    std::fs::write(context.join("broken.json"), "{SECRET=startup-canary").unwrap();
    let owner = daemon::TestDaemon::start(&sb.project, sb.home.path());
    let seen = std::fs::read_to_string(owner.socket_dir().join("daemon.err")).unwrap();
    owner.shutdown();
    assert_eq!(
        seen.matches("Context index rebuild skipped: invalid context entity")
            .count(),
        1,
        "{seen}"
    );
    assert!(!seen.contains("SECRET"), "{seen}");
    assert!(!seen.contains("startup-canary"), "{seen}");
}
