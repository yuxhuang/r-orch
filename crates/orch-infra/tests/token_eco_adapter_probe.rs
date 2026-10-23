//! T1 / D5-9 and D5-10: the OpenCode wire preflight against **fake
//! executables** run by the real `SystemProcessManager`.
//!
//! `B2`'s inline tests drive the probe with a hand-written `ProcessManager`
//! double. These tests instead put real child processes (small `sh` scripts in
//! a temp dir) behind the production process manager, so the pipe draining,
//! deadline, process-group kill and reaping run for real. The wrapper below
//! maps the command `opencode` to the fake script and **panics on any other
//! command**, so no real AI CLI can be reached and `PATH` is never consulted.
//!
//! Only one scenario waits out the real 2 s deadline, and the timing scenarios
//! run in parallel with the rest, so the file's wall time is ~2-3 s.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use orch_core::adapter::{Adapter, ExecuteParams};
use orch_core::process::{
    EnvInheritance, ProcessManager, Signal, SpawnOptions, SpawnedChild, StdioMode,
};
use orch_domain::{AgentConfig, ReasoningEffort as E};
use orch_infra::adapters::OpenCodeAdapter;
use orch_infra::process::SystemProcessManager;
use tokio_util::sync::CancellationToken;

const MODEL: &str = "openrouter/anthropic/claude-opus-5.5";
const GOOD_HELP: &str = "Usage: opencode run [message..]\n\nOptions:\n  -m, --model   model to use in the format of provider/model#variant\n";

#[derive(Debug, Clone)]
struct Call {
    args: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    inherit: EnvInheritance,
    stdin: StdioMode,
}

/// Real process manager; `opencode` is redirected to a fake script.
struct FakeBin {
    inner: SystemProcessManager,
    script: PathBuf,
    calls: Mutex<Vec<Call>>,
}

#[async_trait]
impl ProcessManager for FakeBin {
    fn is_alive(&self, pid: u32) -> bool {
        self.inner.is_alive(pid)
    }
    fn kill(&self, pid: u32, signal: Signal) {
        self.inner.kill(pid, signal)
    }
    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        self.inner.kill_with_grace(pid, grace).await
    }
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        opts: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        assert_eq!(command, "opencode", "only the fake OpenCode may be spawned");
        self.calls.lock().unwrap().push(Call {
            args: args.to_vec(),
            cwd: opts.cwd.clone(),
            env: opts.env.clone(),
            inherit: opts.inherit_env,
            stdin: opts.stdin,
        });
        // Run the fake as data for the always-hot /bin/sh rather than exec'ing
        // a freshly written file: a first exec of a new file can be scanned by
        // the OS, which on a loaded machine alone can eat the probe's 2 s.
        let mut argv = vec![self.script.to_str().unwrap().to_owned()];
        argv.extend_from_slice(args);
        self.inner.spawn("/bin/sh", &argv, opts)
    }
}

/// What the fake `opencode --version` / `run --help` do.
#[derive(Clone, Default)]
struct Fake {
    version: &'static str,
    /// Shell fragment replacing the default version behaviour.
    version_sh: Option<String>,
    help: &'static str,
    help_sh: Option<String>,
}

impl Fake {
    fn good() -> Self {
        Fake {
            version: "2.1.2",
            help: GOOD_HELP,
            ..Default::default()
        }
    }
    fn version(v: &'static str) -> Self {
        Fake {
            version: v,
            ..Fake::good()
        }
    }
    fn version_sh(sh: &str) -> Self {
        Fake {
            version_sh: Some(sh.to_owned()),
            ..Fake::good()
        }
    }
    fn help(h: &'static str) -> Self {
        Fake {
            help: h,
            ..Fake::good()
        }
    }
    fn help_sh(sh: &str) -> Self {
        Fake {
            help_sh: Some(sh.to_owned()),
            ..Fake::good()
        }
    }
}

struct Rig {
    dir: tempfile::TempDir,
    pm: Arc<FakeBin>,
    adapter: OpenCodeAdapter,
}

impl Rig {
    fn new(fake: Fake) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let version = fake
            .version_sh
            .clone()
            .unwrap_or_else(|| format!("printf '%s\\n' '{}'", fake.version));
        let help = fake.help_sh.clone().unwrap_or_else(|| {
            let hf = dir.path().join("help.txt");
            std::fs::write(&hf, fake.help).unwrap();
            format!("cat '{}'", hf.display())
        });
        let script = dir.path().join("fake-opencode");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nD='{d}'\n\
                 version() {{\n{version}\n}}\n\
                 help() {{\n{help}\n}}\n\
                 case \"$1\" in\n\
                 --version) version ;;\n\
                 run) if [ \"$2\" = \"--help\" ]; then help; else : > \"$D/model-child-ran\"; printf '%s\\n' '{{\"type\":\"step_finish\",\"part\":{{\"reason\":\"stop\"}}}}'; fi ;;\n\
                 *) exit 64 ;;\n\
                 esac\n",
                d = dir.path().display(),
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let pm = Arc::new(FakeBin {
            inner: SystemProcessManager::new(),
            script,
            calls: Mutex::new(Vec::new()),
        });
        let adapter = OpenCodeAdapter::new(Arc::clone(&pm) as Arc<dyn ProcessManager>);
        Rig { dir, pm, adapter }
    }

    fn params(&self, model: Option<&str>, effort: Option<E>) -> ExecuteParams {
        let mut env = BTreeMap::new();
        env.insert("ORCH_T1_MARK".to_owned(), "1".to_owned());
        ExecuteParams {
            prompt: "the user prompt".into(),
            system_prompt: None,
            workspace: self.dir.path().to_path_buf(),
            env,
            inherit_env: EnvInheritance::WithoutSecrets,
            config: AgentConfig {
                model: model.map(str::to_owned),
                effort,
                ..Default::default()
            },
            cancel: CancellationToken::new(),
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.pm.calls.lock().unwrap().clone()
    }
    fn probe_args(&self) -> Vec<String> {
        self.calls().iter().map(|c| c.args.join(" ")).collect()
    }
    fn model_child_ran(&self) -> bool {
        self.dir.path().join("model-child-ran").exists()
    }
}

/// Prepare, then (when preparation succeeds) execute with the prepared config
/// exactly as dispatch does. Returns the argv of the model child.
async fn dispatch(
    rig: &Rig,
    model: Option<&str>,
    effort: Option<E>,
) -> Result<(AgentConfig, Vec<String>, Vec<String>), String> {
    let params = rig.params(model, effort);
    let prepared = rig
        .adapter
        .prepare_effort(&params)
        .await
        .map_err(|e| e.message)?;
    let mut run = rig.params(model, effort);
    run.config = prepared.config.clone();
    let handle = rig.adapter.execute(run).map_err(|e| e.message)?;
    let _ = support_drain(handle).await;
    let argv = rig
        .calls()
        .into_iter()
        .map(|c| c.args)
        .find(|a| {
            a.first().map(String::as_str) == Some("run")
                && a.get(1).map(String::as_str) == Some("--format")
        })
        .expect("the model child was spawned");
    Ok((prepared.config, prepared.diagnostics, argv))
}

async fn support_drain(handle: orch_core::adapter::ExecuteHandle) -> usize {
    use futures::StreamExt;
    let mut n = 0;
    let mut events = handle.events;
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while events.next().await.is_some() {
            n += 1;
        }
    })
    .await;
    n
}

fn model_of(argv: &[String]) -> Option<&str> {
    argv.iter()
        .position(|a| a == "--model")
        .map(|i| argv[i + 1].as_str())
}

fn pid_alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn assert_dead_soon(pid: i32, what: &str) {
    for _ in 0..200 {
        if !pid_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Do not leave it behind.
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    panic!("{what} (pid {pid}) survived the probe cleanup");
}

// ── success ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_v2_binary_with_the_variant_signature_permits_synthesis_after_exactly_two_probes() {
    let _gate = GATE.read().await;
    let rig = Rig::new(Fake::good());
    let (cfg, diags, argv) = dispatch(&rig, Some(MODEL), Some(E::Xhigh)).await.unwrap();
    assert_eq!(cfg.effort, Some(E::Xhigh), "effort is preserved on success");
    assert_eq!(model_of(&argv), Some(format!("{MODEL}#xhigh").as_str()));
    assert_eq!(rig.probe_args()[..2], ["--version", "run --help"]);
    assert_eq!(rig.calls().len(), 3, "two probes then one model child");
    assert!(rig.model_child_ran());
    // D5-10: the request is reported as a request with the override caveat,
    // never as a confirmed effective level.
    assert!(
        diags.iter().any(|d| d.starts_with("request-only:")),
        "{diags:?}"
    );
    assert!(
        diags
            .iter()
            .any(|d| d.starts_with("local-override-possible:")),
        "{diags:?}"
    );
    for d in &diags {
        let l = d.to_lowercase();
        assert!(
            !l.contains("confirmed") && !l.contains("verified effective"),
            "{d}"
        );
    }
}

#[tokio::test]
async fn probes_run_in_the_workspace_with_the_agent_environment_and_no_stdin() {
    let _gate = GATE.read().await;
    let rig = Rig::new(Fake::good());
    dispatch(&rig, Some(MODEL), Some(E::High)).await.unwrap();
    for call in &rig.calls()[..2] {
        assert_eq!(call.cwd, rig.dir.path());
        assert_eq!(call.env.get("ORCH_T1_MARK").map(String::as_str), Some("1"));
        assert_eq!(
            call.inherit,
            EnvInheritance::WithoutSecrets,
            "mirror secret filter kept"
        );
        assert_eq!(format!("{:?}", call.stdin), "Ignore");
    }
}

#[tokio::test]
async fn prefixed_and_v_prefixed_v2_versions_and_stderr_only_output_are_recognised() {
    let _gate = GATE.read().await;
    for sh in [
        "printf 'opencode 2.3.4\\n'",
        "printf 'v2.0.0\\n'",
        "printf '2.1.2\\n' >&2",
    ] {
        let rig = Rig::new(Fake::version_sh(sh));
        let (cfg, _, argv) = dispatch(&rig, Some(MODEL), Some(E::High)).await.unwrap();
        assert_eq!(cfg.effort, Some(E::High), "{sh}");
        assert_eq!(
            model_of(&argv),
            Some(format!("{MODEL}#high").as_str()),
            "{sh}"
        );
    }
}

#[tokio::test]
async fn effort_unset_runs_no_probe_and_leaves_the_model_untouched() {
    let _gate = GATE.read().await;
    let rig = Rig::new(Fake::good());
    let (cfg, diags, argv) = dispatch(&rig, Some(MODEL), None).await.unwrap();
    assert_eq!(cfg.effort, None);
    assert!(diags.is_empty());
    assert_eq!(rig.calls().len(), 1, "only the model child; no preflight");
    assert_eq!(model_of(&argv), Some(MODEL));
}

// ── failure outcomes ────────────────────────────────────────────────────────

/// Every wire failure: legacy low/medium/high keep the original model and warn
/// (the model child still runs); a new value errors with no model child.
async fn assert_wire_failure(fake: Fake, reason_fragment: &str) {
    for legacy in [E::Low, E::Medium, E::High] {
        let rig = Rig::new(fake.clone());
        let (cfg, diags, argv) = dispatch(&rig, Some(MODEL), Some(legacy))
            .await
            .unwrap_or_else(|e| panic!("legacy {legacy} must not fail: {e}"));
        assert_eq!(
            cfg.effort, None,
            "generic effort cleared on the invocation copy"
        );
        assert_eq!(cfg.model.as_deref(), Some(MODEL), "model bytes untouched");
        assert_eq!(model_of(&argv), Some(MODEL), "no #variant on the wire");
        assert!(rig.model_child_ran(), "legacy dispatch continues");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].starts_with("legacy-ignored:"), "{diags:?}");
        assert!(
            diags[0].contains(reason_fragment),
            "{} !~ {reason_fragment}",
            diags[0]
        );
    }
    for new in [E::Xhigh, E::Max] {
        let rig = Rig::new(fake.clone());
        let err = dispatch(&rig, Some(MODEL), Some(new)).await.unwrap_err();
        assert!(err.contains(reason_fragment), "{err} !~ {reason_fragment}");
        assert!(
            !rig.model_child_ran(),
            "a new value must not start a model child"
        );
        assert!(
            !rig.calls()
                .iter()
                .any(|c| c.args.first().map(String::as_str) == Some("run")
                    && c.args.get(1).map(String::as_str) == Some("--format")),
            "no model argv was even attempted"
        );
    }
}

#[tokio::test]
async fn a_v1_binary_is_not_given_a_variant_suffix() {
    let _gate = GATE.read().await;
    assert_wire_failure(Fake::version("1.14.0"), "not recognized major 2").await;
}

#[tokio::test]
async fn a_v3_binary_is_not_given_a_variant_suffix() {
    let _gate = GATE.read().await;
    assert_wire_failure(Fake::version("3.0.0"), "not recognized major 2").await;
}

#[tokio::test]
async fn an_unparseable_version_is_not_given_a_variant_suffix() {
    let _gate = GATE.read().await;
    for v in ["dev-build", "2.1", "2.x.1", "2.1.2-beta", "", "two.one.two"] {
        let rig = Rig::new(Fake::version_sh(&format!("printf '%s\\n' '{v}'")));
        let (cfg, diags, argv) = dispatch(&rig, Some(MODEL), Some(E::High)).await.unwrap();
        assert_eq!(cfg.effort, None, "{v:?}");
        assert_eq!(model_of(&argv), Some(MODEL), "{v:?}");
        assert!(diags[0].starts_with("legacy-ignored:"), "{v:?}: {diags:?}");
    }
}

#[tokio::test]
async fn help_without_the_model_variant_signature_is_not_given_a_variant_suffix() {
    let _gate = GATE.read().await;
    assert_wire_failure(
        Fake::help(
            "Usage: opencode run\n  -m, --model  provider/model\n  --variant  reasoning variant\n",
        ),
        "lacks the v2 model variant signature",
    )
    .await;
}

#[tokio::test]
async fn a_signature_on_a_line_without_the_model_flag_does_not_count() {
    let _gate = GATE.read().await;
    assert_wire_failure(
        Fake::help("--agent  see provider/model#variant in the docs\n  --model  x\n"),
        "lacks the v2 model variant signature",
    )
    .await;
}

#[tokio::test]
async fn a_nonzero_probe_exit_is_a_wire_failure() {
    let _gate = GATE.read().await;
    assert_wire_failure(
        Fake::version_sh("printf '2.1.2\\n'; exit 3"),
        "exited unsuccessfully",
    )
    .await;
}

#[tokio::test]
async fn non_utf8_probe_output_is_a_wire_failure() {
    let _gate = GATE.read().await;
    assert_wire_failure(Fake::version_sh("printf '\\377\\376\\n'"), "not UTF-8").await;
}

#[tokio::test]
async fn help_phase_failure_is_a_wire_failure_after_a_good_version() {
    let _gate = GATE.read().await;
    assert_wire_failure(Fake::help_sh("exit 7"), "exited unsuccessfully").await;
}

#[tokio::test]
async fn probe_output_over_64_kib_is_cut_off_and_never_grown_unbounded() {
    let _gate = GATE.read().await;
    // 70 000 bytes on stdout (> 65 536) and, separately, 33 000 on each pipe
    // (combined > 65 536).
    assert_wire_failure(
        Fake::version_sh("head -c 70000 /dev/zero | tr '\\0' 'a'"),
        "exceeded 64 KiB",
    )
    .await;
    assert_wire_failure(
        Fake::version_sh(
            "head -c 33000 /dev/zero | tr '\\0' 'a'; head -c 33000 /dev/zero | tr '\\0' 'b' >&2",
        ),
        "exceeded 64 KiB",
    )
    .await;
    assert_wire_failure(
        Fake::help_sh("head -c 70000 /dev/zero | tr '\\0' 'a'"),
        "exceeded 64 KiB",
    )
    .await;
}

#[tokio::test]
async fn output_just_under_the_cap_is_still_read_in_full() {
    let _gate = GATE.read().await;
    // 65 000 bytes of padding then the real version: the cap is not hit.
    let rig = Rig::new(Fake::version_sh(
        "head -c 65000 /dev/zero | tr '\\0' ' '; printf '\\n2.1.2\\n'",
    ));
    let (cfg, _, _) = dispatch(&rig, Some(MODEL), Some(E::High)).await.unwrap();
    assert_eq!(cfg.effort, Some(E::High));
}

#[tokio::test]
async fn diagnostics_never_contain_raw_probe_output() {
    let _gate = GATE.read().await;
    let rig = Rig::new(Fake::version_sh(
        "printf 'SECRET-TOKEN-abc123 sk-live-xyz\\n' >&2; exit 9",
    ));
    let (_, diags, _) = dispatch(&rig, Some(MODEL), Some(E::High)).await.unwrap();
    for d in &diags {
        assert!(!d.contains("SECRET-TOKEN") && !d.contains("sk-live"), "{d}");
    }
    let rig = Rig::new(Fake::version_sh(
        "printf 'SECRET-TOKEN-abc123\\n' >&2; exit 9",
    ));
    let err = dispatch(&rig, Some(MODEL), Some(E::Max)).await.unwrap_err();
    assert!(!err.contains("SECRET-TOKEN"), "{err}");
}

/// macOS has no `pipe2`: std creates a pipe and sets CLOEXEC in two steps, so
/// a `fork` on another thread inside that window inherits the fresh write end.
/// If that fork is (or becomes) one of the long-lived `sleep 60` descendants
/// the group tests deliberately leave behind, an unrelated probe's stdout does
/// not reach EOF until the descendant is killed ~2 s later and the probe hits
/// its own 2 s deadline (measured: 3/50 full-file runs failed before this
/// gate). The three process-group scenarios therefore run together, exclusively,
/// while every other scenario holds the read side.
static GATE: std::sync::LazyLock<tokio::sync::RwLock<()>> =
    std::sync::LazyLock::new(|| tokio::sync::RwLock::new(()));

#[tokio::test]
async fn process_group_scenarios_hang_pipe_holder_and_cancel_are_cleaned_up_together() {
    let _exclusive = GATE.write().await;
    tokio::join!(
        a_hanging_probe_is_cut_off_at_the_deadline_and_its_whole_process_group_is_reaped(),
        a_probe_whose_descendant_holds_the_pipe_open_after_the_parent_exits_is_still_cut_off(),
        cancelling_preparation_kills_the_probe_group_without_waiting_for_the_deadline(),
    );
}

// ── deadline and cleanup (real 2 s; the two cases run in parallel) ──────────

async fn a_hanging_probe_is_cut_off_at_the_deadline_and_its_whole_process_group_is_reaped() {
    // The probe forks a grandchild that outlives a naive parent kill.
    let rig = Rig::new(Fake::version_sh(
        "sleep 60 & echo $! > \"$D/grandchild.pid\"; echo $$ > \"$D/child.pid\"; wait",
    ));
    let started = Instant::now();
    let (cfg, diags, argv) = dispatch(&rig, Some(MODEL), Some(E::High)).await.unwrap();
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(1900),
        "returned before the 2 s deadline: {took:?}"
    );
    // 2 s deadline + kill + reap. A 3 s deadline (or none) must fail this.
    assert!(
        took < Duration::from_millis(2900),
        "deadline is not 2 s: {took:?}"
    );
    assert_eq!(cfg.effort, None);
    assert_eq!(model_of(&argv), Some(MODEL));
    assert!(diags[0].contains("exceeded 2 s"), "{diags:?}");
    for f in ["child.pid", "grandchild.pid"] {
        let pid: i32 = std::fs::read_to_string(rig.dir.path().join(f))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_dead_soon(pid, f).await;
    }
}

async fn a_probe_whose_descendant_holds_the_pipe_open_after_the_parent_exits_is_still_cut_off() {
    // Parent prints a valid version and exits; a background sleeper keeps
    // stdout open so EOF never arrives.
    let rig = Rig::new(Fake::version_sh(
        "printf '2.1.2\\n'; sleep 60 & echo $! > \"$D/grandchild.pid\"",
    ));
    let started = Instant::now();
    let (cfg, diags, _) = dispatch(&rig, Some(MODEL), Some(E::Low)).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(cfg.effort, None, "an unterminated probe proves nothing");
    assert!(diags[0].starts_with("legacy-ignored:"), "{diags:?}");
    let pid: i32 = std::fs::read_to_string(rig.dir.path().join("grandchild.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_dead_soon(pid, "pipe-holding descendant").await;
}

async fn cancelling_preparation_kills_the_probe_group_without_waiting_for_the_deadline() {
    let rig = Rig::new(Fake::version_sh(
        "sleep 60 & echo $! > \"$D/grandchild.pid\"; echo $$ > \"$D/child.pid\"; wait",
    ));
    let params = rig.params(Some(MODEL), Some(E::High));
    let token = params.cancel.clone();
    let canceller = tokio::spawn({
        let dir: PathBuf = rig.dir.path().to_path_buf();
        async move {
            // Cancel once the probe child is demonstrably running.
            for _ in 0..500 {
                if std::fs::read_to_string(dir.join("grandchild.pid"))
                    .is_ok_and(|c| !c.trim().is_empty())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            token.cancel();
        }
    });
    let started = Instant::now();
    let _ = rig.adapter.prepare_effort(&params).await;
    canceller.await.unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(1800),
        "cancel waited for the deadline"
    );
    let pid: i32 = std::fs::read_to_string(rig.dir.path().join("grandchild.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_dead_soon(pid, "grandchild after cancel").await;
}

// ── no real CLI ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_rig_can_only_ever_run_the_fake_script() {
    let _gate = GATE.read().await;
    // The wrapper asserts `command == "opencode"` and substitutes the script;
    // this guards the guard: the script path is inside the temp dir.
    let rig = Rig::new(Fake::good());
    assert!(rig.pm.script.starts_with(rig.dir.path()));
    dispatch(&rig, Some(MODEL), Some(E::High)).await.unwrap();
}
