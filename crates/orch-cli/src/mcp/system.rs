//! The two system tools B1 owns: `status` and `doctor`.

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use orch_core::error::{Error, Result};
use orch_core::process::ProcessManager;
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

/// The production diagnostics: the eight-adapter registry an initialized
/// project gets, probed by the existing bounded runner (child stdin is null
/// and output is piped, so a probe never touches the MCP transport). The git
/// checks run in the project root.
pub(crate) fn production_doctor() -> DoctorRunner {
    Arc::new(|root| {
        Box::pin(async move {
            let processes: Arc<dyn ProcessManager> = Arc::new(SystemProcessManager::new());
            let adapters = orch_infra::adapters::build_registry(processes);
            let report = DoctorService::new(Arc::new(adapters), root).run_all().await;
            serde_json::to_value(crate::doctor::DoctorJson::from(&report))
                .map_err(|_| Error::other("serialize the doctor report"))
        })
    })
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
