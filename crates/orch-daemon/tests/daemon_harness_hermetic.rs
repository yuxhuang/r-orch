#![cfg(feature = "testing")]
//! Default child/discovery isolation and explicit fixture overrides.
mod support;

use orch_core::process::{SpawnOptions, SpawnedChild};
use orch_daemon::{DaemonError, testing::DaemonHarness};
use orch_infra::process::gate::{SpawnObservation, SpawnObserver, SpawnSite, SpawnStage};
use serde_json::json;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};
use support::{DEADLINE, Raw, ok};
use tokio::io::AsyncReadExt;
use tokio::sync::Notify;

const CHILD_ROOT: &str = "ORCH_TEST_HERMETIC_ROOT";

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

// Hostile ambient values live only in a fresh process. Even the red control
// can resolve only test-owned canaries, never an installed AI executable.
fn hostile_process(name: &str, body: impl AsyncFnOnce(&Path)) {
    hostile_process_with_runtime(name, false, body);
}

fn hostile_process_with_runtime(name: &str, xdg: bool, body: impl AsyncFnOnce(&Path)) {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(body(Path::new(&root)));
        return;
    }
    let root = tempfile::tempdir().unwrap();
    for dir in ["home/.codex", "bin", "tmp", "runtime"] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    std::fs::write(root.path().join("home/launch-canary"), "ambient-home").unwrap();
    std::fs::write(
        root.path().join("home/.codex/models_cache.json"),
        r#"{"models":[{"slug":"ambient-home-model"}]}"#,
    )
    .unwrap();
    for name in [
        "claude",
        "codex",
        "opencode",
        "cursor-agent",
        "gemini",
        "grok",
        "agy",
        "pi",
    ] {
        executable(
            &root.path().join("bin").join(name),
            "#!/bin/sh\nprintf called >> \"$HOME/ambient-executed\"\nexit 0\n",
        );
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.env_clear();
    if xdg {
        command.env("XDG_RUNTIME_DIR", root.path().join("runtime"));
    }
    let mut child = command
        .args(["--exact", name, "--nocapture"])
        .env(CHILD_ROOT, root.path())
        .env("HOME", root.path().join("home"))
        .env("PATH", root.path().join("bin"))
        .env("TMPDIR", root.path().join("tmp"))
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            root.path().join("home/global.yml"),
        )
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    assert!(child.wait().unwrap().success(), "isolated {name} failed");
}

fn options(daemon: &DaemonHarness) -> SpawnOptions {
    SpawnOptions {
        cwd: daemon.project_root().to_owned(),
        ..Default::default()
    }
}

async fn output(mut child: SpawnedChild) -> String {
    let mut text = String::new();
    tokio::time::timeout(
        DEADLINE,
        child.stdout.take().unwrap().read_to_string(&mut text),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        tokio::time::timeout(DEADLINE, child.exit.wait())
            .await
            .unwrap()
            .success()
    );
    text
}

async fn shell(daemon: &DaemonHarness, script: &str) -> String {
    let child = daemon
        .engine()
        .processes
        .spawn("/bin/sh", &["-c".into(), script.into()], options(daemon))
        .unwrap();
    output(child).await
}

async fn environment(daemon: &DaemonHarness) -> Vec<PathBuf> {
    shell(
        daemon,
        "printf '%s\\n' \"$HOME\" \"$TMPDIR\" \"$PATH\" \"$ORCHESTRY_GLOBAL_CONFIG_PATH\"",
    )
    .await
    .lines()
    .map(PathBuf::from)
    .collect()
}

#[test]
fn default_discovery_ignores_hostile_home_and_path() {
    hostile_process(
        "default_discovery_ignores_hostile_home_and_path",
        async |root| {
            // Neither socket binding nor a config-path seam opts into inheritance.
            let daemon = DaemonHarness::builder()
                .on_socket()
                .global_config_path(root.join("explicit-global.yml"))
                .start()
                .await
                .unwrap();
            let mut raw = Raw::session(&daemon, "cli", "normal").await;
            let reply = raw.request(1, "models.catalog", json!({"adapters":["claude", "codex", "opencode", "cursor", "grok", "antigravity", "pi"]})).await;
            let text = ok(&reply).to_string();
            daemon.shutdown().await.unwrap();
            assert_eq!(
                (
                    root.join("home/ambient-executed").exists(),
                    text.contains("ambient-home-model")
                ),
                (false, false),
                "ambient PATH execution / HOME cache read: {text}"
            );
            assert_eq!(
                std::fs::read_to_string(root.join("home/.codex/models_cache.json")).unwrap(),
                r#"{"models":[{"slug":"ambient-home-model"}]}"#
            );
        },
    );
}

#[test]
fn default_launch_ignores_hostile_home_and_path() {
    hostile_process(
        "default_launch_ignores_hostile_home_and_path",
        async |root| {
            let daemon = DaemonHarness::start().await.unwrap();
            let bare = daemon
                .engine()
                .processes
                .spawn("claude", &[], options(&daemon));
            let resolved = match bare {
                Ok(child) => {
                    output(child).await;
                    true
                }
                Err(_) => false,
            };
            let seen = shell(&daemon, "if [ -f \"$HOME/launch-canary\" ]; then /bin/cat \"$HOME/launch-canary\"; fi; printf touched > \"$HOME/launch-touched\"").await;
            let vars = environment(&daemon).await;
            daemon.shutdown().await.unwrap();
            assert_eq!(
                (
                    resolved,
                    root.join("home/ambient-executed").exists(),
                    root.join("home/launch-touched").exists(),
                    seen.contains("ambient-home")
                ),
                (false, false, false, false),
                "ambient PATH resolution/execution and HOME write/read"
            );
            assert_ne!(vars[0], root.join("home"));
            assert_ne!(vars[1], root.join("tmp"));
            assert_ne!(vars[2], root.join("bin"));
        },
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_harnesses_have_private_immutable_environments() {
    let (first, second) = tokio::join!(DaemonHarness::start(), DaemonHarness::start());
    let first = first.unwrap();
    let second = second.unwrap();
    let a = environment(&first).await;
    let b = environment(&second).await;
    assert_eq!(a.len(), 4);
    assert_eq!(b.len(), 4);
    for (a, b) in a.iter().zip(&b) {
        assert_ne!(a, b);
    }
    for vars in [&a, &b] {
        let scratch = vars[0].parent().unwrap();
        assert_eq!(vars[0], scratch.join("home"));
        assert_eq!(vars[1], scratch.join("tmp"));
        assert_eq!(vars[2], scratch.join("bin"));
        assert_eq!(vars[3], scratch.join("global.yml"));
        assert_eq!(
            std::fs::metadata(scratch).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for dir in &vars[..3] {
            assert!(dir.is_dir());
        }
        assert_eq!(std::fs::read_dir(&vars[2]).unwrap().count(), 0);
    }
    let mut overlay = options(&first);
    overlay
        .env
        .insert("HOME".into(), first.project_root().display().to_string());
    let child = first
        .engine()
        .processes
        .spawn(
            "/bin/sh",
            &["-c".into(), "printf '%s' \"$HOME\"".into()],
            overlay,
        )
        .unwrap();
    assert_eq!(
        output(child).await,
        first.project_root().display().to_string()
    );
    assert_eq!(
        environment(&first).await,
        a,
        "child overlay mutated the snapshot"
    );
    first.shutdown().await.unwrap();
    assert_eq!(
        environment(&second).await,
        b,
        "first teardown affected second"
    );
    second.shutdown().await.unwrap();
}

#[test]
fn supplied_environment_replaces_defaults_without_ambient_merge() {
    hostile_process(
        "supplied_environment_replaces_defaults_without_ambient_merge",
        async |root| {
            let home = root.join("supplied-home");
            let bin = root.join("supplied-bin");
            let tmp = root.join("supplied-tmp");
            for dir in [&home, &bin, &tmp] {
                std::fs::create_dir(dir).unwrap();
            }
            executable(
                &bin.join("claude"),
                "#!/bin/sh\nprintf fixture >> \"$HOME/supplied-executed\"\nprintf '%s' \"$FIXTURE_ONLY\"\n",
            );
            let global = home.join("global.yml");
            let daemon = DaemonHarness::builder()
                .environment([
                    (OsString::from("HOME"), home.clone().into_os_string()),
                    ("PATH".into(), bin.clone().into_os_string()),
                    ("TMPDIR".into(), tmp.clone().into_os_string()),
                    (
                        "ORCHESTRY_GLOBAL_CONFIG_PATH".into(),
                        global.clone().into_os_string(),
                    ),
                    ("FIXTURE_ONLY".into(), "explicit-fixture".into()),
                    ("ORCH_AGENT_ID".into(), "must-be-sanitized".into()),
                ])
                .start()
                .await
                .unwrap();
            assert_eq!(environment(&daemon).await, [home.clone(), tmp, bin, global]);
            let child = daemon
                .engine()
                .processes
                .spawn("claude", &[], options(&daemon))
                .unwrap();
            assert_eq!(output(child).await, "explicit-fixture");
            let mut raw = Raw::session(&daemon, "cli", "normal").await;
            ok(&raw
                .request(1, "models.catalog", json!({"adapters":["claude"]}))
                .await);
            assert_eq!(
                std::fs::read_to_string(home.join("supplied-executed")).unwrap(),
                "fixturefixture"
            );
            assert_eq!(
                shell(
                    &daemon,
                    "printf '%s' \"${ORCH_AGENT_ID-unset}\" \"${ORCH_TEST_HERMETIC_ROOT-unset}\""
                )
                .await,
                "unsetunset"
            );
            assert!(!root.join("home/ambient-executed").exists());
            daemon.shutdown().await.unwrap();
            // An omitted default stays absent; the override is a replacement,
            // not an overlay on the harness's private environment.
            let minimal = DaemonHarness::builder()
                .environment([
                    ("HOME".into(), root.join("supplied-home").into_os_string()),
                    ("PATH".into(), root.join("supplied-bin").into_os_string()),
                ])
                .start()
                .await
                .unwrap();
            assert_eq!(
                shell(
                    &minimal,
                    "printf '%s' \"${TMPDIR-unset}\" \"${ORCHESTRY_GLOBAL_CONFIG_PATH-unset}\""
                )
                .await,
                "unsetunset"
            );
            minimal.shutdown().await.unwrap();
        },
    );
}

#[derive(Default)]
struct HeldSpawn {
    entered: Notify,
    gate: (Mutex<bool>, Condvar),
}
impl HeldSpawn {
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}
impl SpawnObserver for HeldSpawn {
    fn observe(&self, observation: SpawnObservation) {
        if observation.site == SpawnSite::Discovery && observation.stage == SpawnStage::Entered {
            self.entered.notify_one();
            let mut open = self.gate.0.lock().unwrap();
            while !*open {
                open = self.gate.1.wait(open).unwrap();
            }
        }
    }
}
struct Release(Arc<HeldSpawn>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

async fn scratch_during_drain(drop_harness: bool) {
    let daemon = DaemonHarness::builder().on_socket().start().await.unwrap();
    let scratch = daemon.socket().unwrap().parent().unwrap().to_owned();
    let marker = scratch.join("lifetime-canary");
    std::fs::write(&marker, "retained").unwrap();
    let held = Arc::new(HeldSpawn::default());
    let _release = Release(held.clone());
    daemon.observe_spawns(held.clone());
    let mut raw = Raw::session(&daemon, "cli", "normal").await;
    let request = tokio::spawn(async move {
        raw.request(1, "models.catalog", json!({"adapters":["claude"]}))
            .await
    });
    tokio::time::timeout(DEADLINE, held.entered.notified())
        .await
        .unwrap();
    let shutdown = if drop_harness {
        let socket = daemon.socket().unwrap().to_owned();
        drop(daemon);
        tokio::time::timeout(DEADLINE, async {
            while socket.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        None
    } else {
        let mut shutdown = tokio::spawn(daemon.shutdown());
        assert!(
            tokio::time::timeout(Duration::from_secs(11), &mut shutdown)
                .await
                .is_err(),
            "shutdown ended while an accepted spawn job was held"
        );
        Some(shutdown)
    };
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "retained",
        "scratch removed before late spawn/drain completed"
    );
    held.release();
    let reply = tokio::time::timeout(DEADLINE, request)
        .await
        .unwrap()
        .unwrap();
    ok(&reply);
    if let Some(shutdown) = shutdown {
        assert!(
            matches!(
                tokio::time::timeout(DEADLINE, shutdown)
                    .await
                    .unwrap()
                    .unwrap(),
                Err(DaemonError::StopIncomplete)
            ),
            "holding spawn work beyond the stop deadline must report an incomplete stop"
        );
    }
    tokio::time::timeout(DEADLINE, async {
        while scratch.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scratch removed after drain");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scratch_outlives_late_spawn_and_shutdown() {
    scratch_during_drain(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scratch_outlives_late_spawn_and_drop() {
    scratch_during_drain(true).await;
}

#[tokio::test]
async fn synthetic_opt_in_still_requires_a_supplied_manager() {
    let error = DaemonHarness::builder()
        .isolated_fake_spawns()
        .start()
        .await
        .err()
        .expect("must fail closed");
    assert!(
        matches!(error, DaemonError::InvalidArguments(ref text) if text == "isolated fake spawns require a supplied process manager")
    );
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synthetic_token_cannot_authorize_the_default_process_manager() {
    let daemon = DaemonHarness::start().await.unwrap();
    let domain = orch_infra::process::gate::SpawnDomain::for_synthetic_processes();
    let reservation = domain.reserve(SpawnSite::Launch).await;
    let mut escaped = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reservation.run(|| {
            escaped = Some(
                daemon
                    .engine()
                    .processes
                    .spawn("/bin/sh", &["-c".into(), "exit 0".into()], options(&daemon))
                    .unwrap(),
            );
        });
    }));
    if let Some(child) = escaped {
        output(child).await;
    }
    let panic = result.expect_err("synthetic token authorized a real process");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap();
    assert!(
        message.contains("synthetic spawn reservation cannot authorize a real process"),
        "{message}"
    );
    assert_eq!(shell(&daemon, "printf recovered").await, "recovered");
    daemon.shutdown().await.unwrap();
}

// Record the native filesystem effect at the process boundary, before a fast
// fake child lets the event stream drop its prompt-file guard.
#[derive(Default)]
struct PromptProcesses {
    prompts: Mutex<Vec<(PathBuf, String, u32)>>,
}

#[async_trait::async_trait]
impl orch_core::process::ProcessManager for PromptProcesses {
    fn is_alive(&self, pid: u32) -> bool {
        let _ = pid;
        false
    }
    fn kill(&self, pid: u32, signal: orch_core::process::Signal) {
        let _ = (pid, signal);
    }
    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        let _ = (pid, grace);
    }
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        opts: SpawnOptions,
    ) -> orch_core::error::Result<SpawnedChild> {
        if let Some(pair) = args
            .windows(2)
            .find(|pair| pair[0] == "--system-prompt-file")
        {
            let path = PathBuf::from(&pair[1]);
            self.prompts.lock().unwrap().push((
                path.clone(),
                std::fs::read_to_string(&path).unwrap(),
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            ));
        }
        let _ = (command, opts);
        Ok(SpawnedChild {
            pid: 987654,
            stdout: Some(Box::new(tokio::io::empty())),
            stderr: Some(Box::new(tokio::io::empty())),
            stdin: Some(Box::new(tokio::io::sink())),
            exit: orch_core::process::ExitSignal::resolved(orch_core::process::ExitOutcome {
                code: Some(0),
                signal: None,
            }),
        })
    }
}

async fn prompt_canary(root: &Path, xdg: bool, dispatch: bool) {
    let ambient = if xdg {
        root.join("runtime/orch")
    } else {
        std::env::temp_dir().join(format!("orch-{}", nix::unistd::Uid::current()))
    };
    assert!(ambient.starts_with(root), "canary must be test-owned");
    std::fs::create_dir(&ambient).unwrap();
    std::fs::set_permissions(&ambient, std::fs::Permissions::from_mode(0o700)).unwrap();
    let stale = ambient.join("orch-claude-system-prompt-canary.txt");
    std::fs::write(&stale, "ambient canary").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(90000))
        .unwrap();
    let processes = Arc::new(PromptProcesses::default());
    let daemon = DaemonHarness::builder()
        .on_socket()
        .processes(processes.clone())
        .isolated_fake_spawns()
        .start()
        .await
        .unwrap();
    let scratch = daemon.socket().unwrap().parent().unwrap().to_owned();
    assert!(!ambient.starts_with(&scratch));
    if dispatch {
        let mut raw = Raw::session(&daemon, "cli", "normal").await;
        let reply = raw.request(1, "agent.create", json!({"name":"prompt canary", "adapter":"claude", "workspace_mode":"shared", "approval_policy":"manual", "role":"private system prompt"})).await;
        let agent = ok(&reply)["agent"]["id"].as_str().unwrap().to_owned();
        let task = support::task(&mut raw, "prepare the prompt", json!({"assignee":agent})).await;
        let reply = raw.request(2, "dispatch.tick", json!({})).await;
        assert_eq!(ok(&reply)["launched"], json!([task]));
    } else {
        let handle = daemon
            .engine()
            .adapters
            .get("claude")
            .unwrap()
            .execute(orch_core::adapter::ExecuteParams {
                prompt: "test prompt".into(),
                system_prompt: Some("private system prompt".into()),
                workspace: daemon.project_root().into(),
                env: Default::default(),
                inherit_env: Default::default(),
                config: Default::default(),
                cancel: Default::default(),
            })
            .unwrap();
        drop(handle);
    }
    let prompts = processes.prompts.lock().unwrap().clone();
    daemon.shutdown().await.unwrap();
    println!(
        "prompt case XDG={xdg} dispatch={dispatch}: observations={} paths={:?}",
        prompts.len(),
        prompts.iter().map(|p| &p.0).collect::<Vec<_>>()
    );
    assert!(!prompts.is_empty(), "prompt launch must be observed");
    assert_eq!(
        std::fs::read_to_string(&stale).ok().as_deref(),
        Some("ambient canary"),
        "ambient stale prompt must remain untouched"
    );
    assert_eq!(
        std::fs::read_dir(&ambient).unwrap().count(),
        1,
        "no ambient prompt may be written"
    );
    assert_eq!(prompts.len(), 1);
    let (path, text, mode) = &prompts[0];
    assert!(
        path.starts_with(&scratch),
        "prompt outside retained scratch: {path:?}"
    );
    assert!(text.contains("private system prompt"));
    assert_eq!(*mode, 0o600);
}

#[test]
fn claude_prompt_ignores_ambient_runtime_directory() {
    hostile_process_with_runtime(
        "claude_prompt_ignores_ambient_runtime_directory",
        true,
        async |root| prompt_canary(root, true, false).await,
    );
}

#[test]
fn claude_prompt_ignores_ambient_temp_directory() {
    hostile_process_with_runtime(
        "claude_prompt_ignores_ambient_temp_directory",
        false,
        async |root| prompt_canary(root, false, false).await,
    );
}

#[test]
fn dispatched_claude_prompt_stays_in_harness_scratch() {
    hostile_process_with_runtime(
        "dispatched_claude_prompt_stays_in_harness_scratch",
        true,
        async |root| prompt_canary(root, true, true).await,
    );
}
