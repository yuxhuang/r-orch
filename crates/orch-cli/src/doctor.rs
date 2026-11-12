//! `orch doctor` renders daemon-owned checks when connected. Before owner
//! startup it reports only bounded discovery facts and preserves the central
//! client error. It never probes local executables or opens project stores.
//!
//! Connected structured output retains its two header lines and camelCase keys.

use orch_domain::{CheckStatus, DoctorCheck, DoctorReport};
use serde::Serialize;

use crate::output::{Color, Icon, Styles};

/// The printed shape of a report: camelCase here, unlike
/// every stored entity, and this is the only place that matters.
#[derive(Debug, Serialize)]
pub(crate) struct DoctorJson<'a> {
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

/// Inspect only the client discovery files, using the same bounded reader as
/// connection setup. A failed diagnostic must not replace the original error.
pub async fn discovery_only(styles: &Styles, cwd: &std::path::Path) {
    let cwd = cwd.to_path_buf();
    let facts = tokio::task::spawn_blocking(move || {
        match orch_client::ClientConfig::from_env(cwd).and_then(|config| config.project()) {
            Ok(project) => {
                let record = if project.read_record().is_ok() {
                    "available"
                } else {
                    "unavailable"
                };
                format!(
                    "  project root: {}\n  daemon discovery: {record}\n",
                    orch_proto::path::escape_controls(&project.root.to_string_lossy())
                )
            }
            Err(_) => "  project discovery: unavailable\n".to_owned(),
        }
    })
    .await
    .unwrap_or_else(|_| "  project discovery: unavailable\n".to_owned());
    println!(
        "\n  {} · discovery only (daemon unavailable)\n\n{facts}",
        styles.paint(Color::Amber, "orch doctor")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
