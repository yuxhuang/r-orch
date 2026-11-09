//! The two system tools: `status` and `doctor`.
//!
//! Both are daemon reads. `doctor`'s probes run inside the daemon, which
//! bounds each probe and the whole run; the bridge only adds its own lane and
//! whole-call deadline (see `server`).

use orch_core::error::Result;
use orch_proto::ops::args::DoctorRunArgs;
use orch_proto::ops::results::DoctorReport;
use serde_json::Value;

use super::projections;
use super::registry::{CallContext, Class, EmptyArgs, Execution, Registry, ToolMeta, ToolResult};

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

/// `orch doctor --json` without the CLI's header lines: `{checks,
/// adaptersReady, adaptersTotal}`.
pub(crate) fn doctor_json(report: &DoctorReport) -> Value {
    serde_json::json!({
        "checks": report.checks,
        "adaptersReady": report.adapters_ready,
        "adaptersTotal": report.adapters_total,
    })
}

async fn doctor(cx: std::sync::Arc<CallContext>, _args: EmptyArgs) -> ToolResult {
    Ok(doctor_json(&cx.request(DoctorRunArgs {}).await?))
}

pub(crate) fn register(registry: &mut Registry) -> Result<()> {
    registry.add::<EmptyArgs, _, _>(STATUS, |cx, _args| async move {
        projections::status(&cx).await
    })?;
    registry.add::<EmptyArgs, _, _>(DOCTOR, doctor)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::registry::Mode;
    use orch_proto::ops::results::{CheckStatus, DoctorCheck};

    fn listed(mode: Mode) -> Vec<rmcp::model::Tool> {
        let mut registry = Registry::new(mode);
        register(&mut registry).unwrap();
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

    #[test]
    fn the_doctor_report_keeps_the_cli_json_shape() {
        let report = DoctorReport {
            checks: vec![DoctorCheck {
                name: "git".into(),
                status: CheckStatus::Ok,
                detail: None,
            }],
            adapters_ready: 1,
            adapters_total: 8,
        };
        assert_eq!(
            serde_json::to_string(&doctor_json(&report)).unwrap(),
            r#"{"checks":[{"name":"git","status":"ok"}],"adaptersReady":1,"adaptersTotal":8}"#
        );
    }
}
