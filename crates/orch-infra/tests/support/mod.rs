//! The one shared harness for every adapter test.
//!
//! This harness uses real async pipes and a **real** child running a
//! generated `sh` script: scripted stdout, scripted stderr, a
//! scripted exit, and an optional stdin capture. That costs a few
//! milliseconds per case and in exchange every test exercises the real pipe,
//! decode and reaping path.
//!
//! [`ScriptedProcessManager`] also records every `spawn` and
//! `kill_with_grace`, so argv, cwd, environment and the adapters' own kill
//! calls are assertable without a second double.

#![allow(dead_code, reason = "each integration test binary uses a subset")]

use std::collections::{BTreeMap, HashSet};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use orch_core::adapter::{AdapterError, AgentEvent, AgentEventKind, ExecuteHandle, ExecuteParams};
use orch_core::clock::Clock;
use orch_core::process::{
    EnvInheritance, ExitOutcome, ExitSignal, ProcessManager, REMOTE_ENV_PASSTHROUGH, Signal,
    SpawnOptions, SpawnedChild, StdioMode, secret_env_names,
};
use orch_core::test_support::FixedClock;
use orch_domain::AgentConfig;
use orch_infra::process::SystemProcessManager;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Wall-clock ceiling for a scripted run. Any test that hits it is a hang,
/// which is a failure rather than a timeout to be retried.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// Marker the stdin capture appends once it sees EOF, so a test can tell
/// "stdin was closed" from "stdin is still open".
pub const STDIN_EOF_MARKER: &str = "<<ORCH_STDIN_EOF>>";

#[derive(Debug, Clone)]
pub enum Step {
    /// Write bytes to stdout.
    Stdout(Vec<u8>),
    /// Write bytes to stderr.
    Stderr(Vec<u8>),
    /// Pause, for the tests that need the child alive for a moment.
    SleepMs(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    /// Kill itself, so the exit has a signal and no code.
    Signal(i32),
    /// Never exit on its own — for the long-lived pi session.
    Hang,
}

/// What a scripted child does. Build it with the fluent helpers.
#[derive(Debug, Clone)]
pub struct Script {
    steps: Vec<Step>,
    exit: Exit,
    capture_stdin: bool,
}

impl Default for Script {
    fn default() -> Self {
        Script {
            steps: Vec::new(),
            exit: Exit::Code(0),
            capture_stdin: false,
        }
    }
}

impl Script {
    pub fn new() -> Self {
        Script::default()
    }

    /// One JSONL line per element, newline-terminated.
    pub fn lines<I, S>(lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut joined = String::new();
        for line in lines {
            joined.push_str(line.as_ref());
            joined.push('\n');
        }
        Script::new().stdout(joined)
    }

    pub fn stdout(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.steps.push(Step::Stdout(bytes.into()));
        self
    }

    pub fn stderr(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.steps.push(Step::Stderr(bytes.into()));
        self
    }

    pub fn sleep_ms(mut self, ms: u64) -> Self {
        self.steps.push(Step::SleepMs(ms));
        self
    }

    pub fn exit(mut self, code: i32) -> Self {
        self.exit = Exit::Code(code);
        self
    }

    pub fn killed_by(mut self, signal: i32) -> Self {
        self.exit = Exit::Signal(signal);
        self
    }

    pub fn hang(mut self) -> Self {
        self.exit = Exit::Hang;
        self
    }

    /// Drain stdin into a file the test can read back, appending
    /// [`STDIN_EOF_MARKER`] when the writer closes it.
    pub fn capturing_stdin(mut self) -> Self {
        self.capture_stdin = true;
        self
    }
}

#[derive(Debug, Clone)]
pub struct SpawnRecord {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub stdin: StdioMode,
}

impl SpawnRecord {
    /// The value immediately after `flag`, for the "`--model` is followed by
    /// the model" style of assertion.
    pub fn value_after(&self, flag: &str) -> Option<&str> {
        let at = self.args.iter().position(|arg| arg == flag)?;
        self.args.get(at + 1).map(String::as_str)
    }

    pub fn has_flag(&self, flag: &str) -> bool {
        self.args.iter().any(|arg| arg == flag)
    }

    pub fn last_arg(&self) -> &str {
        self.args.last().map(String::as_str).unwrap_or_default()
    }
}

/// A [`ProcessManager`] that spawns a scripted real child and records what it
/// was asked for.
pub struct ScriptedProcessManager {
    inner: SystemProcessManager,
    script: Script,
    dir: tempfile::TempDir,
    /// Commands whose `spawn` fails with `ENOENT`, for the `test()` probes.
    missing: Mutex<HashSet<String>>,
    /// Each group has an unreaped anchor, so numeric group identities cannot
    /// be recycled before cleanup, even after the scripted child exits.
    children: Mutex<BTreeMap<u32, OwnedScript>>,
    spawns: Mutex<Vec<SpawnRecord>>,
    kills: Mutex<Vec<(u32, Signal)>>,
    graceful_kills: Mutex<Vec<(u32, Duration)>>,
}

/// No reaper ever receives the anchor. Its unreaped PID reserves the PGID until
/// after the last signal. The joined observer owns only the script's wait, not
/// cleanup; no runtime, detached task, process-table search or PID fallback is
/// needed. This is fixture lifetime cleanup, not crash/SIGKILL recovery.
///
/// ARCHITECTURE §4.8 compatibility exemption: the fixture must retain group
/// identity *past* the script's reaping. A normal process wrapper owns/reaps the
/// script itself; std Child plus process_group supplies the separate anchor
/// without a dependency or changes to the production process port.
struct OwnedScript {
    anchor: Child,
    child: Arc<Mutex<Option<Child>>>,
    observer: Option<std::thread::JoinHandle<()>>,
    exit: ExitSignal,
}

impl OwnedScript {
    fn signal(&self, signal: Signal) {
        use nix::sys::signal::{Signal as S, killpg};
        use nix::unistd::Pid;
        let signal = match signal {
            Signal::Term => S::SIGTERM,
            Signal::Kill => S::SIGKILL,
            Signal::Int => S::SIGINT,
            Signal::Hup => S::SIGHUP,
        };
        // process_group(0) established this group at spawn. The unreaped
        // anchor prevents PGID reuse, even if an explicit kill already ran.
        let pgid = i32::try_from(self.anchor.id()).expect("positive OS pid");
        debug_assert!(pgid > 0 && Pid::from_raw(pgid) != nix::unistd::getpgrp());
        let _ = killpg(Pid::from_raw(pgid), signal);
    }
}

impl Drop for OwnedScript {
    fn drop(&mut self) {
        // Cleanup must not depend on progress of a diagnostic reader.
        self.signal(Signal::Kill);
        // Reap the anchor only AFTER signalling; never signal this PGID again.
        let _ = self.anchor.wait();
        if let Some(observer) = self.observer.take() {
            let _ = observer.join();
        }
        // Also handles failure before the exit-observer thread was installed.
        if let Some(mut child) = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = child.wait();
        }
    }
}

impl Drop for ScriptedProcessManager {
    fn drop(&mut self) {
        // Finish owned work before removing the scripts/capture directory.
        self.children
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

impl ScriptedProcessManager {
    pub fn new(script: Script) -> Arc<Self> {
        Arc::new(ScriptedProcessManager {
            inner: SystemProcessManager::new(),
            script,
            dir: tempfile::tempdir().expect("tempdir"),
            missing: Mutex::new(HashSet::new()),
            children: Mutex::new(BTreeMap::new()),
            spawns: Mutex::new(Vec::new()),
            kills: Mutex::new(Vec::new()),
            graceful_kills: Mutex::new(Vec::new()),
        })
    }

    /// A child that exits 0 without writing anything.
    pub fn silent() -> Arc<Self> {
        ScriptedProcessManager::new(Script::new())
    }

    /// Make these commands look absent from `PATH`, so the version probes can
    /// be tested without depending on which CLIs are installed.
    pub fn without(self: Arc<Self>, commands: &[&str]) -> Arc<Self> {
        let mut missing = self.missing.lock().expect("missing");
        for command in commands {
            missing.insert((*command).to_owned());
        }
        drop(missing);
        self
    }

    pub fn spawns(&self) -> Vec<SpawnRecord> {
        self.spawns.lock().expect("spawns").clone()
    }

    /// The single spawn a test performed.
    pub fn spawn_record(&self) -> SpawnRecord {
        let spawns = self.spawns();
        assert_eq!(spawns.len(), 1, "expected exactly one spawn");
        spawns.into_iter().next().expect("one spawn")
    }

    pub fn killed(&self) -> Vec<(u32, Signal)> {
        self.kills.lock().expect("kills").clone()
    }

    pub fn killed_with_grace(&self) -> Vec<(u32, Duration)> {
        self.graceful_kills.lock().expect("graceful kills").clone()
    }

    /// What the adapter wrote to the child's stdin, once the capture has seen
    /// it and stopped growing. `None` if nothing arrived within the timeout.
    pub async fn stdin_capture(&self) -> Option<String> {
        let path = self.dir.path().join("stdin");
        let mut previous = String::new();
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            if !contents.is_empty() && contents == previous {
                return Some(contents);
            }
            previous = contents;
        }
        None
    }

    fn build_script(&self) -> String {
        let dir = self.dir.path();
        let mut script = String::new();

        if self.script.capture_stdin {
            // Backgrounded so a long-lived session can keep writing while the
            // script emits stdout, and with its own stdio redirected so it
            // never holds the parent's pipes open past the script's exit.
            // `exec 3<&0` first: a POSIX shell redirects a background job's
            // stdin to /dev/null when job control is off, so the capture has
            // to read the real stdin through its own descriptor.
            script.push_str("exec 3<&0\n");
            script.push_str(&format!(
                "( cat <&3 >> '{sink}'; printf '%s' '{marker}' >> '{sink}' ) >/dev/null 2>&1 &\n",
                sink = dir.join("stdin").display(),
                marker = STDIN_EOF_MARKER,
            ));
        }

        for (index, step) in self.script.steps.iter().enumerate() {
            match step {
                Step::Stdout(bytes) => {
                    let path = dir.join(format!("out_{index}"));
                    std::fs::write(&path, bytes).expect("write stdout payload");
                    script.push_str(&format!("cat '{}'\n", path.display()));
                }
                Step::Stderr(bytes) => {
                    let path = dir.join(format!("err_{index}"));
                    std::fs::write(&path, bytes).expect("write stderr payload");
                    script.push_str(&format!("cat '{}' >&2\n", path.display()));
                }
                Step::SleepMs(ms) => {
                    script.push_str(&format!("sleep {}\n", *ms as f64 / 1000.0));
                }
            }
        }

        match self.script.exit {
            Exit::Code(code) => script.push_str(&format!("exit {code}\n")),
            Exit::Signal(signal) => script.push_str(&format!("kill -{signal} $$\n")),
            // Close stdout so the reader sees EOF while the process lives on,
            // exactly like a pi session that has finished streaming.
            Exit::Hang => script.push_str("exec 1>&- 2>&-\nwhile :; do sleep 1; done\n"),
        }
        script
    }
}

#[async_trait]
impl ProcessManager for ScriptedProcessManager {
    fn is_alive(&self, pid: u32) -> bool {
        let children = self.children.lock().expect("children");
        children.get(&pid).map_or_else(
            || self.inner.is_alive(pid),
            |child| child.exit.try_get().is_none(),
        )
    }

    fn kill(&self, pid: u32, signal: Signal) {
        self.kills.lock().expect("kills").push((pid, signal));
        if let Some(child) = self.children.lock().expect("children").get(&pid) {
            child.signal(signal);
        }
    }

    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        self.graceful_kills
            .lock()
            .expect("graceful kills")
            .push((pid, grace));
        let exit = {
            let children = self.children.lock().expect("children");
            children.get(&pid).map(|child| {
                child.signal(Signal::Term);
                child.exit.clone()
            })
        };
        if let Some(exit) = exit
            && tokio::time::timeout(grace, exit.wait()).await.is_err()
            && let Some(child) = self.children.lock().expect("children").get(&pid)
        {
            child.signal(Signal::Kill);
        }
    }

    fn spawn(
        &self,
        command: &str,
        args: &[String],
        opts: SpawnOptions,
    ) -> orch_core::Result<SpawnedChild> {
        self.spawns.lock().expect("spawns").push(SpawnRecord {
            command: command.to_owned(),
            args: args.to_vec(),
            cwd: opts.cwd.clone(),
            env: opts.env.clone(),
            stdin: opts.stdin,
        });

        if self.missing.lock().expect("missing").contains(command) {
            return Err(orch_core::error::Error::Io {
                context: format!("Failed to spawn process: {command}"),
                path: Some(PathBuf::from(command)),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            });
        }

        // A separate, unreaped group leader pins group identity while the
        // script's exit is delivered normally. Killing a recorded/reaped PID
        // would race PID reuse; an is_alive/getpgid check cannot close that race.
        // std Child ownership also makes final cleanup independent of Tokio.
        let mut anchor = Command::new("/bin/cat");
        anchor
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let io_error = |source| orch_core::error::Error::Io {
            context: "spawn scripted fixture".to_owned(),
            path: None,
            source,
        };
        let mut owned = OwnedScript {
            anchor: anchor.spawn().map_err(io_error)?,
            child: Arc::new(Mutex::new(None)),
            observer: None,
            exit: ExitSignal::resolved(ExitOutcome::default()),
        };
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &self.build_script()])
            .current_dir(self.dir.path())
            .envs(&opts.env)
            .process_group(i32::try_from(owned.anchor.id()).expect("positive OS pid"))
            .stdin(match opts.stdin {
                StdioMode::Ignore => Stdio::null(),
                StdioMode::Pipe => Stdio::piped(),
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if opts.inherit_env == EnvInheritance::WithoutSecrets {
            let passthrough = std::env::var_os(REMOTE_ENV_PASSTHROUGH);
            for name in secret_env_names(std::env::vars_os(), passthrough.as_deref()) {
                if !opts.env.contains_key(&name.to_string_lossy().into_owned()) {
                    command.env_remove(name);
                }
            }
        }
        let child = command.spawn().map_err(io_error)?;
        let pid = child.id();
        let mut slot = owned
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(child);
        let child = slot.as_mut().expect("just inserted child");
        let stdout = child
            .stdout
            .take()
            .map(tokio::process::ChildStdout::from_std)
            .transpose()
            .map_err(io_error)?;
        let stderr = child
            .stderr
            .take()
            .map(tokio::process::ChildStderr::from_std)
            .transpose()
            .map_err(io_error)?;
        let stdin = child
            .stdin
            .take()
            .map(tokio::process::ChildStdin::from_std)
            .transpose()
            .map_err(io_error)?;
        drop(slot);
        let (notifier, exit) = ExitSignal::channel();
        owned.exit = exit.clone();
        let slot = Arc::clone(&owned.child);
        owned.observer = Some(
            std::thread::Builder::new()
                .name("scripted-child-exit".to_owned())
                .spawn(move || {
                    let mut slot = slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let outcome = slot
                        .as_mut()
                        .expect("owned script")
                        .wait()
                        .map(|status| ExitOutcome {
                            code: status.code(),
                            signal: status.signal(),
                        })
                        .unwrap_or_default();
                    *slot = None;
                    notifier.notify(outcome);
                })
                .map_err(io_error)?,
        );
        self.children.lock().expect("children").insert(pid, owned);
        let child = SpawnedChild::from_tokio(pid, stdout, stderr, stdin, exit);
        Ok(child)
    }
}

/// Everything a run produced: the events in order, and the error that ended
/// the stream, if any.
#[derive(Debug)]
pub struct Collected {
    pub events: Vec<AgentEvent>,
    pub error: Option<AdapterError>,
}

impl Collected {
    pub fn kinds(&self) -> Vec<AgentEventKind> {
        self.events.iter().map(|event| event.kind).collect()
    }

    pub fn data(&self) -> Vec<&Value> {
        self.events.iter().map(|event| &event.data).collect()
    }

    pub fn first(&self) -> &AgentEvent {
        self.events.first().expect("at least one event")
    }

    pub fn last(&self) -> &AgentEvent {
        self.events.last().expect("at least one event")
    }

    pub fn of_kind(&self, kind: AgentEventKind) -> Vec<&AgentEvent> {
        self.events
            .iter()
            .filter(|event| event.kind == kind)
            .collect()
    }

    pub fn has_kind(&self, kind: AgentEventKind) -> bool {
        self.events.iter().any(|event| event.kind == kind)
    }

    /// The error message, failing the test if the run succeeded.
    pub fn error_message(&self) -> &str {
        &self
            .error
            .as_ref()
            .expect("the run should have failed")
            .message
    }

    pub fn expect_ok(&self) -> &Self {
        assert!(
            self.error.is_none(),
            "the run failed: {:?}",
            self.error.as_ref().map(|err| &err.message)
        );
        self
    }
}

/// Drain a handle to the end, under a timeout so a deadlock fails loudly.
pub async fn collect(handle: ExecuteHandle) -> Collected {
    let mut events = Vec::new();
    let mut error = None;
    let mut stream = handle.events;

    let drained = tokio::time::timeout(RUN_TIMEOUT, async {
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => events.push(event),
                Err(err) => {
                    error = Some(err);
                    // The generator yields the error once and ends.
                    break;
                }
            }
        }
    })
    .await;

    assert!(
        drained.is_ok(),
        "the event stream did not finish within {RUN_TIMEOUT:?} — \
         it is deadlocked (events so far: {})",
        events.len()
    );
    Collected { events, error }
}

/// `ExecuteParams` with test defaults.
pub fn params(prompt: &str, workspace: &str) -> ExecuteParams {
    ExecuteParams {
        prompt: prompt.to_owned(),
        system_prompt: None,
        workspace: PathBuf::from(workspace),
        env: BTreeMap::new(),
        inherit_env: Default::default(),
        config: AgentConfig::default(),
        cancel: CancellationToken::new(),
    }
}

pub fn with_config(mut params: ExecuteParams, config: AgentConfig) -> ExecuteParams {
    params.config = config;
    params
}

pub fn with_system_prompt(mut params: ExecuteParams, system_prompt: &str) -> ExecuteParams {
    params.system_prompt = Some(system_prompt.to_owned());
    params
}

/// A clock frozen at [`orch_core::test_support::FIXED_NOW_ISO`], so event
/// timestamps are exact.
pub fn fixed_clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::new())
}

/// 256 KiB of stderr — more than a pipe buffer, which is what deadlocks an
/// adapter that never reads it.
pub fn stderr_flood() -> Vec<u8> {
    let mut flood = vec![b'E'; 256 * 1024];
    // Line-delimited, so the shell adapter (which streams stderr) does not
    // have to buffer one 256 KiB line.
    for index in (0..flood.len()).step_by(128) {
        flood[index] = b'\n';
    }
    flood
}

/// `fixtures/adapters/<adapter>/` — the recorded stdout streams.
pub fn fixture_dir(adapter: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/adapters")
        .join(adapter)
}

/// Successful exit with a resolved outcome, for the handful of cases that do
/// not need a child at all.
pub fn exited(code: i32) -> ExitOutcome {
    ExitOutcome {
        code: Some(code),
        signal: None,
    }
}
