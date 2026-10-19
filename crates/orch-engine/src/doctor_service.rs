//! Diagnostics.
//!
//! Probes every registered adapter, then git, then whether the project is a git
//! repository, then whether `.orchestry` is git-ignored, then the toolchain.
//! The order is contract because the CLI prints the rows as
//! it receives them.
//!
//! `doctor` **reports, it does not gate**: the CLI exits 0 even when every
//! adapter is missing.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use orch_core::adapter::AdapterRegistry;

use crate::review_runner::{CommandRunner, ProcessCommandRunner};

/// `git --version` and friends are fast; a hang here would block `doctor`
/// indefinitely.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The toolchain row. Both halves (label and value) are compile-time
/// constants, so the row is deterministic.
const TOOLCHAIN_LABEL: &str = "toolchain";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Fail,
    Skip,
}

impl CheckStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Fail => "fail",
            CheckStatus::Skip => "skip",
        }
    }
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    pub adapters_ready: usize,
    pub adapters_total: usize,
}

pub struct DoctorService<R: CommandRunner = ProcessCommandRunner> {
    adapters: Arc<AdapterRegistry>,
    /// The directory the git checks run in — `projectRoot ?? process.cwd()`.
    cwd: PathBuf,
    runner: R,
}

impl DoctorService<ProcessCommandRunner> {
    pub fn new(adapters: Arc<AdapterRegistry>, cwd: impl Into<PathBuf>) -> Self {
        DoctorService {
            adapters,
            cwd: cwd.into(),
            runner: ProcessCommandRunner,
        }
    }
}

impl<R: CommandRunner> DoctorService<R> {
    pub fn with_runner(adapters: Arc<AdapterRegistry>, cwd: impl Into<PathBuf>, runner: R) -> Self {
        DoctorService {
            adapters,
            cwd: cwd.into(),
            runner,
        }
    }

    pub async fn run_all(&self) -> DoctorReport {
        let adapters = self.adapters.list();
        let adapters_total = adapters.len();
        let mut checks = Vec::with_capacity(adapters_total + 4);
        let mut adapters_ready = 0;

        // Sequential, not concurrent: the probe order is contract and each probe
        // spawns a child, so running eight at once would also be eight CLIs
        // competing for the terminal.
        for adapter in adapters {
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

        checks.push(self.check_command("git", &["--version"], "git").await);
        checks.push(self.check_git_repo().await);
        checks.push(self.check_gitignore().await);
        checks.push(toolchain_check());

        DoctorReport {
            checks,
            adapters_ready,
            adapters_total,
        }
    }

    async fn check_command(&self, command: &str, args: &[&str], name: &str) -> DoctorCheck {
        let outcome = self
            .runner
            .run(command, args, &self.cwd, PROBE_TIMEOUT)
            .await;
        if outcome.failed {
            DoctorCheck {
                name: name.to_owned(),
                status: CheckStatus::Fail,
                detail: Some(format!("{command}: command not found")),
            }
        } else {
            DoctorCheck {
                name: name.to_owned(),
                status: CheckStatus::Ok,
                detail: Some(outcome.stdout.trim().to_owned()),
            }
        }
    }

    async fn check_git_repo(&self) -> DoctorCheck {
        let outcome = self
            .runner
            .run(
                "git",
                &["rev-parse", "--is-inside-work-tree"],
                &self.cwd,
                PROBE_TIMEOUT,
            )
            .await;
        if outcome.failed {
            DoctorCheck {
                name: "git repo".to_owned(),
                status: CheckStatus::Fail,
                detail: Some(
                    "not a git repository — worktree/isolated modes will fail. Run: git init"
                        .to_owned(),
                ),
            }
        } else {
            DoctorCheck {
                name: "git repo".to_owned(),
                status: CheckStatus::Ok,
                detail: Some("git repository detected".to_owned()),
            }
        }
    }

    /// An un-ignored `.orchestry` makes a worktree copy project state into
    /// itself recursively, so this is a correctness check, not hygiene.
    async fn check_gitignore(&self) -> DoctorCheck {
        let path = self.cwd.join(".gitignore");
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => {
                let has_entry = content.lines().any(|line| line.trim() == ".orchestry");
                if has_entry {
                    DoctorCheck {
                        name: ".gitignore".to_owned(),
                        status: CheckStatus::Ok,
                        detail: Some(".orchestry is excluded".to_owned()),
                    }
                } else {
                    DoctorCheck {
                        name: ".gitignore".to_owned(),
                        status: CheckStatus::Fail,
                        detail: Some(
                            ".orchestry not in .gitignore — worktrees will copy state recursively. Run: orch init"
                                .to_owned(),
                        ),
                    }
                }
            }
            Err(_) => DoctorCheck {
                name: ".gitignore".to_owned(),
                status: CheckStatus::Fail,
                detail: Some(
                    "no .gitignore found — .orchestry may be committed to git. Run: orch init"
                        .to_owned(),
                ),
            },
        }
    }
}

fn toolchain_check() -> DoctorCheck {
    DoctorCheck {
        name: TOOLCHAIN_LABEL.to_owned(),
        status: CheckStatus::Ok,
        detail: Some(format!(
            "r-orch {} (rust {})",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_RUST_VERSION"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_runner::CommandOutcome;
    use async_trait::async_trait;
    use orch_core::adapter::{
        Adapter, AdapterError, AdapterTestResult, ExecuteHandle, ExecuteParams,
    };
    use orch_domain::AdapterErrorKind;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    struct StubAdapter {
        kind: &'static str,
        result: AdapterTestResult,
    }

    #[async_trait]
    impl Adapter for StubAdapter {
        fn kind(&self) -> &'static str {
            self.kind
        }

        async fn test(&self) -> AdapterTestResult {
            self.result.clone()
        }

        fn execute(
            &self,
            _params: ExecuteParams,
        ) -> std::result::Result<ExecuteHandle, AdapterError> {
            Err(AdapterError::new("not implemented", None))
        }

        async fn stop(&self, _pid: u32) -> orch_core::error::Result<()> {
            Ok(())
        }
    }

    /// Answers each `(command, first arg)` pair from a table; anything absent
    /// fails, which is what "binary not on PATH" looks like.
    #[derive(Default)]
    struct TableRunner {
        answers: HashMap<String, CommandOutcome>,
        calls: Mutex<Vec<String>>,
    }

    impl TableRunner {
        fn with(pairs: &[(&str, CommandOutcome)]) -> Self {
            TableRunner {
                answers: pairs
                    .iter()
                    .map(|(key, outcome)| ((*key).to_owned(), outcome.clone()))
                    .collect(),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl CommandRunner for TableRunner {
        async fn run(
            &self,
            command: &str,
            args: &[&str],
            _cwd: &Path,
            _timeout: Duration,
        ) -> CommandOutcome {
            let key = format!("{command} {}", args.join(" "));
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(key.clone());
            self.answers.get(&key).cloned().unwrap_or(CommandOutcome {
                failed: true,
                ..Default::default()
            })
        }
    }

    fn ok(stdout: &str) -> CommandOutcome {
        CommandOutcome {
            failed: false,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    fn registry(adapters: &[(&'static str, AdapterTestResult)]) -> Arc<AdapterRegistry> {
        let mut registry = AdapterRegistry::new();
        for (kind, result) in adapters {
            registry.register(Arc::new(StubAdapter {
                kind,
                result: result.clone(),
            }));
        }
        Arc::new(registry)
    }

    fn names(report: &DoctorReport) -> Vec<&str> {
        report.checks.iter().map(|c| c.name.as_str()).collect()
    }

    #[tokio::test]
    async fn checks_come_back_in_the_contract_order() {
        let temp = tempfile::tempdir().unwrap();
        let service = DoctorService::with_runner(
            registry(&[
                ("claude", AdapterTestResult::ok("2.1.296")),
                ("shell", AdapterTestResult::ok("GNU bash, version 5.2.37")),
            ]),
            temp.path(),
            TableRunner::with(&[
                ("git --version", ok("git version 2.47.3")),
                ("git rev-parse --is-inside-work-tree", ok("true")),
            ]),
        );

        let report = service.run_all().await;
        assert_eq!(
            names(&report),
            vec![
                "claude",
                "shell",
                "git",
                "git repo",
                ".gitignore",
                "toolchain"
            ]
        );
        assert_eq!(report.adapters_ready, 2);
        assert_eq!(report.adapters_total, 2);
    }

    #[tokio::test]
    async fn a_failing_adapter_reports_its_own_message_and_does_not_count_as_ready() {
        let temp = tempfile::tempdir().unwrap();
        let service = DoctorService::with_runner(
            registry(&[
                ("claude", AdapterTestResult::ok("2.1.296")),
                (
                    "pi",
                    AdapterTestResult::failed(
                        "Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent",
                        AdapterErrorKind::AdapterNotFound,
                    ),
                ),
            ]),
            temp.path(),
            TableRunner::default(),
        );

        let report = service.run_all().await;
        assert_eq!(report.adapters_ready, 1);
        assert_eq!(report.adapters_total, 2);
        let pi = &report.checks[1];
        assert_eq!(pi.status, CheckStatus::Fail);
        assert_eq!(
            pi.detail.as_deref(),
            Some("Pi CLI not found. Install: npm i -g @mariozechner/pi-coding-agent")
        );
    }

    #[tokio::test]
    async fn a_missing_git_reports_command_not_found_and_a_failed_repo_check() {
        let temp = tempfile::tempdir().unwrap();
        let service =
            DoctorService::with_runner(registry(&[]), temp.path(), TableRunner::default());

        let report = service.run_all().await;
        let git = &report.checks[0];
        assert_eq!(git.status, CheckStatus::Fail);
        assert_eq!(git.detail.as_deref(), Some("git: command not found"));

        let repo = &report.checks[1];
        assert_eq!(repo.status, CheckStatus::Fail);
        assert_eq!(
            repo.detail.as_deref(),
            Some("not a git repository — worktree/isolated modes will fail. Run: git init")
        );
    }

    #[tokio::test]
    async fn the_gitignore_check_wants_an_exact_orchestry_line() {
        let temp = tempfile::tempdir().unwrap();
        let service =
            DoctorService::with_runner(registry(&[]), temp.path(), TableRunner::default());

        // No file at all.
        let report = service.run_all().await;
        let check = report
            .checks
            .iter()
            .find(|c| c.name == ".gitignore")
            .unwrap();
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(
            check.detail.as_deref(),
            Some("no .gitignore found — .orchestry may be committed to git. Run: orch init")
        );

        // Present but without the entry — a near-miss must not pass.
        tokio::fs::write(
            temp.path().join(".gitignore"),
            "node_modules\n.orchestry/\n",
        )
        .await
        .unwrap();
        let report = service.run_all().await;
        let check = report
            .checks
            .iter()
            .find(|c| c.name == ".gitignore")
            .unwrap();
        assert_eq!(check.status, CheckStatus::Fail);

        // Exact line, with surrounding whitespace tolerated.
        tokio::fs::write(
            temp.path().join(".gitignore"),
            "node_modules\n  .orchestry  \n",
        )
        .await
        .unwrap();
        let report = service.run_all().await;
        let check = report
            .checks
            .iter()
            .find(|c| c.name == ".gitignore")
            .unwrap();
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail.as_deref(), Some(".orchestry is excluded"));
    }

    #[tokio::test]
    async fn the_toolchain_row_replaces_the_node_row_in_the_same_position() {
        let temp = tempfile::tempdir().unwrap();
        let service =
            DoctorService::with_runner(registry(&[]), temp.path(), TableRunner::default());

        let report = service.run_all().await;
        let last = report.checks.last().unwrap();
        assert_eq!(last.name, "toolchain");
        assert_eq!(last.status, CheckStatus::Ok);
        assert!(last.detail.as_deref().unwrap().starts_with("r-orch "));
    }

    #[tokio::test]
    async fn an_empty_registry_yields_the_four_environment_checks_only() {
        let temp = tempfile::tempdir().unwrap();
        let service =
            DoctorService::with_runner(registry(&[]), temp.path(), TableRunner::default());

        let report = service.run_all().await;
        assert_eq!(
            names(&report),
            vec!["git", "git repo", ".gitignore", "toolchain"]
        );
        assert_eq!(report.adapters_ready, 0);
        assert_eq!(report.adapters_total, 0);
    }
}
