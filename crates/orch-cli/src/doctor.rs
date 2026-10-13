//! `orch doctor` — ported from `src/cli/commands/doctor.ts` and
//! `DoctorService.runAll` (`docs/parity/cli.md` §14).
//!
//! Three things here are easy to get wrong and all three are observable:
//!
//! - **the two header lines are printed before the JSON**, so `doctor --json`
//!   stdout is not pure JSON (§22.2);
//! - the adapter count depends on the tier: **8** adapters with a project,
//!   **5** without (claude, shell, pi, grok, antigravity) — and the trailing
//!   `N of M adapters ready` line changes with it;
//! - the exit code is **always 0**, however many checks fail.
//!
//! The report type and the checks live here rather than in `orch-engine`
//! because the container-less tier has no service to ask. When WP-4 lands
//! `DoctorService` it should produce this same [`DoctorReport`] and the
//! renderer below stays as it is.

use std::process::ExitCode;
use std::sync::Arc;

use orch_core::adapter::AdapterRegistry;
use orch_core::error::Result;
use orch_core::process::ProcessManager;
use orch_infra::adapters::{
    AntigravityAdapter, ClaudeAdapter, GrokAdapter, PiAdapter, ShellAdapter,
};
use orch_infra::process::SystemProcessManager;
use orch_infra::storage::Paths;
use serde::{Deserialize, Serialize};

use crate::context::CliContext;
use crate::output::{Color, Icon, Styles, print_json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Fail,
    /// In the type but never produced; the renderer maps it to `dim('—')`.
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl DoctorCheck {
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        DoctorCheck {
            name: name.into(),
            status: CheckStatus::Ok,
            detail: Some(detail.into()),
        }
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        DoctorCheck {
            name: name.into(),
            status: CheckStatus::Fail,
            detail: Some(detail.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    #[serde(rename = "adaptersReady")]
    pub adapters_ready: usize,
    #[serde(rename = "adaptersTotal")]
    pub adapters_total: usize,
}

/// The five adapters the container-less tier probes, in the order it
/// registers them (§14.1) — note that codex, cursor and opencode are absent.
pub fn container_less_registry(process_manager: Arc<dyn ProcessManager>) -> AdapterRegistry {
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ClaudeAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(ShellAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(PiAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(GrokAdapter::new(Arc::clone(&process_manager))));
    registry.register(Arc::new(AntigravityAdapter::new(process_manager)));
    registry
}

/// `DoctorService.runAll`: one check per adapter **sequentially**, then git,
/// the repo check, `.gitignore` and node.
pub async fn probe(registry: &AdapterRegistry, cwd: &std::path::Path) -> DoctorReport {
    let mut checks = Vec::new();
    let mut adapters_ready = 0;

    for adapter in registry.list() {
        let result = adapter.test().await;
        if result.ok {
            adapters_ready += 1;
            checks.push(DoctorCheck {
                name: adapter.kind().to_owned(),
                status: CheckStatus::Ok,
                detail: result.version,
            });
        } else {
            checks.push(DoctorCheck {
                name: adapter.kind().to_owned(),
                status: CheckStatus::Fail,
                detail: result.error,
            });
        }
    }
    let adapters_total = registry.len();

    checks.push(match command_version("git", &["--version"], cwd).await {
        Some(version) => DoctorCheck::ok("git", version),
        None => DoctorCheck::fail("git", "git: command not found"),
    });

    checks.push(
        match command_version("git", &["rev-parse", "--is-inside-work-tree"], cwd).await {
            Some(_) => DoctorCheck::ok("git repo", "git repository detected"),
            None => DoctorCheck::fail(
                "git repo",
                "not a git repository — worktree/isolated modes will fail. Run: git init",
            ),
        },
    );

    checks.push(gitignore_check(cwd).await);

    checks.push(match command_version("node", &["--version"], cwd).await {
        Some(version) => DoctorCheck::ok("node", version),
        None => DoctorCheck::fail("node", "node: command not found"),
    });

    DoctorReport {
        checks,
        adapters_ready,
        adapters_total,
    }
}

/// `checkCommand`: ok → trimmed stdout, failure → `None`.
async fn command_version(command: &str, args: &[&str], cwd: &std::path::Path) -> Option<String> {
    let output = tokio::process::Command::new(command)
        .args(args)
        .current_dir(cwd)
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// A line whose trimmed form is exactly `.orchestry` passes; a present file
/// without one and a missing file have **different** messages.
async fn gitignore_check(cwd: &std::path::Path) -> DoctorCheck {
    let path = cwd.join(".gitignore");
    match tokio::fs::read_to_string(&path).await {
        Ok(contents) => {
            if contents.lines().any(|line| line.trim() == ".orchestry") {
                DoctorCheck::ok(".gitignore", ".orchestry is excluded")
            } else {
                DoctorCheck::fail(
                    ".gitignore",
                    ".orchestry not in .gitignore — worktrees will copy state recursively. Run: orch init",
                )
            }
        }
        Err(_) => DoctorCheck::fail(
            ".gitignore",
            "no .gitignore found — .orchestry may be committed to git. Run: orch init",
        ),
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
    json: bool,
    project: Option<&(CliContext, orch_engine::Engine)>,
) -> Result<ExitCode> {
    print!("{}", render_header(styles));

    let process_manager: Arc<dyn ProcessManager> = Arc::new(SystemProcessManager::new());
    let registry = match project {
        // With a project, all eight adapters in registration order.
        Some(_) => orch_infra::adapters::build_registry(Arc::clone(&process_manager)),
        None => container_less_registry(Arc::clone(&process_manager)),
    };

    let report = probe(&registry, cwd).await;

    // JSON mode is gated on a container existing, so a container-less
    // `doctor --json` still prints text.
    if json && project.is_some() {
        print_json(&report)?;
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

    fn report() -> DoctorReport {
        DoctorReport {
            checks: vec![
                DoctorCheck::ok("claude", "2.1.296 (Claude Code)"),
                DoctorCheck::fail(
                    "pi",
                    "Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent",
                ),
                DoctorCheck {
                    name: "skipped".to_owned(),
                    status: CheckStatus::Skip,
                    detail: None,
                },
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
    fn the_json_shape_is_the_reference_one() {
        let json = serde_json::to_value(report()).expect("serialize");
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
    async fn the_probe_reports_one_check_per_adapter_plus_four() {
        // A fake process manager makes every version probe fail, which is the
        // interesting shape: the names and the ready count still have to be
        // right.
        let registry = container_less_registry(Arc::new(FakeProcessManager::new().with_exit(
            orch_core::process::ExitOutcome {
                code: Some(1),
                signal: None,
            },
        )));
        let report = probe(&registry, std::path::Path::new(".")).await;

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
                "node"
            ]
        );
        assert_eq!(report.adapters_total, 5);
        assert_eq!(report.adapters_ready, 0);
    }

    #[tokio::test]
    async fn the_gitignore_check_distinguishes_missing_from_incomplete() {
        let dir = tempfile::tempdir().expect("tempdir");

        let missing = gitignore_check(dir.path()).await;
        assert_eq!(missing.status, CheckStatus::Fail);
        assert_eq!(
            missing.detail.as_deref(),
            Some("no .gitignore found — .orchestry may be committed to git. Run: orch init")
        );

        tokio::fs::write(dir.path().join(".gitignore"), "node_modules\n")
            .await
            .expect("write");
        let incomplete = gitignore_check(dir.path()).await;
        assert_eq!(incomplete.status, CheckStatus::Fail);
        assert_eq!(
            incomplete.detail.as_deref(),
            Some(
                ".orchestry not in .gitignore — worktrees will copy state recursively. Run: orch init"
            )
        );

        tokio::fs::write(
            dir.path().join(".gitignore"),
            "node_modules\n  .orchestry  \n",
        )
        .await
        .expect("write");
        let excluded = gitignore_check(dir.path()).await;
        assert_eq!(excluded.status, CheckStatus::Ok);
        assert_eq!(excluded.detail.as_deref(), Some(".orchestry is excluded"));
    }

    #[tokio::test]
    async fn a_missing_command_is_a_failed_check_not_an_error() {
        assert_eq!(
            command_version(
                "orch-definitely-not-a-binary",
                &[],
                std::path::Path::new(".")
            )
            .await,
            None
        );
        // `git --version` exists in any environment this is developed in; the
        // check's shape is what matters.
        let git = command_version("git", &["--version"], std::path::Path::new(".")).await;
        assert!(git.is_none_or(|version| version.starts_with("git version")));
    }
}
