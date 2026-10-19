//! `orch doctor`.
//!
//! Three things here are easy to get wrong and all three are observable:
//!
//! - **the two header lines are printed before the JSON**, so `doctor --json`
//!   stdout is not pure JSON;
//! - the adapter count depends on the tier: **8** adapters with a project,
//!   **5** without (claude, shell, pi, grok, antigravity) — and the trailing
//!   `N of M adapters ready` line changes with it;
//! - the exit code is **always 0**, however many checks fail.
//!
//! The checks themselves are [`orch_engine::DoctorService`]; what lives here
//! is the rendering, the container-less adapter registry (the tier has no
//! container, but the service only needs a registry and a cwd) and the
//! camelCase JSON view — `adaptersReady`/`adaptersTotal` are the printed key
//! names, and the engine's own struct is snake_case like every other domain
//! type.

use std::process::ExitCode;
use std::sync::Arc;

use orch_core::adapter::AdapterRegistry;
use orch_core::error::Result;
use orch_core::process::ProcessManager;
use orch_engine::{CheckStatus, DoctorCheck, DoctorReport, DoctorService};
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, GrokAdapter, PiAdapter, ShellAdapter,
};
use orch_infra::process::SystemProcessManager;
use orch_infra::storage::Paths;
use serde::Serialize;

use crate::context::CliContext;
use crate::output::{Color, Icon, StructuredFormat, Styles};

/// The five adapters the container-less tier probes, in the order it
/// registers them — note that codex, cursor and opencode are absent.
pub fn container_less_registry(process_manager: Arc<dyn ProcessManager>) -> AdapterRegistry {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ClaudeAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(ShellAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(PiAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(GrokAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(AntigravityAdapter::new(process_manager)));
    registry
}

/// The printed shape of a report: camelCase here, unlike
/// every stored entity, and this is the only place that matters.
#[derive(Debug, Serialize)]
struct DoctorJson<'a> {
    checks: &'a [DoctorCheck],
    #[serde(rename = "adaptersReady")]
    adapters_ready: usize,
    #[serde(rename = "adaptersTotal")]
    adapters_total: usize,
}

impl<'a> From<&'a DoctorReport> for DoctorJson<'a> {
    fn from(report: &'a DoctorReport) -> Self {
        DoctorJson {
            checks: &report.checks,
            adapters_ready: report.adapters_ready,
            adapters_total: report.adapters_total,
        }
    }
}

/// How the project line is rendered, which depends on both the project state
/// and whether a container was built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectLine {
    /// `exists · N agents · M tasks` — initialized **and** a container.
    Initialized { agents: usize, tasks: usize },
    /// `not found — run: orch init`.
    Missing,
    /// Initialized but no container: the line is omitted entirely.
    Omitted,
}

/// The two header lines, printed before everything — including before the
/// JSON.
pub fn render_header(styles: &Styles) -> String {
    format!(
        "\n  {} · checking adapters and dependencies\n\n",
        styles.paint(Color::Amber, "orch doctor")
    )
}

/// The human body: one line per check, the project line, then the footer.
pub fn render_report(styles: &Styles, report: &DoctorReport, project: &ProjectLine) -> String {
    let mut out = String::new();
    for check in &report.checks {
        let icon = match check.status {
            CheckStatus::Ok => styles.paint(Color::Green, styles.icon(Icon::Done)),
            CheckStatus::Fail => styles.paint(Color::Red, styles.icon(Icon::Failed)),
            CheckStatus::Skip => styles.dim("—"),
        };
        let detail = match &check.detail {
            Some(detail) if !detail.is_empty() => styles.dim(&format!(" {detail}")),
            _ => String::new(),
        };
        // Two spaces after the icon, and the name padded to 12.
        out.push_str(&format!("  {icon}  {:<12}{detail}\n", check.name));
    }

    match project {
        ProjectLine::Initialized { agents, tasks } => {
            out.push('\n');
            out.push_str(&format!(
                "  {}  .orchestry/   {}\n",
                styles.paint(Color::Green, styles.icon(Icon::Done)),
                styles.dim(&format!("exists · {agents} agents · {tasks} tasks")),
            ));
        }
        ProjectLine::Missing => {
            out.push('\n');
            out.push_str(&format!(
                "  {}  .orchestry/   {}\n",
                styles.paint(Color::Red, styles.icon(Icon::Failed)),
                styles.dim("not found — run: orch init"),
            ));
        }
        ProjectLine::Omitted => {}
    }

    out.push('\n');
    out.push_str(&format!(
        "  {} of {} adapters ready\n\n",
        report.adapters_ready, report.adapters_total
    ));
    out
}

/// Run and render. **Always exits 0.**
pub async fn run(
    styles: &Styles,
    cwd: &std::path::Path,
    output: Option<StructuredFormat>,
    project: Option<&(CliContext, orch_engine::Engine)>,
) -> Result<ExitCode> {
    print!("{}", render_header(styles));

    let process_manager: Arc<dyn ProcessManager> = Arc::new(SystemProcessManager::new());
    let registry = match project {
        // With a project, all eight adapters in registration order.
        Some(_) => orch_infra::adapters::build_registry(Arc::clone(&process_manager)),
        None => container_less_registry(Arc::clone(&process_manager)),
    };

    let report = DoctorService::new(Arc::new(registry), cwd.to_path_buf())
        .run_all()
        .await;

    // Structured mode is gated on a container existing, so a container-less
    // `doctor --json` (or `--yaml`) still prints text.
    if let Some(format) = output
        && project.is_some()
    {
        format.print(&DoctorJson::from(&report))?;
        return Ok(ExitCode::SUCCESS);
    }

    let line = match project {
        Some((ctx, engine)) => {
            let paths = Paths::new(ctx.project_root.clone());
            if paths.is_initialized().await {
                ProjectLine::Initialized {
                    agents: engine.agent_service.list().await?.len(),
                    tasks: engine
                        .task_service
                        .list(&orch_core::storage::TaskFilter::default())
                        .await?
                        .len(),
                }
            } else {
                ProjectLine::Missing
            }
        }
        None => {
            if Paths::new(cwd.to_path_buf()).is_initialized().await {
                // Initialized but container-less: the line is omitted.
                ProjectLine::Omitted
            } else {
                ProjectLine::Missing
            }
        }
    };

    print!("{}", render_report(styles, &report, &line));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::FakeProcessManager;

    fn check(name: &str, status: CheckStatus, detail: Option<&str>) -> DoctorCheck {
        DoctorCheck {
            name: name.to_owned(),
            status,
            detail: detail.map(str::to_owned),
        }
    }

    fn report() -> DoctorReport {
        DoctorReport {
            checks: vec![
                check("claude", CheckStatus::Ok, Some("2.1.296 (Claude Code)")),
                check(
                    "pi",
                    CheckStatus::Fail,
                    Some("Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent"),
                ),
                check("skipped", CheckStatus::Skip, None),
            ],
            adapters_ready: 5,
            adapters_total: 8,
        }
    }

    #[test]
    fn the_header_is_two_lines_and_comes_first() {
        assert_eq!(
            render_header(&Styles::plain()),
            "\n  orch doctor · checking adapters and dependencies\n\n"
        );
    }

    #[test]
    fn a_check_line_pads_the_name_to_twelve_after_two_spaces() {
        let rendered = render_report(&Styles::plain(), &report(), &ProjectLine::Omitted);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "  ✓  claude       2.1.296 (Claude Code)");
        assert_eq!(
            lines[1],
            "  ✕  pi           Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent"
        );
        // `skip` renders as a dim dash and has no detail.
        assert_eq!(lines[2], "  —  skipped     ");
    }

    #[test]
    fn the_footer_reports_the_adapter_counts() {
        let rendered = render_report(&Styles::plain(), &report(), &ProjectLine::Omitted);
        assert!(rendered.ends_with("\n  5 of 8 adapters ready\n\n"));
    }

    #[test]
    fn the_project_line_has_three_forms() {
        let initialized = render_report(
            &Styles::plain(),
            &report(),
            &ProjectLine::Initialized {
                agents: 8,
                tasks: 0,
            },
        );
        assert!(initialized.contains("  ✓  .orchestry/   exists · 8 agents · 0 tasks\n"));

        let missing = render_report(&Styles::plain(), &report(), &ProjectLine::Missing);
        assert!(missing.contains("  ✕  .orchestry/   not found — run: orch init\n"));

        let omitted = render_report(&Styles::plain(), &report(), &ProjectLine::Omitted);
        assert!(!omitted.contains(".orchestry/"));
    }

    #[test]
    fn the_json_shape_is_stable() {
        let report = report();
        let json = serde_json::to_value(DoctorJson::from(&report)).expect("serialize");
        assert_eq!(json["adaptersReady"], 5);
        assert_eq!(json["adaptersTotal"], 8);
        assert_eq!(json["checks"][0]["name"], "claude");
        assert_eq!(json["checks"][0]["status"], "ok");
        assert_eq!(json["checks"][1]["status"], "fail");
        // A check with no detail omits the key.
        assert_eq!(json["checks"][2].get("detail"), None);
    }

    #[test]
    fn the_container_less_tier_probes_five_named_adapters() {
        let registry = container_less_registry(Arc::new(FakeProcessManager::new()));
        assert_eq!(
            registry.kinds(),
            ["claude", "shell", "pi", "grok", "antigravity"]
        );
    }

    #[test]
    fn the_full_tier_probes_eight_in_registration_order() {
        let registry = orch_infra::adapters::build_registry(Arc::new(FakeProcessManager::new()));
        assert_eq!(
            registry.kinds(),
            [
                "claude",
                "codex",
                "cursor",
                "shell",
                "opencode",
                "pi",
                "grok",
                "antigravity"
            ]
        );
    }

    #[tokio::test]
    async fn the_container_less_tier_reports_five_adapters_plus_four_checks() {
        // The service is the engine's, but which registry it gets is this
        // module's decision, and the row count the footer reports depends on
        // it. A fake process manager fails every version probe, which is the
        // interesting shape: the names and the ready count still have to be
        // right.
        let registry = container_less_registry(Arc::new(FakeProcessManager::new().with_exit(
            orch_core::process::ExitOutcome {
                code: Some(1),
                signal: None,
            },
        )));
        let report = DoctorService::new(Arc::new(registry), std::path::PathBuf::from("."))
            .run_all()
            .await;

        let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "claude",
                "shell",
                "pi",
                "grok",
                "antigravity",
                "git",
                "git repo",
                ".gitignore",
                // The `toolchain` row replaces the former `node` row, in the same position.
                "toolchain"
            ]
        );
        assert_eq!(report.adapters_total, 5);
        assert_eq!(report.adapters_ready, 0);
    }
}
