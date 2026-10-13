//! The differential harness: run a scenario against both binaries and compare
//! stdout, stderr, exit codes and the whole `.orchestry/` tree.
//!
//! Hermeticity (PARITY.md §15 V31) is not optional here. The TypeScript binary
//! writes `~/.orchestry/update-check.json` from almost every command and
//! spawns `npm view` on a cold cache, so every scenario gets
//!
//! - its own `HOME`/`USERPROFILE`, pointed inside the tmpdir, and
//! - a **pre-seeded, fresh** `update-check.json` whose `latest` equals the
//!   oracle's own version, so the cache is warm, no registry query happens and
//!   no update notification is printed.
//!
//! The tmpdir must also have no `.orchestry` ancestor, because
//! `findProjectRoot` walks up and would otherwise find the developer's own
//! project. That is asserted, not assumed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::normalize::{IdRegistry, Normalizer};
use crate::scenario::{Kind, Runner, Scenario};

/// The version the oracle reports, so the seeded cache never looks like an
/// available update.
const ORACLE_VERSION: &str = "1.0.34";

#[derive(Debug)]
pub struct Binaries {
    /// A private snapshot of this tree's `orch`, not `target/debug/orch`
    /// itself — see [`Binaries::discover`].
    pub rust: PathBuf,
    /// `parity/orch-ts/dist/cli.js`.
    pub oracle_entry: PathBuf,
    pub node: PathBuf,
    pub repo_root: PathBuf,
    /// Holds the snapshot directory open for the lifetime of the run.
    _snapshot: Option<tempfile::TempDir>,
}

impl Binaries {
    /// Build this tree's CLI and take a private copy of it.
    ///
    /// Both halves matter, because the parallel work packages run in separate
    /// git worktrees that **share** one `CARGO_TARGET_DIR`. So
    /// `target/debug/orch` is not this tree's binary — it is whichever tree
    /// built last, and a parity run that reads it can silently grade a
    /// different branch. (Observed: a run reported 11 scenarios failing with
    /// rich diffs because it was testing a worktree that had the full command
    /// surface, while this tree had three commands.) Building first makes the
    /// binary ours; snapshotting it means a rebuild elsewhere cannot swap it
    /// out from under a run that takes minutes.
    pub fn discover(repo_root: &Path) -> Result<Binaries> {
        let built = build_cli(repo_root)?;

        let snapshot = tempfile::Builder::new()
            .prefix("r-orch-parity-bin-")
            .tempdir()
            .context("create binary snapshot dir")?;
        let rust = snapshot.path().join("orch");
        std::fs::copy(&built, &rust)
            .with_context(|| format!("snapshot {} for this run", built.display()))?;

        let oracle_entry = repo_root.join("parity/orch-ts/dist/cli.js");
        if !oracle_entry.exists() {
            bail!(
                "{} is missing — run `parity/setup.sh` to build the vendored oracle",
                oracle_entry.display()
            );
        }
        Ok(Binaries {
            rust,
            oracle_entry,
            node: PathBuf::from("node"),
            repo_root: repo_root.to_path_buf(),
            _snapshot: Some(snapshot),
        })
    }
}

/// `cargo build -p orch-cli` against *this* tree's manifest, returning the
/// executable cargo reports.
fn build_cli(repo_root: &Path) -> Result<PathBuf> {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args([
            "build",
            "-p",
            "orch-cli",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(repo_root.join("Cargo.toml"))
        .current_dir(repo_root)
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("run cargo build -p orch-cli")?;

    if !output.status.success() {
        bail!("cargo build -p orch-cli failed — fix the build before running the harness");
    }

    // The last `compiler-artifact` message carrying an executable is the bin.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let executable = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter_map(|message| message["executable"].as_str().map(PathBuf::from))
        .next_back();

    executable.context(
        "cargo reported no executable for orch-cli — the harness needs the binary path from \
         cargo rather than a guessed target/debug/orch, because the target directory is shared \
         between worktrees",
    )
}

/// One executed step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepOutput {
    pub step: String,
    pub runner: &'static str,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// A prepared, hermetic project directory.
struct Project {
    root: PathBuf,
    home: PathBuf,
    /// Kept so the directory outlives the run.
    _temp: Option<tempfile::TempDir>,
}

impl Project {
    /// Create a tmpdir, `git init` it with exactly one commit, and point HOME
    /// inside it with a warm update-check cache.
    fn create(label: &str) -> Result<Project> {
        let temp = tempfile::Builder::new()
            .prefix(&format!("r-orch-parity-{label}-"))
            .tempdir()
            .context("create scenario tmpdir")?;
        let root = temp.path().to_path_buf();
        assert_no_orchestry_ancestor(&root)?;

        let home = root.join(".harness-home");
        std::fs::create_dir_all(home.join(".orchestry")).context("create harness HOME")?;
        seed_update_cache(&home)?;

        // Both trees are git-initialized identically: worktree mode needs a
        // repo, and `init` itself runs `git init` plus a first commit.
        git(&root, &["init", "--initial-branch=main"])?;
        git(&root, &["config", "user.email", "harness@r-orch.test"])?;
        git(&root, &["config", "user.name", "Parity Harness"])?;
        git(&root, &["commit", "--allow-empty", "-m", "Initial commit"])?;

        Ok(Project {
            root,
            home,
            _temp: Some(temp),
        })
    }
}

fn assert_no_orchestry_ancestor(dir: &Path) -> Result<()> {
    let mut current = Some(dir);
    while let Some(path) = current {
        if path != dir && path.join(".orchestry").exists() {
            bail!(
                "{} has an .orchestry ancestor at {} — findProjectRoot would escape the \
                 scenario. Point TMPDIR somewhere without one.",
                dir.display(),
                path.display()
            );
        }
        current = path.parent();
    }
    Ok(())
}

/// A warm cache: fresh `checked_at`, and `latest` equal to the oracle's own
/// version so `updateAvailable` is false.
fn seed_update_cache(home: &Path) -> Result<()> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let cache = serde_json::json!({ "latest": ORACLE_VERSION, "checked_at": now_ms });
    std::fs::write(
        home.join(".orchestry/update-check.json"),
        serde_json::to_string(&cache)?,
    )
    .context("seed update-check.json")
}

fn git(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("spawn git {args:?}"))?;
    if !output.status.success() {
        bail!(
            "git {args:?} failed in {}: {}",
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Which subcommands the Rust binary actually has.
///
/// Probed with `<cmd> --help`, which clap answers during parsing: exit 0 for a
/// command it knows, non-zero (`unrecognized subcommand`) for one it does not.
/// That happens before the container is built, so an uninitialized directory
/// does not confuse it.
#[derive(Debug, Default)]
pub struct Capabilities {
    probed: std::cell::RefCell<BTreeMap<String, bool>>,
}

impl Capabilities {
    pub fn new() -> Self {
        Capabilities::default()
    }

    /// Every requirement this scenario declares that the Rust binary lacks.
    pub fn missing(&self, bins: &Binaries, requires: &[String]) -> Vec<String> {
        requires
            .iter()
            .filter(|command| !self.supports(bins, command))
            .cloned()
            .collect()
    }

    fn supports(&self, bins: &Binaries, command: &str) -> bool {
        if let Some(known) = self.probed.borrow().get(command) {
            return *known;
        }
        let argv = shell_words::split(command).unwrap_or_default();
        let supported = Command::new(&bins.rust)
            .args(&argv)
            .arg("--help")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        self.probed
            .borrow_mut()
            .insert(command.to_owned(), supported);
        supported
    }
}

/// The outcome of comparing one scenario.
#[derive(Debug)]
pub struct ScenarioResult {
    pub name: String,
    pub kind: Kind,
    pub failures: Vec<String>,
    /// Real id → placeholder, in the order the ids were first seen. Printed
    /// under `--verbose`, and the thing to read first when a diff looks
    /// nonsensical: a wrong mapping means two entities were matched up wrongly.
    pub id_mapping: Vec<(String, String)>,
    /// Set when the scenario did not run, with the reason. A skipped scenario
    /// is **not** a pass: it is counted and reported separately, so the corpus
    /// cannot quietly stop covering anything.
    pub skipped: Option<String>,
}

impl ScenarioResult {
    pub fn passed(&self) -> bool {
        self.skipped.is_none() && self.failures.is_empty()
    }

    pub fn ran(&self) -> bool {
        self.skipped.is_none()
    }

    fn skipped(scenario: &Scenario, reason: String) -> ScenarioResult {
        ScenarioResult {
            name: scenario.name.clone(),
            kind: scenario.kind,
            failures: Vec::new(),
            id_mapping: Vec::new(),
            skipped: Some(reason),
        }
    }

    /// `rust_then_ts` over 8 steps, say — the one-line shape of what ran.
    pub fn headline(&self) -> String {
        format!("{} [{}]", self.name, kind_label(self.kind))
    }
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Differential => "differential",
        Kind::TsThenRust => "ts→rust",
        Kind::RustThenTs => "rust→ts",
        Kind::Interleaved => "interleaved",
    }
}

pub fn run_scenario(
    bins: &Binaries,
    capabilities: &Capabilities,
    scenario: &Scenario,
) -> Result<ScenarioResult> {
    let missing = capabilities.missing(bins, &scenario.requires);
    if !missing.is_empty() {
        return Ok(ScenarioResult::skipped(
            scenario,
            format!("the Rust binary has no {}", missing.join(", ")),
        ));
    }

    let mut failures = Vec::new();

    if scenario.kind.is_cross_compat() {
        // One shared directory: the two binaries take turns in it. There is
        // nothing to compare *between* trees, so what is asserted is that
        // every step succeeds and that the artifacts stay readable by both —
        // which the js-yaml cross-parse below does.
        let project = Project::create(&scenario.name)?;
        let plan = scenario.plan();
        let mut outputs = Vec::new();
        let mut ids = IdRegistry::new();

        for (step, runner) in scenario.steps.iter().zip(&plan) {
            let output = execute(bins, &project, scenario, step, *runner, &ids)?;
            ids.learn(&output.stdout);
            if output.runner == "ts" {
                failures.extend(usage_error_failure(scenario, step, &output.stderr));
            }
            if output.exit_code != 0 {
                failures.push(format!(
                    "step `{step}` ({}) exited {} — cross-compat steps must all succeed\n\
                     stdout: {}\nstderr: {}",
                    output.runner,
                    output.exit_code,
                    output.stdout.trim(),
                    output.stderr.trim()
                ));
            }
            outputs.push(output);
        }

        // Both binaries now read the *same* tree, so a read-only step must
        // produce identical output — ids and timestamps included, since there
        // is only one set of them. This is what catches a second binary that
        // parses an artifact successfully but reads a different value out of
        // it.
        let mut norm = Normalizer::new().with_root(&project.root, "<ROOT>");
        for step in &scenario.verify {
            let rust_out = execute(bins, &project, scenario, step, Runner::Rust, &ids)?;
            let ts_out = execute(bins, &project, scenario, step, Runner::Ts, &ids)?;

            if rust_out.exit_code != ts_out.exit_code {
                failures.push(format!(
                    "verify `{step}`: exit code {} (rust) vs {} (ts) over the shared tree",
                    rust_out.exit_code, ts_out.exit_code
                ));
            }
            for (channel, rust_text, ts_text) in [
                ("stdout", &rust_out.stdout, &ts_out.stdout),
                ("stderr", &rust_out.stderr, &ts_out.stderr),
            ] {
                let left = norm.text(rust_text);
                let right = norm.text(ts_text);
                if left != right {
                    failures.push(format!(
                        "verify `{step}`: {channel} differs over the shared tree — one binary \
                         misreads what the other wrote\n{}",
                        unified_diff(&right, &left)
                    ));
                }
            }
        }

        failures.extend(cross_parse_failures(bins, &project)?);
        return Ok(ScenarioResult {
            name: scenario.name.clone(),
            kind: scenario.kind,
            failures,
            id_mapping: mapping_of(&norm),
            skipped: None,
        });
    }
    {
        let rust = Project::create(&format!("{}-rust", scenario.name))?;
        let ts = Project::create(&format!("{}-ts", scenario.name))?;

        let mut rust_norm = Normalizer::new().with_root(&rust.root, "<ROOT>");
        let mut ts_norm = Normalizer::new().with_root(&ts.root, "<ROOT>");
        let (mut rust_ids, mut ts_ids) = (IdRegistry::new(), IdRegistry::new());

        for step in &scenario.steps {
            let rust_out = execute(bins, &rust, scenario, step, Runner::Rust, &rust_ids)?;
            let ts_out = execute(bins, &ts, scenario, step, Runner::Ts, &ts_ids)?;

            // Learn ids in step order, so the Nth created entity maps to the
            // same placeholder on both sides — and so a later step can refer
            // to it by position.
            rust_norm.learn(&rust_out.stdout);
            rust_norm.learn(&rust_out.stderr);
            ts_norm.learn(&ts_out.stdout);
            ts_norm.learn(&ts_out.stderr);
            rust_ids.learn(&rust_out.stdout);
            ts_ids.learn(&ts_out.stdout);

            failures.extend(usage_error_failure(scenario, step, &ts_out.stderr));

            if rust_out.exit_code != ts_out.exit_code {
                failures.push(format!(
                    "step `{step}`: exit code {} (rust) vs {} (ts)",
                    rust_out.exit_code, ts_out.exit_code
                ));
            }
            // `@exit-only` steps have already had the only L0 thing about them
            // compared, just above.
            if Scenario::exit_only(step) {
                continue;
            }
            // `serve --log-format json` emits records, not prose: the record
            // *fields* are L0 and the text format is L2 (PARITY.md §11.1), so
            // compare those parsed and normalized, and everything else as text.
            let stdout_diff = if scenario.stdout_is_json_records(step) {
                let left = json_records(&rust_out.stdout, &mut rust_norm);
                let right = json_records(&ts_out.stdout, &mut ts_norm);
                (left != right).then(|| {
                    (
                        "log records",
                        unified_diff(&records_text(&right), &records_text(&left)),
                    )
                })
            } else {
                let left = rust_norm.text(&rust_out.stdout);
                let right = ts_norm.text(&ts_out.stdout);
                (left != right).then(|| ("stdout", unified_diff(&right, &left)))
            };
            if let Some((what, diff)) = stdout_diff {
                failures.push(format!("step `{step}`: {what} differ(s)\n{diff}"));
            }

            let left = rust_norm.text(&rust_out.stderr);
            let right = ts_norm.text(&ts_out.stderr);
            if left != right {
                failures.push(format!(
                    "step `{step}`: stderr differs\n{}",
                    unified_diff(&right, &left)
                ));
            }
        }

        let rust_tree = collect_tree(&rust.root, &mut rust_norm)?;
        let ts_tree = collect_tree(&ts.root, &mut ts_norm)?;
        failures.extend(compare_trees(&rust_tree, &ts_tree));

        failures.extend(cross_parse_failures(bins, &rust)?);

        Ok(ScenarioResult {
            name: scenario.name.clone(),
            kind: scenario.kind,
            failures,
            id_mapping: mapping_of(&rust_norm),
            skipped: None,
        })
    }
}

/// Real id → placeholder, in first-seen order.
/// Replay a scenario's steps through the oracle alone and report anything that
/// makes the scenario itself wrong rather than the implementation.
///
/// Three classes of mistake, all of which would otherwise sit undetected in a
/// skipped scenario until the command it needs lands:
///
/// 1. a misspelled flag or subcommand, which the oracle rejects at parse time;
/// 2. a `${task.N}` whose index the earlier steps never produced;
/// 3. a `@exit-only` marker on a step that does *not* produce a usage error,
///    which silently discards a real output comparison.
///
/// Steps are allowed to *fail* — plenty of them assert error paths — so a
/// non-zero exit is not itself a problem. Only the oracle is consulted, so the
/// Rust side's gaps do not enter into it.
pub fn lint_against_oracle(bins: &Binaries, scenario: &Scenario) -> Result<Vec<String>> {
    let project = Project::create(&format!("lint-{}", scenario.name))?;
    let mut problems = Vec::new();
    let mut ids = IdRegistry::new();

    for step in scenario.steps.iter().chain(&scenario.verify) {
        // A `@kill-after` step is meant to be interrupted; replaying it in
        // full would just burn the timeout, and the kill itself is what the
        // scenario asserts.
        if Scenario::kill_after(step).is_some() {
            continue;
        }
        let output = match execute(bins, &project, scenario, step, Runner::Ts, &ids) {
            Ok(output) => output,
            // An unresolvable placeholder is reported as an error by `execute`;
            // here it is a finding about the scenario, not a reason to stop.
            Err(err) => {
                problems.push(format!("{err:#}"));
                continue;
            }
        };
        ids.learn(&output.stdout);

        let usage_error = crate::scenario::is_usage_error(&output.stderr);
        if usage_error && !Scenario::exit_only(step) {
            problems.push(format!(
                "step `{step}`: the oracle rejected this argv at parse time, so the step \
                 asserts nothing — fix the flag, or mark it `@exit-only` if that is the point\n\
                 stderr: {}",
                output.stderr.trim()
            ));
        }
        if !usage_error
            && Scenario::exit_only(step)
            && !Scenario::exit_only_reason(step)
            && !is_group_help(&output)
        {
            problems.push(format!(
                "step `{step}`: marked `@exit-only`, but the oracle ran it and produced real \
                 output, so the marker throws away a comparison that would have counted. \
                 Drop it, or say why the output is L2 with `@exit-only:<reason>`.\nstdout: {}",
                output.stdout.trim().chars().take(200).collect::<String>()
            ));
        }
    }

    Ok(problems)
}

/// A command group invoked with no subcommand: Commander prints the group's
/// help to stderr and exits 1, which is not an `error:` line but is still
/// output whose layout is L2 (PARITY.md §1.3), so `@exit-only` is right there.
fn is_group_help(output: &StepOutput) -> bool {
    output.exit_code == 1 && output.stdout.is_empty() && output.stderr.starts_with("Usage:")
}

fn mapping_of(norm: &Normalizer) -> Vec<(String, String)> {
    norm.mapping()
        .iter()
        .map(|(id, placeholder)| (id.clone(), placeholder.clone()))
        .collect()
}

fn execute(
    bins: &Binaries,
    project: &Project,
    scenario: &Scenario,
    step: &str,
    runner: Runner,
    ids: &IdRegistry,
) -> Result<StepOutput> {
    // Expanded per tree: the two binaries mint different ids, so `${task.0}`
    // resolves to whatever *this* tree produced.
    let argv = ids
        .expand(&scenario.argv(step)?)
        .map_err(|err| anyhow!("step `{step}`: {err}"))?;
    let (mut command, label) = match runner {
        Runner::Rust | Runner::Both => (Command::new(&bins.rust), "rust"),
        Runner::Ts => {
            let mut command = Command::new(&bins.node);
            command.arg(&bins.oracle_entry);
            (command, "ts")
        }
    };

    if scenario.fake_agents || scenario.fake_editor.is_some() {
        command.env("PATH", stand_in_path(bins, scenario)?);
    }
    if let Some(editor) = &scenario.fake_editor {
        // `VISUAL` wins over `EDITOR` in some editors but not in the oracle,
        // which checks `EDITOR` first — so set both to the same script and
        // leave the precedence question to a test of its own.
        command.env("EDITOR", editor).env("VISUAL", editor);
    }
    for (key, value) in &scenario.env {
        command.env(key, value);
    }

    command
        .args(&argv)
        .current_dir(&project.root)
        .env("HOME", &project.home)
        .env("USERPROFILE", &project.home)
        // `global.yml` lives under the harness HOME too, so a scenario cannot
        // read or write the developer's real preferences.
        .env(
            "ORCHESTRY_GLOBAL_CONFIG_PATH",
            project.home.join(".orchestry/global.yml"),
        )
        // The oracle's vitest config caps workers at 2 under CI; the habit
        // belongs in the tooling even though the binary is unaffected
        // (PARITY.md §14.9).
        .env("CI", "1")
        // Deterministic presentation on both sides.
        .env("NO_COLOR", "1")
        .env_remove("ORCHESTRY_DEBUG")
        .stdin(std::process::Stdio::null());

    let output = match Scenario::kill_after(step) {
        Some(after) => run_and_kill(command, after, label, step)?,
        None => run_with_timeout(command, scenario.timeout(), label, step)?,
    };

    Ok(StepOutput {
        step: step.to_owned(),
        runner: label,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Run a step and `SIGKILL` it after `after`, for the crash-recovery
/// scenarios. The exit code is whatever the kill produced, which the scenario
/// is expected to tolerate — what is compared is the state the next step finds.
fn run_and_kill(
    mut command: Command,
    after: std::time::Duration,
    label: &str,
    step: &str,
) -> Result<std::process::Output> {
    let scratch = tempfile::tempdir().context("create stdio scratch dir")?;
    let (out_path, err_path) = (scratch.path().join("stdout"), scratch.path().join("stderr"));
    command
        .stdout(std::fs::File::create(&out_path).context("create stdout file")?)
        .stderr(std::fs::File::create(&err_path).context("create stderr file")?);

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {label} step `{step}`"))?;

    std::thread::sleep(after);
    // Already finished is fine: the point is that it did not outlive the
    // deadline, not that it was necessarily still running.
    let status = match child.try_wait().context("poll child")? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            child.wait().context("reap killed child")?
        }
    };

    Ok(std::process::Output {
        status,
        stdout: std::fs::read(&out_path).context("read captured stdout")?,
        stderr: std::fs::read(&err_path).context("read captured stderr")?,
    })
}

/// `fixtures/fake-agents/` first on `PATH`, with the inherited `PATH` after
/// it so `git` and `sh` still resolve.
/// `PATH` with the stand-in directories this scenario asked for in front, so
/// both binaries spawn the fixtures rather than whatever the machine has
/// installed.
fn stand_in_path(bins: &Binaries, scenario: &Scenario) -> Result<String> {
    let mut dirs = Vec::new();
    if scenario.fake_agents {
        dirs.push("fixtures/fake-agents");
    }
    if scenario.fake_editor.is_some() {
        dirs.push("fixtures/fake-editors");
    }

    let mut path = String::new();
    for dir in dirs {
        let resolved = bins
            .repo_root
            .join(dir)
            .canonicalize()
            .with_context(|| format!("locate {dir}"))?;
        path.push_str(&resolved.display().to_string());
        path.push(':');
    }
    path.push_str(&std::env::var("PATH").unwrap_or_default());
    Ok(path)
}

/// Run to completion or kill at the deadline.
///
/// `Command::output()` has no timeout, and a `run --watch` or a `serve`
/// without `--once` never terminates — so a scenario that names one must fail
/// on its own rather than hanging the harness. stdio goes to temp files
/// instead of pipes, which also removes any chance of deadlocking on a full
/// pipe while we wait.
fn run_with_timeout(
    mut command: Command,
    timeout: std::time::Duration,
    label: &str,
    step: &str,
) -> Result<std::process::Output> {
    let scratch = tempfile::tempdir().context("create stdio scratch dir")?;
    let (out_path, err_path) = (scratch.path().join("stdout"), scratch.path().join("stderr"));
    command
        .stdout(std::fs::File::create(&out_path).context("create stdout file")?)
        .stderr(std::fs::File::create(&err_path).context("create stderr file")?);

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {label} step `{step}`"))?;

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait().context("poll child")? {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "{label} step `{step}` did not finish within {timeout:?} — a \
                     non-terminating command (`run --watch`, `serve` without `--once`) \
                     cannot be a scenario step"
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };

    Ok(std::process::Output {
        status,
        stdout: std::fs::read(&out_path).context("read captured stdout")?,
        stderr: std::fs::read(&err_path).context("read captured stderr")?,
    })
}

/// The byte-level properties of a JSON artifact that **are** contract even
/// though key order is not: `JSON.stringify(data, null, 2) + "\n"` — 2-space
/// indent and a trailing newline (PARITY.md §6.4).
///
/// Comparing parsed values alone cannot see these, which a mutation probe
/// proved: dropping the trailing newline went undetected until this was added.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonFraming {
    trailing_newlines: usize,
    /// Leading spaces on the first indented line, or `None` for a document
    /// with no nesting.
    indent: Option<usize>,
}

impl JsonFraming {
    fn of(raw: &str) -> JsonFraming {
        let trailing_newlines = raw.len() - raw.trim_end_matches('\n').len();
        let indent = raw
            .lines()
            .skip(1)
            .map(|line| line.len() - line.trim_start_matches(' ').len())
            .find(|width| *width > 0);
        JsonFraming {
            trailing_newlines,
            indent,
        }
    }
}

/// Every file under `.orchestry/`, keyed by its **normalized** relative path,
/// parsed according to its extension.
fn collect_tree(project_root: &Path, norm: &mut Normalizer) -> Result<Tree> {
    let root = project_root.join(".orchestry");
    let mut files = Vec::new();
    collect_files(&root, &root, &mut files)?;
    files.sort();

    let mut framing = BTreeMap::new();
    let mut tree = BTreeMap::new();
    for relative in files {
        let path = root.join(&relative);
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        // Parse first, normalize second: substituting placeholders into the
        // raw text would make `"pid": 360135` invalid JSON.
        let parsed = parse_by_extension(&relative, &raw)
            .with_context(|| format!("parse {}", path.display()))?;
        let value = norm.value(&parsed);
        let key = norm.path(&relative.to_string_lossy());
        if relative.extension().is_some_and(|e| e == "json") {
            framing.insert(key.clone(), JsonFraming::of(&raw));
        }
        if let Some(previous) = tree.insert(key.clone(), value) {
            bail!(
                "two files normalize to the same path {key}: the id mapping is not \
                 injective, which means an id never appeared in the scenario output \
                 (previous value: {previous})"
            );
        }
    }
    Ok(Tree {
        values: tree,
        framing,
    })
}

/// A collected `.orchestry/` tree.
#[derive(Debug, Default)]
struct Tree {
    values: BTreeMap<String, Value>,
    framing: BTreeMap<String, JsonFraming>,
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(anyhow!("read dir {}: {err}", dir.display())),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            out.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

fn parse_by_extension(relative: &Path, text: &str) -> Result<Value> {
    match relative.extension().and_then(|e| e.to_str()) {
        Some("json") => Ok(serde_json::from_str(text)?),
        Some("yml" | "yaml") => {
            let mut value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)?;
            value.apply_merge()?;
            Ok(serde_json::to_value(value)?)
        }
        Some("jsonl") => {
            let records: Result<Vec<Value>> = text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| Ok(serde_json::from_str(line)?))
                .collect();
            Ok(Value::Array(records?))
        }
        // Plain text (`.gitignore`, `workspace-exclude`, `templates/*.md`,
        // the lock file) compares as a string, so byte differences show.
        _ => Ok(Value::String(text.to_owned())),
    }
}

/// Fail a step the oracle rejected at parse time, unless the scenario says it
/// means to.
///
/// Both binaries reject a misspelled flag the same way, so such a step is
/// green and vacuous: it compares two usage messages and never reaches the
/// code under test. Scenarios that deliberately test usage errors set
/// `allow_usage_errors`.
fn usage_error_failure(_scenario: &Scenario, step: &str, ts_stderr: &str) -> Option<String> {
    if Scenario::exit_only(step) || !crate::scenario::is_usage_error(ts_stderr) {
        return None;
    }
    Some(format!(
        "step `{step}`: the oracle rejected this argv at parse time, so the step asserts \
         nothing about behaviour — fix the flag, or prefix the step with `@exit-only` if the \
         usage error is the point\nstderr: {}",
        ts_stderr.trim()
    ))
}

fn compare_trees(rust: &Tree, ts: &Tree) -> Vec<String> {
    let mut failures = compare_values(&rust.values, &ts.values);

    for (path, ts_framing) in &ts.framing {
        if let Some(rust_framing) = rust.framing.get(path)
            && rust_framing != ts_framing
        {
            failures.push(format!(
                "`.orchestry/{path}` has different JSON framing — 2-space indent plus a \
                 trailing newline is contract (PARITY.md §6.4)\n    ts:   {ts_framing:?}\n    \
                 rust: {rust_framing:?}"
            ));
        }
    }
    failures
}

fn compare_values(rust: &BTreeMap<String, Value>, ts: &BTreeMap<String, Value>) -> Vec<String> {
    let mut failures = Vec::new();

    for path in ts.keys() {
        if !rust.contains_key(path) {
            failures.push(format!(
                "`.orchestry/{path}` exists in the ts tree but not in rust"
            ));
        }
    }
    for path in rust.keys() {
        if !ts.contains_key(path) {
            failures.push(format!(
                "`.orchestry/{path}` exists in the rust tree but not in ts"
            ));
        }
    }
    for (path, ts_value) in ts {
        if let Some(rust_value) = rust.get(path)
            && rust_value != ts_value
        {
            failures.push(format!(
                "`.orchestry/{path}` differs\n{}",
                unified_diff(&pretty(ts_value), &pretty(rust_value))
            ));
        }
    }
    failures
}

/// V32 — re-parse every YAML artifact **in the node process with js-yaml**
/// and compare against the Rust parse.
///
/// This is the only check that tests what PARITY.md §6.4 calls the
/// cross-compat crux: verifying both trees with the Rust parser alone would
/// make every "js-yaml reads r-orch's bytes differently" bug invisible by
/// construction. It subsumes the timestamp assertion (V8) as a special case.
fn cross_parse_failures(bins: &Binaries, project: &Project) -> Result<Vec<String>> {
    let root = project.root.join(".orchestry");
    let mut files = Vec::new();
    collect_files(&root, &root, &mut files)?;
    let yaml: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml"))
        .collect();
    if yaml.is_empty() {
        return Ok(Vec::new());
    }

    let absolute: Vec<String> = yaml
        .iter()
        .map(|p| root.join(p).to_string_lossy().into_owned())
        .collect();

    // `JSON.stringify` would turn a js-yaml `Date` straight back into an ISO
    // string, erasing the exact difference this check exists to find — a
    // measured miss, not a hypothetical one. So every value is **type-tagged**
    // before it crosses back: a Date, a Buffer or anything else that is not a
    // plain JSON value reports itself structurally and therefore cannot
    // compare equal to our string.
    let script = r#"
const fs = require('node:fs');
const path = require('node:path');
// argv[2] is the oracle package root. Resolving js-yaml from there rather
// than from this script's own directory is what makes the check use the
// *vendored* js-yaml — the exact parser the reference binary reads with.
const oracleRoot = process.argv[2];
const yaml = require(path.join(oracleRoot, 'node_modules', 'js-yaml'));

function tag(value) {
  if (value === null) return null;
  if (value === undefined) return { __js: 'undefined' };
  if (value instanceof Date) return { __js: 'Date', iso: value.toISOString() };
  if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
    return { __js: 'Buffer', base64: Buffer.from(value).toString('base64') };
  }
  if (Array.isArray(value)) return value.map(tag);
  const kind = typeof value;
  if (kind === 'string' || kind === 'number' || kind === 'boolean') return value;
  if (kind !== 'object') return { __js: kind, text: String(value) };
  if (Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) {
    return { __js: value.constructor ? value.constructor.name : 'unknown' };
  }
  const out = {};
  for (const [key, child] of Object.entries(value)) out[key] = tag(child);
  return out;
}

const out = {};
for (const file of process.argv.slice(3)) {
  try {
    // The DEFAULT_SCHEMA is what the real binary reads with, timestamp type
    // included — that is the whole point of the check.
    out[file] = { ok: true, value: tag(yaml.load(fs.readFileSync(file, 'utf8'))) };
  } catch (err) {
    out[file] = { ok: false, error: String(err && err.message || err) };
  }
}
process.stdout.write(JSON.stringify(out));
"#;

    // `node -e <script> a b` puts `a` at argv[1], not argv[2], so the script
    // goes to a file and is invoked normally.
    let script_dir = tempfile::tempdir().context("create cross-parse script dir")?;
    let script_path = script_dir.path().join("cross-parse.cjs");
    std::fs::write(&script_path, script).context("write cross-parse script")?;

    let oracle_root = bins
        .oracle_entry
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow!("oracle entry has no package root"))?;

    let output = Command::new(&bins.node)
        .arg(&script_path)
        .arg(oracle_root)
        .args(&absolute)
        .output()
        .context("run the js-yaml cross-parse step")?;
    if !output.status.success() {
        bail!(
            "js-yaml cross-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let parsed: BTreeMap<String, JsYamlResult> =
        serde_json::from_slice(&output.stdout).context("parse the js-yaml cross-parse output")?;

    let mut failures = Vec::new();
    for (path, relative) in absolute.iter().zip(&yaml) {
        let Some(js) = parsed.get(path) else {
            failures.push(format!(
                "js-yaml did not report on `{}`",
                relative.display()
            ));
            continue;
        };
        if !js.ok {
            failures.push(format!(
                "js-yaml cannot read `.orchestry/{}`: {}",
                relative.display(),
                js.error.as_deref().unwrap_or("unknown error")
            ));
            continue;
        }
        let text = std::fs::read_to_string(root.join(relative))?;
        let mut ours: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)?;
        ours.apply_merge()?;
        let ours = serde_json::to_value(ours)?;
        let theirs = js.value.clone().unwrap_or(Value::Null);

        if ours != theirs {
            failures.push(format!(
                "`.orchestry/{}` parses differently in js-yaml than in serde_yaml_ng — \
                 this is the cross-compat crux (PARITY.md §6.4)\n{}",
                relative.display(),
                unified_diff(&pretty(&theirs), &pretty(&ours))
            ));
        }
    }
    Ok(failures)
}

#[derive(Debug, serde::Deserialize)]
struct JsYamlResult {
    ok: bool,
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// Parse JSONL stdout into normalized records. A line that is not JSON is
/// kept as a string record, so a stray human line still shows in the diff
/// rather than vanishing.
fn json_records(stdout: &str, norm: &mut Normalizer) -> Vec<Value> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| match serde_json::from_str::<Value>(line) {
            Ok(value) => norm.value(&value),
            Err(_) => Value::String(norm.text(line)),
        })
        .collect()
}

fn records_text(records: &[Value]) -> String {
    records
        .iter()
        .map(|record| format!("{}\n", pretty(record)))
        .collect()
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// A unified diff, `expected` (the oracle) against `actual` (r-orch).
fn unified_diff(expected: &str, actual: &str) -> String {
    use similar::{ChangeTag, TextDiff};

    let diff = TextDiff::from_lines(expected, actual);
    let mut out = String::from("  --- ts (oracle)\n  +++ rust\n");
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        if change.tag() == ChangeTag::Equal {
            continue;
        }
        out.push_str(&format!("  {sign}{}", change.value()));
        if !change.value().ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario_with(body: &str) -> Scenario {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("s.toml");
        std::fs::write(&path, body).unwrap();
        Scenario::load(&path).unwrap()
    }

    #[test]
    fn a_step_the_oracle_rejected_at_parse_time_fails_unless_declared() {
        let scenario =
            scenario_with("name = \"s\"\nkind = \"differential\"\nsteps = [\"task list\"]\n");
        let stderr = "error: unknown option '--nosuchflag'\n";

        let failure = usage_error_failure(&scenario, "task list --nosuchflag", stderr)
            .expect("a misspelled flag must fail rather than pass vacuously");
        assert!(failure.contains("asserts nothing"), "{failure}");

        // Declared, so it is the point of the step rather than a typo.
        assert!(
            usage_error_failure(&scenario, "@exit-only task list --nosuchflag", stderr).is_none()
        );
        // A domain error is not a usage error.
        assert!(
            usage_error_failure(&scenario, "task show tsk_nosuch", "  ✕ Task not found\n")
                .is_none()
        );
    }

    #[test]
    fn stand_in_directories_go_on_the_path_only_when_asked_for() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let bins = Binaries {
            rust: PathBuf::from("/nonexistent"),
            oracle_entry: PathBuf::from("/nonexistent"),
            node: PathBuf::from("node"),
            repo_root,
            _snapshot: None,
        };

        let plain = scenario_with("name = \"s\"\nkind = \"differential\"\nsteps = [\"a\"]\n");
        let path = stand_in_path(&bins, &plain).unwrap();
        assert!(!path.contains("fake-agents"), "{path}");
        assert!(!path.contains("fake-editors"), "{path}");

        let both = scenario_with(
            "name = \"s\"\nkind = \"differential\"\nfake_agents = true\n\
             fake_editor = \"orch-edit-title\"\nsteps = [\"a\"]\n",
        );
        let path = stand_in_path(&bins, &both).unwrap();
        let agents = path.find("fake-agents").expect("fake-agents on PATH");
        let editors = path.find("fake-editors").expect("fake-editors on PATH");
        // Both ahead of the inherited PATH, so the fixtures win over anything
        // the machine happens to have installed.
        let inherited = path.find("/usr/bin").unwrap_or(usize::MAX);
        assert!(agents < inherited && editors < inherited, "{path}");
    }

    #[test]
    fn a_group_help_step_is_recognized_as_l2_output() {
        // `orch task` with no subcommand: Commander prints the group's help to
        // stderr and exits 1 — measured, and contrary to PARITY.md §3, which
        // says stdout. The lint must accept a bare `@exit-only` on it.
        let output = StepOutput {
            step: "task".into(),
            runner: "ts",
            stdout: String::new(),
            stderr: "Usage: orchestry task [options] [command]\n".into(),
            exit_code: 1,
        };
        assert!(is_group_help(&output));

        let with_stdout = StepOutput {
            stdout: "something real\n".into(),
            ..output.clone()
        };
        assert!(!is_group_help(&with_stdout));
    }

    #[test]
    fn a_tmpdir_with_an_orchestry_ancestor_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(assert_no_orchestry_ancestor(&nested).is_ok());

        std::fs::create_dir_all(temp.path().join(".orchestry")).unwrap();
        let err = assert_no_orchestry_ancestor(&nested).expect_err("rejected");
        assert!(
            err.to_string().contains("findProjectRoot would escape"),
            "{err}"
        );
    }

    #[test]
    fn a_projects_own_orchestry_does_not_trip_the_ancestor_check() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".orchestry")).unwrap();
        assert!(assert_no_orchestry_ancestor(temp.path()).is_ok());
    }

    #[test]
    fn the_seeded_update_cache_looks_fresh_and_offers_no_update() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join(".orchestry")).unwrap();
        seed_update_cache(&home).unwrap();

        let raw = std::fs::read_to_string(home.join(".orchestry/update-check.json")).unwrap();
        let cache: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(cache["latest"], ORACLE_VERSION);
        let checked_at = cache["checked_at"].as_u64().expect("millis");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // Well inside the 3-hour revalidation window, so no npm spawn.
        assert!(now - checked_at < 60_000, "stale seed");
    }

    #[test]
    fn files_are_parsed_according_to_their_extension() {
        assert_eq!(
            parse_by_extension(Path::new("state.json"), "{\"a\":1}").unwrap(),
            serde_json::json!({ "a": 1 })
        );
        assert_eq!(
            parse_by_extension(Path::new("tasks/x.yml"), "a: 1\n").unwrap(),
            serde_json::json!({ "a": 1 })
        );
        assert_eq!(
            parse_by_extension(Path::new("runs/x.jsonl"), "{\"a\":1}\n{\"b\":2}\n").unwrap(),
            serde_json::json!([{ "a": 1 }, { "b": 2 }])
        );
        assert_eq!(
            parse_by_extension(Path::new(".gitignore"), "state.json\n").unwrap(),
            Value::String("state.json\n".to_owned())
        );
    }

    #[test]
    fn yaml_merge_keys_are_resolved_before_comparison() {
        let value = parse_by_extension(
            Path::new("config.yml"),
            "base: &b\n  x: 1\nscheduling:\n  <<: *b\n",
        )
        .unwrap();
        assert_eq!(value["scheduling"]["x"], 1);
    }

    #[test]
    fn tree_comparison_reports_missing_and_differing_files() {
        let mut rust = BTreeMap::new();
        rust.insert("a.json".to_owned(), serde_json::json!({ "v": 1 }));
        rust.insert("only-rust.json".to_owned(), Value::Null);
        let mut ts = BTreeMap::new();
        ts.insert("a.json".to_owned(), serde_json::json!({ "v": 2 }));
        ts.insert("only-ts.json".to_owned(), Value::Null);

        let failures = compare_values(&rust, &ts);
        assert_eq!(failures.len(), 3);
        assert!(
            failures
                .iter()
                .any(|f| f.contains("only-ts.json") && f.contains("not in rust"))
        );
        assert!(
            failures
                .iter()
                .any(|f| f.contains("only-rust.json") && f.contains("not in ts"))
        );
        assert!(
            failures
                .iter()
                .any(|f| f.contains("a.json") && f.contains("differs"))
        );
    }

    #[test]
    fn identical_trees_produce_no_failures() {
        let mut tree = BTreeMap::new();
        tree.insert("a.json".to_owned(), serde_json::json!({ "v": 1 }));
        assert!(compare_values(&tree, &tree).is_empty());
    }

    #[test]
    fn json_framing_captures_the_trailing_newline_and_indent() {
        let with = JsonFraming::of("{\n  \"a\": 1\n}\n");
        assert_eq!(
            with,
            JsonFraming {
                trailing_newlines: 1,
                indent: Some(2)
            }
        );

        // The exact difference a mutation probe showed the value comparison
        // cannot see.
        let without = JsonFraming::of("{\n  \"a\": 1\n}");
        assert_eq!(
            without,
            JsonFraming {
                trailing_newlines: 0,
                indent: Some(2)
            }
        );
        assert_ne!(with, without);

        // A four-space indent is a different framing too.
        assert_eq!(JsonFraming::of("{\n    \"a\": 1\n}\n").indent, Some(4));
        // A flat document has no indented line.
        assert_eq!(JsonFraming::of("[]\n").indent, None);
    }

    #[test]
    fn framing_differences_are_reported() {
        let mut rust = Tree::default();
        rust.values.insert("a.json".to_owned(), Value::Null);
        rust.framing.insert(
            "a.json".to_owned(),
            JsonFraming {
                trailing_newlines: 0,
                indent: Some(2),
            },
        );
        let mut ts = Tree::default();
        ts.values.insert("a.json".to_owned(), Value::Null);
        ts.framing.insert(
            "a.json".to_owned(),
            JsonFraming {
                trailing_newlines: 1,
                indent: Some(2),
            },
        );

        let failures = compare_trees(&rust, &ts);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("JSON framing"), "{}", failures[0]);
    }

    #[test]
    fn the_diff_labels_which_side_is_which() {
        let diff = unified_diff("expected\n", "actual\n");
        assert!(diff.contains("--- ts (oracle)"));
        assert!(diff.contains("+++ rust"));
        assert!(diff.contains("-expected"));
        assert!(diff.contains("+actual"));
    }
}
