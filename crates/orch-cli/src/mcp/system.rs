//! The two system tools B1 owns: `status` and `doctor`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use orch_core::error::{Error, Result};
use orch_core::process::{ProcessManager, Signal, SpawnOptions, SpawnedChild};
use orch_engine::DoctorService;
use orch_infra::process::SystemProcessManager;
use serde_json::Value;

use super::projections;
use super::registry::{Class, EmptyArgs, Execution, Registry, ToolMeta};

/// Runs the diagnostics for a project root and returns the `doctor --json`
/// report (without the CLI's header lines).
pub(crate) type DoctorRunner =
    Arc<dyn Fn(PathBuf) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

const STATUS: ToolMeta = ToolMeta {
    name: "status",
    description: "Read the project's tasks, local agents and orchestrator state (orch status).",
    class: Class::Read,
    execution: Execution::Store,
    destructive_hint: false,
    idempotent_hint: true,
    open_world_hint: false,
};

const DOCTOR: ToolMeta = ToolMeta {
    name: "doctor",
    description: "Run bounded adapter and dependency diagnostics (orch doctor). Executes each \
                  adapter's version probe and git checks with captured output and a 10 s limit \
                  per probe, 120 s for the whole call; failed checks are report data.",
    class: Class::Read,
    execution: Execution::Diagnostic,
    destructive_hint: false,
    idempotent_hint: true,
    open_world_hint: true,
};

/// How long one adapter `--version` probe may run before its process group
/// is killed and the check fails.
const PROBE_LIMIT: Duration = Duration::from_secs(10);

/// The production diagnostics: the eight-adapter registry an initialized
/// project gets, probed by the existing runner (child stdin is null and output
/// is piped, so a probe never touches the MCP transport). The git checks run
/// in the project root.
///
/// The adapters spawn their probes through [`BoundedProbes`], so a probe is
/// killed after [`PROBE_LIMIT`], and every probe still running is killed when
/// the call is dropped — by the whole-call deadline, a cancellation or the
/// runtime shutting down. No probe outlives the call.
pub(crate) fn production_doctor() -> DoctorRunner {
    Arc::new(|root| {
        Box::pin(async move {
            let probes = BoundedProbes::new(PROBE_LIMIT);
            let _kill_on_drop = probes.guard();
            let processes: Arc<dyn ProcessManager> = Arc::new(probes);
            let adapters = orch_infra::adapters::build_registry(processes);
            let report = DoctorService::new(Arc::new(adapters), root).run_all().await;
            serde_json::to_value(crate::doctor::DoctorJson::from(&report))
                .map_err(|_| Error::other("serialize the doctor report"))
        })
    })
}

/// The pids of the probes still running, each the leader of its own process
/// group.
type LiveProbes = Arc<Mutex<Vec<u32>>>;

/// [`SystemProcessManager`] with every spawned child bounded and owned by one
/// doctor call. The plain manager deliberately lets a child outlive the
/// caller; a diagnostic probe must not.
struct BoundedProbes {
    inner: SystemProcessManager,
    live: LiveProbes,
    limit: Duration,
}

impl BoundedProbes {
    fn new(limit: Duration) -> Self {
        BoundedProbes {
            inner: SystemProcessManager::new(),
            live: LiveProbes::default(),
            limit,
        }
    }

    /// Kills, when dropped, the process group of every probe still running.
    fn guard(&self) -> ProbeGuard {
        ProbeGuard {
            inner: self.inner,
            live: Arc::clone(&self.live),
        }
    }
}

/// Removes `pid` from the live set, reporting whether it was still there, so
/// the watchdog and the guard never both signal one probe.
fn untrack(live: &LiveProbes, pid: u32) -> bool {
    let mut live = live.lock().unwrap_or_else(PoisonError::into_inner);
    let before = live.len();
    live.retain(|p| *p != pid);
    live.len() != before
}

#[async_trait]
impl ProcessManager for BoundedProbes {
    fn is_alive(&self, pid: u32) -> bool {
        self.inner.is_alive(pid)
    }

    fn kill(&self, pid: u32, signal: Signal) {
        self.inner.kill(pid, signal);
    }

    async fn kill_with_grace(&self, pid: u32, grace: Duration) {
        self.inner.kill_with_grace(pid, grace).await;
    }

    fn spawn(&self, command: &str, args: &[String], opts: SpawnOptions) -> Result<SpawnedChild> {
        let child = self.inner.spawn(command, args, opts)?;
        let pid = child.pid;
        self.live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(pid);
        // The watchdog is dropped with the runtime; the guard covers that.
        let (inner, live, limit, exit) = (
            self.inner,
            Arc::clone(&self.live),
            self.limit,
            child.exit.clone(),
        );
        tokio::spawn(async move {
            let overran = tokio::time::timeout(limit, exit.wait()).await.is_err();
            if untrack(&live, pid) && overran {
                tracing::debug!(pid, "doctor probe overran its limit; killing it");
                inner.kill(pid, Signal::Kill);
            }
        });
        Ok(child)
    }
}

/// See [`BoundedProbes::guard`]. Dropping is synchronous, so there is no
/// grace period: a `--version` probe has nothing to flush.
struct ProbeGuard {
    inner: SystemProcessManager,
    live: LiveProbes,
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        let live = std::mem::take(&mut *self.live.lock().unwrap_or_else(PoisonError::into_inner));
        for pid in live {
            tracing::debug!(pid, "doctor call ended with its probe running; killing it");
            self.inner.kill(pid, Signal::Kill);
        }
    }
}

pub(crate) fn register(registry: &mut Registry, doctor: DoctorRunner) -> Result<()> {
    registry.add::<EmptyArgs, _, _>(STATUS, |cx, _args| async move {
        projections::status(&cx.engine).await
    })?;
    registry.add::<EmptyArgs, _, _>(DOCTOR, move |cx, _args| {
        let doctor = Arc::clone(&doctor);
        async move { doctor(cx.project_root.clone()).await }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::mode::Mode;

    fn listed(mode: Mode) -> Vec<rmcp::model::Tool> {
        let mut registry = Registry::new(mode);
        register(&mut registry, production_doctor()).unwrap();
        registry.into_router().list_all()
    }

    /// The probe's exit, or a panic if it is still running after `within`.
    async fn exited(child: &SpawnedChild, within: Duration) -> orch_core::process::ExitOutcome {
        tokio::time::timeout(within, child.exit.wait())
            .await
            .expect("the probe was not killed")
    }

    fn live(probes: &BoundedProbes) -> Vec<u32> {
        probes.live.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn a_probe_that_overruns_its_limit_is_killed() {
        let probes = BoundedProbes::new(Duration::from_millis(100));
        let child = probes
            .spawn("sleep", &["60".to_owned()], SpawnOptions::default())
            .unwrap();

        let outcome = exited(&child, Duration::from_secs(10)).await;

        assert_eq!(outcome.signal, Some(9), "{outcome:?}");
        assert!(live(&probes).is_empty());
    }

    #[tokio::test]
    async fn dropping_the_guard_kills_every_running_probe() {
        let probes = BoundedProbes::new(Duration::from_secs(600));
        let guard = probes.guard();
        let children: Vec<SpawnedChild> = (0..2)
            .map(|_| {
                probes
                    .spawn("sleep", &["60".to_owned()], SpawnOptions::default())
                    .unwrap()
            })
            .collect();

        drop(guard);

        for child in &children {
            let outcome = exited(child, Duration::from_secs(10)).await;
            assert_eq!(outcome.signal, Some(9), "{outcome:?}");
        }
        assert!(live(&probes).is_empty());
    }

    #[tokio::test]
    async fn a_probe_that_exits_is_forgotten_and_never_signalled() {
        let probes = BoundedProbes::new(Duration::from_secs(600));
        let child = probes.spawn("true", &[], SpawnOptions::default()).unwrap();

        let outcome = exited(&child, Duration::from_secs(10)).await;
        let forgotten = async {
            while !live(&probes).is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), forgotten)
            .await
            .expect("the exited probe stayed tracked");

        assert_eq!(outcome.code, Some(0), "{outcome:?}");
    }

    #[test]
    fn status_and_doctor_are_read_tools_in_every_mode() {
        for mode in [Mode::ReadOnly, Mode::Default, Mode::AllowDestructive] {
            let names: Vec<String> = listed(mode)
                .into_iter()
                .map(|t| t.name.into_owned())
                .collect();
            assert_eq!(names, ["doctor", "status"], "{mode:?}");
        }
        let tools = listed(Mode::Default);
        let hints = |tool: &rmcp::model::Tool| {
            let a = tool.annotations.clone().expect("annotations");
            (
                a.read_only_hint,
                a.destructive_hint,
                a.idempotent_hint,
                a.open_world_hint,
            )
        };
        assert_eq!(
            hints(&tools[0]),
            (Some(true), Some(false), Some(true), Some(true))
        );
        assert_eq!(
            hints(&tools[1]),
            (Some(true), Some(false), Some(true), Some(false))
        );
    }
}
