//! A `ProcessManager` for tests: records every spawn and either runs the real
//! command, plays back a scripted outcome through a real `sh`, or fails to spawn.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;

use async_trait::async_trait;
use orch_core::process::{
    ExitOutcome, ExitSignal, ProcessManager, Signal, SpawnOptions, SpawnedChild,
};
use orch_core::{Error, Result};
use std::time::Duration;

pub enum Behavior {
    Real,
    Script {
        stdout: &'static str,
        stderr: &'static str,
        code: i32,
    },
    SpawnError(&'static str),
}

pub type Handler = Box<dyn Fn(&str, &[String]) -> Behavior + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

pub struct TestProcessManager {
    handler: Handler,
    calls: Mutex<Vec<Call>>,
}

impl TestProcessManager {
    pub fn real() -> Self {
        Self::with(|_, _| Behavior::Real)
    }

    pub fn with(handler: impl Fn(&str, &[String]) -> Behavior + Send + Sync + 'static) -> Self {
        Self {
            handler: Box::new(handler),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// `command arg arg` lines, for compact assertions.
    pub fn lines(&self) -> Vec<String> {
        self.calls()
            .iter()
            .map(|c| format!("{} {}", c.command, c.args.join(" ")))
            .collect()
    }
}

#[async_trait]
impl ProcessManager for TestProcessManager {
    fn is_alive(&self, _pid: u32) -> bool {
        true
    }

    fn kill(&self, _pid: u32, _signal: Signal) {}

    async fn kill_with_grace(&self, _pid: u32, _grace: Duration) {}

    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild> {
        self.calls.lock().unwrap().push(Call {
            command: command.to_owned(),
            args: args.to_vec(),
            cwd: opts.cwd.clone(),
        });
        let scripted = matches!((self.handler)(command, args), Behavior::Script { .. });
        let mut cmd = match (self.handler)(command, args) {
            Behavior::SpawnError(message) => return Err(Error::other(message)),
            Behavior::Real => {
                let mut c = tokio::process::Command::new(command);
                c.args(args);
                c
            }
            Behavior::Script {
                stdout,
                stderr,
                code,
            } => {
                let mut c = tokio::process::Command::new("sh");
                c.args([
                    "-c",
                    "printf %s \"$0\"; printf %s \"$1\" >&2; exit $2",
                    stdout,
                    stderr,
                    &code.to_string(),
                ]);
                c
            }
        };
        if !scripted {
            cmd.current_dir(&opts.cwd);
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::other(e.to_string()))?;
        let (notifier, exit) = ExitSignal::channel();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pid = child.id().unwrap_or(0);
        tokio::spawn(async move {
            let code = child.wait().await.ok().and_then(|s| s.code());
            notifier.notify(ExitOutcome { code, signal: None });
        });
        Ok(SpawnedChild::from_tokio(pid, stdout, stderr, None, exit))
    }
}
