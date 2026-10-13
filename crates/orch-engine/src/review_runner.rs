//! Automatic review — ported from `src/application/review-runner.ts`.
//!
//! Runs a task's `review_criteria` as child processes and reports pass/fail.
//! The commands are **hard-coded to npm/npx** and are wrong for a non-Node
//! project; they are the documented meaning of the `review_criteria` values and
//! are asserted by tests, so they are replicated rather than fixed
//! (PARITY.md §14.5).
//!
//! Execution is staged — `typecheck`, `lint`, `test_pass`, fastest first,
//! regardless of the order the task lists them in — and fail-fast by default.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use orch_domain::{ReviewCriterion, ReviewResult};

/// Per-criterion timeout. The TS `timeout_ms` default.
pub const DEFAULT_CRITERION_TIMEOUT_MS: u64 = 120_000;

/// node's `execFile` `maxBuffer`. Exceeding it kills the child and surfaces as
/// a failure, which is what makes a runaway test suite a failed criterion
/// rather than an OOM.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// `output` is truncated to this before it is stored on the task
/// (PARITY.md §7.9).
pub const MAX_RESULT_OUTPUT_CHARS: usize = 2000;

/// Execution order: fastest checks first. An unknown criterion sorts last.
const CRITERION_ORDER: [ReviewCriterion; 3] = [
    ReviewCriterion::Typecheck,
    ReviewCriterion::Lint,
    ReviewCriterion::TestPass,
];

/// argv per criterion (PARITY.md §7.9, L0).
fn criterion_command(criterion: ReviewCriterion) -> (&'static str, &'static [&'static str]) {
    match criterion {
        ReviewCriterion::TestPass => ("npm", &["test"]),
        ReviewCriterion::Typecheck => ("npx", &["tsc", "--noEmit"]),
        ReviewCriterion::Lint => ("npm", &["run", "lint"]),
    }
}

/// What one criterion's child process produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutcome {
    /// node reports a non-zero exit, a timeout and a `maxBuffer` overrun all as
    /// an `error` argument, and `passed = !error`. One flag is therefore the
    /// whole of the observable contract.
    pub failed: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The child-process seam.
///
/// A trait rather than a direct `tokio::process::Command` call because the TS
/// suite mocks `execFile` wholesale, and because a unit test must not shell out
/// to `npm` (PARITY.md V31 forbids the harness spawning it at all).
#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(
        &self,
        command: &str,
        args: &[&str],
        cwd: &Path,
        timeout: Duration,
    ) -> CommandOutcome;
}

/// Delegation, so an `Arc<dyn CommandRunner>` can be handed to a
/// [`ReviewRunner`] directly — which is how the orchestrator injects one.
#[async_trait]
impl CommandRunner for std::sync::Arc<dyn CommandRunner> {
    async fn run(
        &self,
        command: &str,
        args: &[&str],
        cwd: &Path,
        timeout: Duration,
    ) -> CommandOutcome {
        (**self).run(command, args, cwd, timeout).await
    }
}

/// The real runner. Drains stdout and stderr **concurrently** — reading them
/// in sequence deadlocks on a full pipe (ARCHITECTURE §6.3).
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessCommandRunner;

#[async_trait]
impl CommandRunner for ProcessCommandRunner {
    async fn run(
        &self,
        command: &str,
        args: &[&str],
        cwd: &Path,
        timeout: Duration,
    ) -> CommandOutcome {
        let spawned = tokio::process::Command::new(command)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .output();

        match tokio::time::timeout(timeout, spawned).await {
            Ok(Ok(output)) => {
                let over_buffer = output.stdout.len() > MAX_OUTPUT_BYTES
                    || output.stderr.len() > MAX_OUTPUT_BYTES;
                CommandOutcome {
                    failed: !output.status.success() || over_buffer,
                    stdout: cap_bytes(&output.stdout),
                    stderr: cap_bytes(&output.stderr),
                }
            }
            // A missing binary is an `error` in node too, so it fails the
            // criterion rather than the run.
            Ok(Err(err)) => CommandOutcome {
                failed: true,
                stdout: String::new(),
                stderr: err.to_string(),
            },
            Err(_elapsed) => CommandOutcome {
                failed: true,
                stdout: String::new(),
                stderr: format!("timed out after {}ms", timeout.as_millis()),
            },
        }
    }
}

/// Lossy because a child's output is not guaranteed UTF-8 and the string is
/// only ever shown to a human.
fn cap_bytes(bytes: &[u8]) -> String {
    let end = bytes.len().min(MAX_OUTPUT_BYTES);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[derive(Debug, Clone)]
pub struct ReviewRunnerOptions {
    /// `task.workspace ?? projectRoot` — the orchestrator picks it.
    pub cwd: PathBuf,
    pub timeout_ms: u64,
    /// Stop at the first failed criterion. The TS default is `true`.
    pub fail_fast: bool,
}

impl ReviewRunnerOptions {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        ReviewRunnerOptions {
            cwd: cwd.into(),
            timeout_ms: DEFAULT_CRITERION_TIMEOUT_MS,
            fail_fast: true,
        }
    }
}

pub struct ReviewRunner<R: CommandRunner> {
    options: ReviewRunnerOptions,
    runner: R,
}

impl ReviewRunner<ProcessCommandRunner> {
    pub fn new(options: ReviewRunnerOptions) -> Self {
        ReviewRunner {
            options,
            runner: ProcessCommandRunner,
        }
    }
}

impl<R: CommandRunner> ReviewRunner<R> {
    pub fn with_runner(options: ReviewRunnerOptions, runner: R) -> Self {
        ReviewRunner { options, runner }
    }

    /// Run the criteria in staged order, stopping at the first failure when
    /// `fail_fast` is set.
    pub async fn run_all(&self, criteria: &[ReviewCriterion]) -> Vec<ReviewResult> {
        let mut results = Vec::with_capacity(criteria.len());
        for criterion in sort_criteria(criteria) {
            let result = self.run_criterion(criterion).await;
            let passed = result.passed;
            results.push(result);
            if self.options.fail_fast && !passed {
                break;
            }
        }
        results
    }

    async fn run_criterion(&self, criterion: ReviewCriterion) -> ReviewResult {
        let (command, args) = criterion_command(criterion);
        let outcome = self
            .runner
            .run(
                command,
                args,
                &self.options.cwd,
                Duration::from_millis(self.options.timeout_ms),
            )
            .await;

        // `(stdout + '\n' + stderr).trim().slice(0, 2000)` — the newline is
        // unconditional, so a criterion with only stderr still trims cleanly.
        let combined = format!("{}\n{}", outcome.stdout, outcome.stderr);
        ReviewResult {
            criterion,
            passed: !outcome.failed,
            output: truncate_chars(combined.trim(), MAX_RESULT_OUTPUT_CHARS),
        }
    }
}

/// `results.length > 0 && every(passed)` — an **empty** result set is not
/// "all passed", which is what stops a task with no runnable criteria from
/// auto-approving.
pub fn all_passed(results: &[ReviewResult]) -> bool {
    !results.is_empty() && results.iter().all(|r| r.passed)
}

/// `"<✓|✗> <criterion>: <PASSED|FAILED>\n  <output>"`, joined by a blank line.
pub fn format_report(results: &[ReviewResult]) -> String {
    results
        .iter()
        .map(|result| {
            let icon = if result.passed { '✓' } else { '✗' };
            let verdict = if result.passed { "PASSED" } else { "FAILED" };
            format!(
                "{icon} {}: {verdict}\n  {}",
                result.criterion, result.output
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Stable sort by [`CRITERION_ORDER`], unknown criteria last. Stable so that
/// two criteria with the same rank keep the caller's order — the `Infinity`
/// bucket in the TS comparator relies on it.
fn sort_criteria(criteria: &[ReviewCriterion]) -> Vec<ReviewCriterion> {
    let rank = |criterion: &ReviewCriterion| {
        CRITERION_ORDER
            .iter()
            .position(|c| c == criterion)
            .unwrap_or(usize::MAX)
    };
    let mut sorted = criteria.to_vec();
    sorted.sort_by_key(rank);
    sorted
}

/// `String.prototype.slice(0, n)` counts UTF-16 code units; this counts
/// `char`s. They differ only above the BMP, where the TS version can split a
/// surrogate pair and produce a lone surrogate — something Rust cannot
/// represent at all. Truncating on a `char` boundary is the nearest valid
/// behaviour and the review output is human-facing text.
fn truncate_chars(value: &str, max: usize) -> String {
    match value.char_indices().nth(max) {
        Some((byte_index, _)) => value[..byte_index].to_owned(),
        None => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::Recorder;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedRun {
        command: String,
        args: Vec<String>,
        cwd: PathBuf,
        timeout: Duration,
    }

    /// Replays a scripted list of outcomes — the Rust equivalent of the TS
    /// `simulateExecFile` queue.
    struct ScriptedRunner {
        script: Mutex<std::collections::VecDeque<CommandOutcome>>,
        calls: Recorder<RecordedRun>,
    }

    impl ScriptedRunner {
        fn new(outcomes: Vec<CommandOutcome>) -> Arc<Self> {
            Arc::new(ScriptedRunner {
                script: Mutex::new(outcomes.into()),
                calls: Recorder::new(),
            })
        }
    }

    #[async_trait]
    impl CommandRunner for Arc<ScriptedRunner> {
        async fn run(
            &self,
            command: &str,
            args: &[&str],
            cwd: &Path,
            timeout: Duration,
        ) -> CommandOutcome {
            self.calls.record(RecordedRun {
                command: command.to_owned(),
                args: args.iter().map(|a| (*a).to_owned()).collect(),
                cwd: cwd.to_path_buf(),
                timeout,
            });
            self.script
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
                .unwrap_or_default()
        }
    }

    fn ok(stdout: &str) -> CommandOutcome {
        CommandOutcome {
            failed: false,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    fn failed(stderr: &str) -> CommandOutcome {
        CommandOutcome {
            failed: true,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    fn runner(
        outcomes: Vec<CommandOutcome>,
        options: ReviewRunnerOptions,
    ) -> (ReviewRunner<Arc<ScriptedRunner>>, Arc<ScriptedRunner>) {
        let scripted = ScriptedRunner::new(outcomes);
        (
            ReviewRunner::with_runner(options, Arc::clone(&scripted)),
            scripted,
        )
    }

    fn result(criterion: ReviewCriterion, passed: bool, output: &str) -> ReviewResult {
        ReviewResult {
            criterion,
            passed,
            output: output.into(),
        }
    }

    // ── run_all ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn runs_all_criteria_and_returns_results_in_staged_order() {
        let (review, _calls) = runner(
            vec![ok("No errors found"), ok("All tests passed")],
            ReviewRunnerOptions::new("/tmp/test"),
        );

        let results = review
            .run_all(&[ReviewCriterion::TestPass, ReviewCriterion::Typecheck])
            .await;

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            result(ReviewCriterion::Typecheck, true, "No errors found")
        );
        assert_eq!(
            results[1],
            result(ReviewCriterion::TestPass, true, "All tests passed")
        );
    }

    #[tokio::test]
    async fn sorts_criteria_typecheck_then_lint_then_test_pass() {
        let (review, calls) = runner(
            vec![ok("tc ok"), ok("lint ok"), ok("test ok")],
            ReviewRunnerOptions::new("/tmp/test"),
        );

        let results = review
            .run_all(&[
                ReviewCriterion::TestPass,
                ReviewCriterion::Lint,
                ReviewCriterion::Typecheck,
            ])
            .await;

        assert_eq!(
            results.iter().map(|r| r.criterion).collect::<Vec<_>>(),
            vec![
                ReviewCriterion::Typecheck,
                ReviewCriterion::Lint,
                ReviewCriterion::TestPass
            ]
        );
        // And the argv matched the staged order.
        assert_eq!(
            calls
                .calls
                .calls()
                .into_iter()
                .map(|c| (c.command, c.args))
                .collect::<Vec<_>>(),
            vec![
                (
                    "npx".to_owned(),
                    vec!["tsc".to_owned(), "--noEmit".to_owned()]
                ),
                ("npm".to_owned(), vec!["run".to_owned(), "lint".to_owned()]),
                ("npm".to_owned(), vec!["test".to_owned()]),
            ]
        );
    }

    #[tokio::test]
    async fn stops_at_the_first_failure_in_fail_fast_mode() {
        let (review, calls) = runner(
            vec![failed("error TS2345: Argument of type")],
            ReviewRunnerOptions::new("/tmp/test"),
        );

        let results = review
            .run_all(&[
                ReviewCriterion::TestPass,
                ReviewCriterion::Typecheck,
                ReviewCriterion::Lint,
            ])
            .await;

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].criterion, ReviewCriterion::Typecheck);
        assert!(!results[0].passed);
        assert_eq!(calls.calls.len(), 1);
    }

    #[tokio::test]
    async fn runs_every_criterion_when_fail_fast_is_off() {
        let (review, _calls) = runner(
            vec![
                failed("type error"),
                failed("lint error"),
                failed("FAIL src/test.ts"),
            ],
            ReviewRunnerOptions {
                fail_fast: false,
                ..ReviewRunnerOptions::new("/tmp/test")
            },
        );

        let results = review
            .run_all(&[
                ReviewCriterion::TestPass,
                ReviewCriterion::Typecheck,
                ReviewCriterion::Lint,
            ])
            .await;

        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| !r.passed));
    }

    #[tokio::test]
    async fn marks_each_criterion_by_its_own_outcome() {
        let (review, _calls) = runner(
            vec![
                ok("No errors found"),
                failed("error TS2345: Argument of type"),
            ],
            ReviewRunnerOptions {
                fail_fast: false,
                ..ReviewRunnerOptions::new("/tmp/test")
            },
        );

        let results = review
            .run_all(&[ReviewCriterion::TestPass, ReviewCriterion::Typecheck])
            .await;

        assert!(results[0].passed);
        assert!(!results[1].passed);
        assert!(results[1].output.contains("TS2345"));
    }

    #[tokio::test]
    async fn passes_the_cwd_and_timeout_through_to_the_child() {
        let (review, calls) = runner(
            vec![ok("ok")],
            ReviewRunnerOptions {
                timeout_ms: 60_000,
                ..ReviewRunnerOptions::new("/my/project")
            },
        );

        review.run_all(&[ReviewCriterion::TestPass]).await;

        assert_eq!(
            calls.calls.first().unwrap(),
            RecordedRun {
                command: "npm".into(),
                args: vec!["test".into()],
                cwd: PathBuf::from("/my/project"),
                timeout: Duration::from_millis(60_000),
            }
        );
    }

    #[tokio::test]
    async fn truncates_the_output_to_2000_chars() {
        let long = "x".repeat(3000);
        let (review, _calls) = runner(vec![ok(&long)], ReviewRunnerOptions::new("/tmp/test"));

        let results = review.run_all(&[ReviewCriterion::TestPass]).await;
        assert_eq!(results[0].output.chars().count(), 2000);
    }

    #[tokio::test]
    async fn the_default_timeout_is_120_seconds_and_fail_fast_is_on() {
        let options = ReviewRunnerOptions::new("/tmp/test");
        assert_eq!(options.timeout_ms, 120_000);
        assert!(options.fail_fast);

        let (review, calls) = runner(vec![ok("ok")], options);
        review.run_all(&[ReviewCriterion::TestPass]).await;
        assert_eq!(
            calls.calls.first().unwrap().timeout,
            Duration::from_millis(120_000)
        );
    }

    #[tokio::test]
    async fn an_empty_criteria_list_runs_nothing() {
        let (review, calls) = runner(vec![], ReviewRunnerOptions::new("/tmp/test"));
        assert!(review.run_all(&[]).await.is_empty());
        assert!(calls.calls.is_empty());
    }

    // ── all_passed ───────────────────────────────────────────────────────

    #[test]
    fn all_passed_is_true_only_for_a_non_empty_all_passing_set() {
        assert!(all_passed(&[
            result(ReviewCriterion::TestPass, true, "ok"),
            result(ReviewCriterion::Typecheck, true, "ok"),
        ]));
        assert!(!all_passed(&[
            result(ReviewCriterion::TestPass, true, "ok"),
            result(ReviewCriterion::Typecheck, false, "error"),
        ]));
        assert!(!all_passed(&[]));
    }

    // ── format_report ────────────────────────────────────────────────────

    #[test]
    fn format_report_renders_a_passing_result() {
        let report = format_report(&[result(ReviewCriterion::TestPass, true, "42 tests passed")]);
        assert_eq!(report, "✓ test_pass: PASSED\n  42 tests passed");
    }

    #[test]
    fn format_report_renders_a_failing_result() {
        let report = format_report(&[result(ReviewCriterion::Lint, false, "3 errors found")]);
        assert_eq!(report, "✗ lint: FAILED\n  3 errors found");
    }

    #[test]
    fn format_report_joins_mixed_results_with_a_blank_line() {
        let report = format_report(&[
            result(ReviewCriterion::TestPass, true, "ok"),
            result(ReviewCriterion::Typecheck, false, "fail"),
        ]);
        assert_eq!(
            report,
            "✓ test_pass: PASSED\n  ok\n\n✗ typecheck: FAILED\n  fail"
        );
        assert_eq!(format_report(&[]), "");
    }

    // ── the real runner ──────────────────────────────────────────────────

    #[tokio::test]
    async fn the_process_runner_reports_a_missing_binary_as_a_failure() {
        let outcome = ProcessCommandRunner
            .run(
                "orch-definitely-not-a-real-binary",
                &[],
                Path::new("."),
                Duration::from_secs(5),
            )
            .await;
        assert!(outcome.failed);
        assert!(!outcome.stderr.is_empty());
    }

    #[tokio::test]
    async fn the_process_runner_captures_output_and_the_exit_status() {
        let outcome = ProcessCommandRunner
            .run(
                "sh",
                &["-c", "printf out; printf err >&2; exit 0"],
                Path::new("."),
                Duration::from_secs(5),
            )
            .await;
        assert!(!outcome.failed);
        assert_eq!(outcome.stdout, "out");
        assert_eq!(outcome.stderr, "err");

        let outcome = ProcessCommandRunner
            .run(
                "sh",
                &["-c", "exit 3"],
                Path::new("."),
                Duration::from_secs(5),
            )
            .await;
        assert!(outcome.failed);
    }

    #[tokio::test]
    async fn the_process_runner_fails_a_child_that_outruns_its_timeout() {
        let outcome = ProcessCommandRunner
            .run(
                "sh",
                &["-c", "sleep 30"],
                Path::new("."),
                Duration::from_millis(50),
            )
            .await;
        assert!(outcome.failed);
        assert!(outcome.stderr.contains("timed out"));
    }
}
