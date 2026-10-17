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

use std::collections::{BTreeMap, BTreeSet};
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
    /// Both halves matter. The work packages originally shared one
    /// `CARGO_TARGET_DIR` across git worktrees, and artifacts from two trees
    /// hash identically, so `target/debug/orch` was whichever tree built last
    /// and a parity run reading it could silently grade a different branch.
    /// (Observed: a run reported 11 scenarios failing with rich diffs because
    /// it was testing a worktree with the full command surface while this tree
    /// had three commands.) The worktrees have their own target directories
    /// now, but both halves still earn their keep: building is what makes the
    /// binary *this source*, and the snapshot is what stops a concurrent
    /// rebuild swapping it out from under a run that takes minutes.
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
        verify_oracle_is_patched(repo_root)?;

        Ok(Binaries {
            rust,
            oracle_entry,
            node: PathBuf::from("node"),
            repo_root: repo_root.to_path_buf(),
            _snapshot: Some(snapshot),
        })
    }
}

/// Refuse to run against an oracle that is not the patched, freshly built one.
///
/// `parity/patches/` holds the minimal patches that make some behaviour
/// observable at all (see that README). Those patches are the difference
/// between a scenario being differential and being vacuous, and the failure is
/// silent: the unpatched oracle exits 0 and simply does nothing. A harness that
/// kept going would report ten green scenarios about a binary that never
/// dispatched.
///
/// Three things have to line up, and the stamp is written only after a
/// successful build:
///
/// 1. the stamp exists at all — `parity/setup.sh` has been run since the
///    patches were introduced;
/// 2. it lists every patch currently in `parity/patches/` — a patch added
///    later is not silently skipped;
/// 3. its recorded hash matches the built `dist/` — someone rebuilding with a
///    plain `npm run build` after a bare `git checkout` invalidates it rather
///    than leaving a stamp that lies.
fn verify_oracle_is_patched(repo_root: &Path) -> Result<()> {
    let stamp_path = repo_root.join("parity/orch-ts/.parity-stamp");
    let rerun = "run `./parity/setup.sh` to apply the patches and rebuild";

    let stamp = std::fs::read_to_string(&stamp_path).with_context(|| {
        format!(
            "{} is missing — the oracle may be unpatched, and an unpatched oracle fails \
             *silently* (it exits 0 without dispatching, so scenarios pass while asserting \
             nothing). {rerun}",
            stamp_path.display()
        )
    })?;

    let stamped: BTreeSet<&str> = stamp
        .lines()
        .filter_map(|line| line.strip_prefix("patch "))
        .map(str::trim)
        .collect();

    let patch_dir = repo_root.join("parity/patches");
    let mut expected: Vec<String> = Vec::new();
    if patch_dir.is_dir() {
        for entry in std::fs::read_dir(&patch_dir)
            .with_context(|| format!("read {}", patch_dir.display()))?
        {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "patch") {
                expected.push(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    expected.sort();

    let missing: Vec<&String> = expected
        .iter()
        .filter(|name| !stamped.contains(name.as_str()))
        .collect();
    if !missing.is_empty() {
        bail!("the built oracle does not include {missing:?} from parity/patches/ — {rerun}");
    }

    let recorded = stamp
        .lines()
        .find_map(|line| line.strip_prefix("dist-sha256 "))
        .map(str::trim)
        .context("the parity stamp has no dist-sha256 line — {rerun}")?;
    let actual = dist_hash(&repo_root.join("parity/orch-ts"))?;
    if recorded != actual {
        bail!(
            "the oracle's dist/ does not match the stamp, so it was rebuilt without the \
             patches (stamp {}…, built {}…) — {rerun}",
            &recorded[..recorded.len().min(12)],
            &actual[..actual.len().min(12)]
        );
    }
    Ok(())
}

/// The oracle's `dist/` hash, computed by running the **same shell pipeline**
/// `setup.sh` records.
///
/// Shelling out rather than hashing in Rust on purpose: a second
/// implementation could disagree with the stamp over file order or line
/// endings and fail the gate for a reason that has nothing to do with the
/// oracle. Running the identical command makes that impossible, and xtask
/// already shells out to cargo and node.
fn dist_hash(oracle_root: &Path) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(
            "find dist -maxdepth 1 -name '*.js' -exec sha256sum {} + \
             | LC_ALL=C sort -k 2 | sha256sum | cut -d' ' -f1",
        )
        .current_dir(oracle_root)
        .output()
        .context("hash the oracle's dist/")?;
    if !output.status.success() {
        bail!(
            "could not hash the oracle's dist/: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
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
    /// The step outlived the scenario's budget and was killed.
    ///
    /// A **scenario failure**, not a harness error: one non-terminating step
    /// must not abort the whole corpus, or a single hang hides every other
    /// result. The partial output is kept and reported with it.
    pub timed_out: bool,
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

/// Which subcommands the Rust binary has **wired**, as opposed to declared.
///
/// `<cmd> --help` is no longer a sufficient probe on its own. WP-5a declares
/// the whole command tree so that V1's surface check is honest — every
/// command, subcommand, alias and flag of PARITY.md §2.2 exists and parses —
/// so `--help` answered yes for commands whose service was not wired yet, and
/// scenarios ran against them: neither a skip nor a real failure, and a
/// `${goal.0}` placeholder in such a scenario aborted the whole suite because
/// no goal was ever created.
///
/// So the probe is an explicit list, intersected with the `--help` check (so a
/// misspelled requirement still skips rather than silently passing). Every
/// declared command is wired now, which makes the list a ratchet rather than a
/// gate: a command removed from it stops its scenarios instead of letting
/// them fail.
const WIRED_COMMANDS: &[&str] = &[
    "agent add",
    "agent autonomous",
    "agent disable",
    "agent edit",
    "agent enable",
    "agent list",
    "agent remove",
    "agent shop",
    "agent status",
    "config edit",
    "config get",
    "config global get",
    "config global set",
    "config global show",
    "config set",
    "context delete",
    "context get",
    "context list",
    "context set",
    "doctor",
    "goal add",
    "goal delete",
    "goal list",
    "goal show",
    "goal status",
    "goal update",
    "init",
    "logs",
    "msg broadcast",
    "msg inbox",
    "msg list",
    "msg send",
    "org deploy",
    "org list",
    "run",
    "serve",
    "status",
    "task add",
    "task approve",
    "task assign",
    "task cancel",
    "task edit",
    "task list",
    "task reject",
    "task retry",
    "task show",
    "team add-task",
    "team create",
    "team disband",
    "team join",
    "team leave",
    "team list",
    "team set-lead",
    "team show",
    "tui",
    "update",
];

/// Which subcommands the Rust binary has wired, probed once per run.
///
/// The probe runs inside an **initialized project of its own**, and this is
/// not a detail: `<cmd> --help` builds the container before Commander parses
/// (PARITY.md §3), so in a directory that is not a project it exits **3**
/// (`Not initialized`) and every command looks absent — silently skipping the
/// entire corpus, since a skip is not a failure. Found by a mutation probe
/// that came back "detected" by two scenarios instead of twenty-nine.
///
/// The project also carries its own `HOME`. Without one the container
/// resolves its root by **walking up**, so once `~/.orchestry` existed every
/// probe latched onto the developer's own home as the project and could
/// create directories in it — fifteen probes per run, every run.
pub struct Capabilities {
    probed: std::cell::RefCell<BTreeMap<String, bool>>,
    project: std::cell::OnceCell<Project>,
}

impl Capabilities {
    pub fn new() -> Self {
        Capabilities {
            probed: std::cell::RefCell::new(BTreeMap::new()),
            project: std::cell::OnceCell::new(),
        }
    }

    /// Why probing is not isolated, if it is not.
    ///
    /// Always `None` now: the probe project is created eagerly on first use
    /// and a failure to create it is fatal, so there is no un-isolated
    /// fallback left to warn about. Kept because the caller prints it, and
    /// because a future probe that cannot be sandboxed should say so here
    /// rather than quietly inheriting the developer's environment.
    pub fn isolation_note(&self) -> Option<&'static str> {
        None
    }

    /// The initialized project every probe runs in, created on first use.
    fn probe_project(&self, bins: &Binaries) -> &Project {
        self.project.get_or_init(|| {
            let project =
                Project::create("capability-probe").expect("create the capability-probe project");
            let status = Command::new(&bins.rust)
                .args(["init", "--name", "probe", "--adapter", "claude"])
                .current_dir(&project.root)
                .env("HOME", &project.home)
                .env("USERPROFILE", &project.home)
                .env(
                    "ORCHESTRY_GLOBAL_CONFIG_PATH",
                    project.home.join(".orchestry/global.yml"),
                )
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("run init in the capability-probe project");
            assert!(
                status.success(),
                "init failed in the capability-probe project; every `requires` would skip"
            );
            project
        })
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
        // Declared *and* wired. The `--help` half catches a requirement that
        // names a command the tree does not have at all — a typo in a
        // scenario, or one written against a renamed verb.
        let argv = shell_words::split(command).unwrap_or_default();
        let project = self.probe_project(bins);
        let declared = Command::new(&bins.rust)
            .args(&argv)
            .arg("--help")
            .current_dir(&project.root)
            .env("HOME", &project.home)
            .env("USERPROFILE", &project.home)
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                project.home.join(".orchestry/global.yml"),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        let supported = declared && WIRED_COMMANDS.contains(&command);
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
            // A `@kill-after` step is killed on purpose, so its exit code is
            // whatever the signal produced — the scenario's subject is the
            // tree the *next* step finds, not this step's status.
            if output.exit_code != 0
                && Scenario::kill_after(step).is_none()
                && Scenario::stop_after(step).is_none()
            {
                failures.push(format!(
                    "step `{step}` ({}) exited {} — cross-compat steps must all succeed\n\
                     stdout: {}\nstderr: {}",
                    output.runner,
                    output.exit_code,
                    output.stdout.trim(),
                    output.stderr.trim()
                ));
            }
            if output.timed_out {
                failures.push(format!(
                    "step `{step}`: the {} binary did not finish within {:?} and was \
                     killed — the shared tree is left mid-flight, so nothing after this \
                     step is comparable",
                    output.runner,
                    scenario.timeout()
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
        let mut verified_claimed: Option<Value> = None;
        for step in &scenario.verify {
            let rust_out = execute(bins, &project, scenario, step, Runner::Rust, &ids)?;
            let ts_out = execute(bins, &project, scenario, step, Runner::Ts, &ids)?;

            if rust_out.timed_out || ts_out.timed_out {
                failures.push(format!(
                    "verify `{step}`: a binary did not finish within {:?} and was killed",
                    scenario.timeout()
                ));
                continue;
            }
            if rust_out.exit_code != ts_out.exit_code {
                failures.push(format!(
                    "verify `{step}`: exit code {} (rust) vs {} (ts) over the shared tree",
                    rust_out.exit_code, ts_out.exit_code
                ));
            }
            // The §14.1 exclusion applies here too: a cross-compat scenario
            // can verify with `status --json`, and the oracle's `claimed` is
            // `{}` whichever tree it reads. Without this the mask would hold
            // in one code path and not the other, which is the sort of gap
            // that shows up as a mystery failure months later.
            let verify_argv = scenario.argv(step)?;
            let masked_stdout = if claimed_mask::applies_to(&verify_argv) {
                // One tree, so both sides share a normalizer — the ids and
                // timestamps they print are literally the same ones.
                match compare_status_json(&rust_out.stdout, &ts_out.stdout, |value| {
                    norm.value(value)
                }) {
                    Ok((claimed, diff)) => {
                        verified_claimed = Some(claimed);
                        diff
                    }
                    Err(problem) => Some(("status --json", problem)),
                }
            } else if toolchain_mask::applies_to(&verify_argv) {
                // §13, over one shared tree: the row's position is still
                // contract, and a missing row on either side is a failure
                // rather than a free pass.
                match (
                    toolchain_mask::mask(&rust_out.stdout),
                    toolchain_mask::mask(&ts_out.stdout),
                ) {
                    (Some((rust_masked, rust_row)), Some((ts_masked, ts_row))) => {
                        if rust_row != ts_row {
                            Some((
                                "doctor toolchain row position",
                                format!(
                                    "§13 keeps the row's position; it moved\n    ts:   line \
                                     {ts_row}\n    rust: line {rust_row}"
                                ),
                            ))
                        } else {
                            let left = norm.text(&rust_masked);
                            let right = norm.text(&ts_masked);
                            (left != right).then(|| {
                                (
                                    "doctor output (toolchain row masked, §13)",
                                    unified_diff(&right, &left),
                                )
                            })
                        }
                    }
                    (rust_row, ts_row) => Some((
                        "doctor output",
                        format!(
                            "no toolchain row to mask, so the comparison would assert nothing \
                             — §13 says the row keeps its position and format (present in \
                             rust: {}, in ts: {})",
                            rust_row.is_some(),
                            ts_row.is_some()
                        ),
                    )),
                }
            } else if let Some((rust_doc, ts_doc)) =
                json_documents(&rust_out.stdout, &ts_out.stdout)
            {
                json_framing_diff(&rust_out.stdout, &ts_out.stdout).or_else(|| {
                    let unordered = Scenario::unordered(step);
                    let left = as_compared(&norm.value(&rust_doc), unordered);
                    let right = as_compared(&norm.value(&ts_doc), unordered);
                    (left != right).then(|| {
                        (
                            "json document",
                            unified_diff(&pretty(&right), &pretty(&left)),
                        )
                    })
                })
            } else {
                let left = norm.text(&rust_out.stdout);
                let right = norm.text(&ts_out.stdout);
                (left != right).then(|| ("stdout", unified_diff(&right, &left)))
            };
            if let Some((channel, diff)) = masked_stdout {
                failures.push(format!(
                    "verify `{step}`: {channel} differs over the shared tree — one binary \
                     misreads what the other wrote\n{diff}"
                ));
            }

            let left = norm.text(&rust_out.stderr);
            let right = norm.text(&ts_out.stderr);
            if left != right {
                failures.push(format!(
                    "verify `{step}`: stderr differs over the shared tree — one binary \
                     misreads what the other wrote\n{}",
                    unified_diff(&right, &left)
                ));
            }
        }

        if verified_claimed.is_some() {
            let tree = collect_tree(&project.root, &mut norm)?;
            failures.extend(claimed_matches_state_file(&verified_claimed, &tree));
        }
        failures.extend(cross_parse_failures(bins, &project, scenario)?);
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
        // r-orch's `state.claimed`, kept from the last `status --json` so it
        // can be checked against the `state.json` the run left behind.
        let mut masked_claimed: Option<Value> = None;

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

            // A step that had to be killed says nothing about parity, and its
            // partial output would produce a misleading diff — so it is
            // reported on its own and the rest of the step is skipped.
            let timed_out: Vec<&str> = [(&rust_out, "rust"), (&ts_out, "ts")]
                .into_iter()
                .filter(|(out, _)| out.timed_out)
                .map(|(_, label)| label)
                .collect();
            if !timed_out.is_empty() {
                failures.push(format!(
                    "step `{step}`: {} did not finish within {:?} and was killed — \
                     either the command does not terminate (`run --watch`, `serve` without \
                     `--once`) or the scenario waits for a status the tasks never reach \
                     (`review` is not terminal)",
                    timed_out.join(" and "),
                    scenario.timeout()
                ));
                continue;
            }
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
            // `status --json` carries the one named exclusion in the
            // contract (PARITY.md §14.1); everything else about the document
            // is still compared, and the masked value is re-asserted against
            // `state.json` after the tree is collected.
            let argv = scenario.argv(step)?;
            let stdout_diff = if claimed_mask::applies_to(&argv) {
                // Each side gets its own normalizer: two trees, two sets of ids.
                // Each side gets its own normalizer: two trees, two sets of
                // ids, so `<tsk_1>` must mean "whatever this tree made first".
                match (
                    mask_claimed(&rust_out.stdout, "r-orch"),
                    mask_claimed(&ts_out.stdout, "the oracle"),
                ) {
                    (Ok((rust_doc, claimed)), Ok((ts_doc, _))) => {
                        masked_claimed = Some(claimed);
                        let rust_framing = JsonFraming::of(&rust_out.stdout);
                        let ts_framing = JsonFraming::of(&ts_out.stdout);
                        if rust_framing != ts_framing {
                            Some((
                                "status --json framing",
                                format!(
                                    "2-space indent plus a trailing newline is contract \
                                     (§6.4)\n    ts:   {ts_framing:?}\n    rust: \
                                     {rust_framing:?}"
                                ),
                            ))
                        } else {
                            let unordered = Scenario::unordered(step);
                            let left = as_compared(&rust_norm.value(&rust_doc), unordered);
                            let right = as_compared(&ts_norm.value(&ts_doc), unordered);
                            (left != right).then(|| {
                                (
                                    "status --json (state.claimed masked, §14.1)",
                                    unified_diff(&pretty(&right), &pretty(&left)),
                                )
                            })
                        }
                    }
                    (Err(problem), _) | (_, Err(problem)) => Some(("status --json", problem)),
                }
            } else if toolchain_mask::applies_to(&argv) {
                // §13: the toolchain row's label and value legitimately
                // differ; its position does not.
                match (
                    toolchain_mask::mask(&rust_out.stdout),
                    toolchain_mask::mask(&ts_out.stdout),
                ) {
                    (Some((rust_masked, rust_row)), Some((ts_masked, ts_row))) => {
                        if rust_row != ts_row {
                            Some((
                                "doctor toolchain row position",
                                format!(
                                    "§13 keeps the row's position; it moved\n    ts:   line \
                                     {ts_row}\n    rust: line {rust_row}"
                                ),
                            ))
                        } else {
                            let left = rust_norm.text(&rust_masked);
                            let right = ts_norm.text(&ts_masked);
                            (left != right).then(|| {
                                (
                                    "doctor output (toolchain row masked, §13)",
                                    unified_diff(&right, &left),
                                )
                            })
                        }
                    }
                    (rust_row, ts_row) => Some((
                        "doctor output",
                        format!(
                            "no toolchain row to mask, so the comparison would assert nothing \
                             — §13 says the row keeps its position and format (present in \
                             rust: {}, in ts: {})",
                            rust_row.is_some(),
                            ts_row.is_some()
                        ),
                    )),
                }
            } else if let Some((rust_doc, ts_doc)) =
                json_documents(&rust_out.stdout, &ts_out.stdout)
            {
                json_framing_diff(&rust_out.stdout, &ts_out.stdout).or_else(|| {
                    let unordered = Scenario::unordered(step);
                    let left = as_compared(&rust_norm.value(&rust_doc), unordered);
                    let right = as_compared(&ts_norm.value(&ts_doc), unordered);
                    (left != right).then(|| {
                        (
                            "json document",
                            unified_diff(&pretty(&right), &pretty(&left)),
                        )
                    })
                })
            } else if scenario.stdout_is_json_records(step) {
                let drop_ticks = Scenario::ticks_vary(step);
                let left = json_records(&rust_out.stdout, &mut rust_norm, drop_ticks);
                let right = json_records(&ts_out.stdout, &mut ts_norm, drop_ticks);
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

            let (left, right) = if Scenario::stderr_prefix_only(step) {
                // §14 malformed-input row: the reason text comes from a
                // different parser on each side. Only the marker is contract.
                (
                    stderr_error_prefix(&rust_out.stderr),
                    stderr_error_prefix(&ts_out.stderr),
                )
            } else {
                (
                    rust_norm.text(&rust_out.stderr),
                    ts_norm.text(&ts_out.stderr),
                )
            };
            if left != right {
                failures.push(format!(
                    "step `{step}`: stderr differs\n{}",
                    unified_diff(&right, &left)
                ));
            }
        }

        let mut rust_tree = collect_tree(&rust.root, &mut rust_norm)?;
        let mut ts_tree = collect_tree(&ts.root, &mut ts_norm)?;
        failures.extend(exclude_tree_paths(scenario, &mut rust_tree, &mut ts_tree));
        failures.extend(compare_trees(
            &rust_tree,
            &ts_tree,
            &scenario.unordered_files,
        ));
        failures.extend(claimed_matches_state_file(&masked_claimed, &rust_tree));

        failures.extend(cross_parse_failures(bins, &rust, scenario)?);

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
    // `@write` puts bytes on disk rather than invoking a binary, for states
    // no command produces — a lock file holding garbage, say.
    if argv[0] == crate::scenario::WRITE_STEP {
        let path = project.root.join(&argv[1]);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {} for `@write`", parent.display()))?;
        }
        std::fs::write(&path, &argv[2]).with_context(|| format!("`@write` {}", path.display()))?;
        return Ok(StepOutput {
            step: step.to_owned(),
            runner: "harness",
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
        });
    }

    let (mut command, label) = match runner {
        Runner::Rust | Runner::Both => (Command::new(&bins.rust), "rust"),
        Runner::Ts => {
            let mut command = Command::new(&bins.node);
            command.arg(&bins.oracle_entry);
            (command, "ts")
        }
    };

    command.env("PATH", stand_in_path(bins, scenario)?);
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

    let (output, timed_out) = match (Scenario::kill_after(step), Scenario::stop_after(step)) {
        (Some(after), _) => (run_and_kill(command, after, label, step)?, false),
        (None, Some(after)) => (run_and_stop(command, after, label, step)?, false),
        (None, None) => run_with_timeout(command, scenario.timeout(), label, step)?,
    };

    Ok(StepOutput {
        step: step.to_owned(),
        runner: label,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
        timed_out,
    })
}

/// Run a step, `SIGTERM` it after `after`, and wait for it to shut down.
///
/// For a watch-mode orchestrator, which does not return on its own: this is
/// what a real operator's Ctrl+C does, so the tree the next step reads is a
/// cleanly shut-down one — agents reaped, lock released. A `SIGKILL` would
/// leave the lock file behind with a different pid per tree, and the steps
/// after it would be comparing wreckage.
fn run_and_stop(
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
    let status = match child.try_wait().context("poll child")? {
        // It finished on its own, which is fine: the point is that it did not
        // outlive the budget.
        Some(status) => status,
        None => {
            // `kill(1)` rather than `Child::kill`, which is SIGKILL.
            let _ = Command::new("kill")
                .args(["-TERM", &child.id().to_string()])
                .status();
            let deadline = std::time::Instant::now() + SHUTDOWN_GRACE;
            loop {
                match child.try_wait().context("poll child")? {
                    Some(status) => break status,
                    None if std::time::Instant::now() >= deadline => {
                        // A shutdown that needs SIGKILL is a finding in
                        // itself; the scenario then sees the exit code.
                        let _ = child.kill();
                        break child.wait().context("reap the killed child")?;
                    }
                    None => std::thread::sleep(std::time::Duration::from_millis(20)),
                }
            }
        }
    };

    Ok(std::process::Output {
        status,
        stdout: std::fs::read(&out_path).context("read captured stdout")?,
        stderr: std::fs::read(&err_path).context("read captured stderr")?,
    })
}

/// How long a `@stop-after` step gets to shut down before it is killed.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

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
    // Always, for every scenario. `doctor` probes whatever is on `PATH`, and
    // the adapters spawn from it, so a scenario that inherited the developer's
    // `PATH` would compare two runs against whatever that machine happens to
    // have installed — green here, red on the next machine, and silently
    // testing the real `claude` CLI rather than the fixture. It was opt-in
    // once; an opt-in that every scenario should set is just a trap.
    let mut dirs = vec!["fixtures/fake-agents"];
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
) -> Result<(std::process::Output, bool)> {
    let scratch = tempfile::tempdir().context("create stdio scratch dir")?;
    let (out_path, err_path) = (scratch.path().join("stdout"), scratch.path().join("stderr"));
    command
        .stdout(std::fs::File::create(&out_path).context("create stdout file")?)
        .stderr(std::fs::File::create(&err_path).context("create stderr file")?);

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {label} step `{step}`"))?;

    let deadline = std::time::Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().context("poll child")? {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                timed_out = true;
                break child.wait().context("reap the timed-out child")?;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    };

    Ok((
        std::process::Output {
            status,
            stdout: std::fs::read(&out_path).context("read captured stdout")?,
            stderr: std::fs::read(&err_path).context("read captured stderr")?,
        },
        timed_out,
    ))
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
    let mut unparseable = BTreeMap::new();
    let mut tree = BTreeMap::new();
    for relative in files {
        let path = root.join(&relative);
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        // Parse first, normalize second: substituting placeholders into the
        // raw text would make `"pid": 360135` invalid JSON.
        //
        // A file that will not parse is a *finding*, not a harness error. The
        // oracle can legitimately write JSON that serde_json rejects — a
        // message body truncated mid-surrogate-pair comes out as the escape
        // `\ud83d`, and serde_json refuses a lone surrogate outright — and
        // that divergence is exactly what a scenario would be testing. So
        // fall back to comparing the raw text and record why.
        let key = norm.path(&relative.to_string_lossy());
        let value = match parse_by_extension(&relative, &raw) {
            Ok(parsed) => norm.value(&parsed),
            Err(err) => {
                unparseable.insert(key.clone(), format!("{err}"));
                norm.value(&Value::String(raw.clone()))
            }
        };
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
        unparseable,
    })
}

/// A collected `.orchestry/` tree.
#[derive(Debug, Default, Clone)]
struct Tree {
    values: BTreeMap<String, Value>,
    framing: BTreeMap<String, JsonFraming>,
    /// Files whose own extension's parser rejected them, and why. Compared
    /// between trees, so "one binary wrote something the other's parser
    /// cannot read" is reported rather than crashing the run.
    unparseable: BTreeMap<String, String>,
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

/// The one named exclusion the parity contract grants, and the machinery that
/// keeps it from spreading.
///
/// PARITY.md §14.1 was decided **FIX**: TS `status --json` serializes the
/// in-memory state object directly, and `JSON.stringify` of a JS `Set` is `{}`
/// whatever it holds, so the oracle's `state.claimed` is always an empty
/// object while `state.json` on disk correctly holds an array. r-orch emits
/// the array. The two therefore cannot be compared at that one path, and §14.1
/// grants a *named* exclusion for it.
///
/// A masked value is an unasserted value, so the mask is paired with a
/// replacement assertion rather than left as a hole: r-orch's array must equal
/// the `claimed` array in the `state.json` it wrote. The cross-implementation
/// check on the same data still happens too — `state.json` is an ordinary file
/// in the tree walk, where both implementations agree on the array — so the
/// mask removes a duplicate comparison in a format that legitimately differs,
/// not the only one.
mod claimed_mask {
    use super::*;

    /// The masked path, as it appears in `status --json`.
    pub const PATH: &str = "state.claimed";

    /// Whether the §14.1 mask applies to this step.
    ///
    /// Deliberately narrow: `status` with `--json` and nothing else. The mask
    /// exists for one documented divergence, and a mask that drifted onto
    /// another command would silently stop comparing a field nobody agreed to
    /// stop comparing. `probe-harness.sh` asserts that breaking `claimed`
    /// anywhere *else* is still caught.
    pub fn applies_to(argv: &[String]) -> bool {
        // The subcommand is the first non-flag argument, because the global
        // flags are accepted on either side of it (`--ascii status --json`
        // and `status --json --ascii` are both valid — measured). This is the
        // same rule the oracle's own `cli.ts` uses to pick the subcommand,
        // limitations included: it would be fooled by a value-taking global
        // flag, and neither has one.
        let subcommand = argv.iter().find(|arg| !arg.starts_with('-'));
        subcommand.map(String::as_str) == Some("status") && argv.iter().any(|arg| arg == "--json")
    }

    /// Remove `state.claimed` from a parsed `status --json` document, handing
    /// back whatever was there.
    ///
    /// `None` for the value means the document had no such path, which is
    /// itself worth reporting: a `status --json` that stopped emitting
    /// `state.claimed` would otherwise pass by having nothing to mask.
    pub fn take(document: &mut Value) -> Option<Value> {
        document
            .get_mut("state")?
            .as_object_mut()?
            .shift_remove("claimed")
    }
}

/// Drop a scenario's named tree exclusions from both sides, reporting one that
/// matched nothing.
///
/// A path that no longer exists means the exclusion is stale, and a stale
/// exclusion is a comparison nobody is doing any more for a reason nobody
/// remembers — so it fails rather than passing quietly.
fn exclude_tree_paths(scenario: &Scenario, rust: &mut Tree, ts: &mut Tree) -> Vec<String> {
    let mut failures = Vec::new();

    for prefix in &scenario.excluded_tree_paths {
        let matched: Vec<String> = ts
            .values
            .keys()
            .chain(rust.values.keys())
            .filter(|path| path.starts_with(prefix.as_str()))
            .cloned()
            .collect();

        if matched.is_empty() {
            failures.push(format!(
                "`excluded_tree_paths` lists `{prefix}`, which matches nothing in either tree \
                 — a stale exclusion silently stops comparing something. Remove it."
            ));
            continue;
        }
        for path in matched {
            rust.values.remove(&path);
            ts.values.remove(&path);
            rust.framing.remove(&path);
            ts.framing.remove(&path);
            rust.unparseable.remove(&path);
            ts.unparseable.remove(&path);
        }
    }
    failures
}

/// The second named exclusion: `doctor`'s toolchain row (PARITY.md §13).
///
/// §13 replaces the `node` version check with the r-orch build identity. The
/// row keeps its **position and format**; only its label and value change. So
/// the harness masks that one row rather than pretending the two match — and,
/// because position is the part §13 actually promises, checks that the row sits
/// at the same index on both sides.
///
/// Like the §14.1 mask, this is a hole in the gate, so it is as narrow as it
/// can be: `doctor` steps only, one row, and a missing row is a failure rather
/// than a free pass.
mod toolchain_mask {
    /// The TS label and the r-orch label, which are the only two this may hit.
    const LABELS: [&str; 2] = ["node", "toolchain"];

    /// Whether the §13 mask applies to this step.
    pub fn applies_to(argv: &[String]) -> bool {
        argv.iter()
            .find(|arg| !arg.starts_with('-'))
            .map(String::as_str)
            == Some("doctor")
    }

    /// Replace the toolchain row with a placeholder, reporting which line it
    /// was on so the two sides' positions can be compared.
    ///
    /// `None` when there is no such row: masking a row that is not there would
    /// assert nothing, and a `doctor` that stopped printing a toolchain row
    /// would otherwise pass by having nothing to mask.
    pub fn mask(stdout: &str) -> Option<(String, usize)> {
        let mut index = None;
        let mut masking_detail = false;
        let mut out = Vec::new();

        for (line_number, line) in stdout.lines().enumerate() {
            match (index, label_of(line)) {
                // Only the first match is masked; a second row with the same
                // label would be a real difference worth seeing.
                (None, Some(_)) => {
                    index = Some(line_number);
                    out.push("  <TOOLCHAIN ROW>".to_owned());
                    // In the `--json` shape the value is a line of its own,
                    // and `r-orch 0.1.0 (rust 1.94.1)` against `v24.21.0` is
                    // the other half of the same divergence. The human row
                    // carries both on the line just masked, so this only ever
                    // consumes a `"detail"` that follows.
                    masking_detail = true;
                }
                _ if masking_detail && line.trim_start().starts_with("\"detail\": ") => {
                    masking_detail = false;
                    out.push("      \"detail\": \"<TOOLCHAIN>\"".to_owned());
                }
                _ => {
                    if line.trim_start().starts_with('}') {
                        masking_detail = false;
                    }
                    out.push(line.to_owned());
                }
            }
        }

        let index = index?;
        let mut masked = out.join("\n");
        if stdout.ends_with('\n') {
            masked.push('\n');
        }
        Some((masked, index))
    }

    /// The row's label, if the line carries one.
    ///
    /// Two shapes, because `doctor` prints the row twice over: the human
    /// `<glyph> <label> <value>` line, and — after the two header lines, which
    /// is why this is text and not a parsed document (§22.2) — the
    /// `"name": "<label>"` line of the `--json` object.
    fn label_of(line: &str) -> Option<&'static str> {
        let rest = line.trim_start();
        if let Some(name) = rest.strip_prefix("\"name\": \"") {
            let label = name
                .strip_suffix("\",")
                .or_else(|| name.strip_suffix('"'))?;
            return LABELS.into_iter().find(|known| *known == label);
        }
        // The glyph is ✓/✕ normally and +/x under --ascii; take whatever the
        // first whitespace-delimited token is and look at the one after it.
        let mut tokens = rest.split_whitespace();
        let _glyph = tokens.next()?;
        let label = tokens.next()?;
        LABELS.into_iter().find(|known| *known == label)
    }
}

/// What differed about a step's output: a label for the message, and the diff.
type StepDiff = Option<(&'static str, String)>;

/// Strip `state.claimed` from a `status --json` document, returning the rest
/// of the document and the value taken out.
///
/// Masking is done on the parsed document, which is what lets one path be
/// removed; `serde_json` is built with `preserve_order`, so key order still
/// survives a round trip, and the caller compares the raw framing separately.
/// Normalization is deliberately left to the caller: a differential run needs
/// one normalizer per tree (the two mint different ids), while a cross-compat
/// run must use the *same* one for both sides, because there is only one tree
/// and therefore only one set of ids.
fn mask_claimed(stdout: &str, side: &str) -> std::result::Result<(Value, Value), String> {
    let mut document: Value = serde_json::from_str(stdout)
        .map_err(|err| format!("{side}'s `status --json` is not valid JSON: {err}"))?;

    // A document with nothing to mask would pass vacuously, so the absence of
    // the path is a failure rather than a no-op.
    let claimed = claimed_mask::take(&mut document).ok_or_else(|| {
        format!(
            "{side}'s `status --json` has no `{}` to mask — the field is contract (§6.6), \
             and masking a path that is missing would assert nothing. If the *oracle* \
             stopped emitting it, re-read §14.1 before touching the mask",
            claimed_mask::PATH
        )
    })?;
    Ok((document, claimed))
}

/// Compare two `status --json` documents with `state.claimed` masked out,
/// returning r-orch's masked value and any difference in the rest.
fn compare_status_json(
    rust_stdout: &str,
    ts_stdout: &str,
    mut normalize: impl FnMut(&Value) -> Value,
) -> std::result::Result<(Value, StepDiff), String> {
    let (rust_doc, rust_claimed) = mask_claimed(rust_stdout, "r-orch")?;
    let (ts_doc, _) = mask_claimed(ts_stdout, "the oracle")?;

    let (rust_framing, ts_framing) = (JsonFraming::of(rust_stdout), JsonFraming::of(ts_stdout));
    if rust_framing != ts_framing {
        return Ok((
            rust_claimed,
            Some((
                "status --json framing",
                format!(
                    "2-space indent plus a trailing newline is contract (§6.4)\n    ts:   \
                     {ts_framing:?}\n    rust: {rust_framing:?}"
                ),
            )),
        ));
    }

    let left = normalize(&rust_doc);
    let right = normalize(&ts_doc);
    let diff = (left != right).then(|| {
        (
            "status --json (state.claimed masked, §14.1)",
            unified_diff(&pretty(&right), &pretty(&left)),
        )
    });
    Ok((rust_claimed, diff))
}

/// The replacement assertion for the §14.1 mask: r-orch's `state.claimed` must
/// equal the `claimed` array in the `state.json` it wrote.
///
/// Without this the mask would be a hole. With it, the field is still pinned —
/// just against r-orch's own on-disk state, which the tree walk has already
/// compared against the oracle's, rather than against a `status --json`
/// document that is empty by construction on the other side.
fn claimed_matches_state_file(masked: &Option<Value>, rust_tree: &Tree) -> Vec<String> {
    let Some(claimed) = masked else {
        return Vec::new();
    };

    // `init` does not write `state.json` — the orchestrator creates it on its
    // first write, and until then `StateStore::read` returns the defaults.
    // So its absence is not a hole: it means the only correct report is the
    // empty set, and that is still asserted.
    let Some(state) = rust_tree.values.get("state.json") else {
        if *claimed == serde_json::json!([]) {
            return Vec::new();
        }
        return vec![format!(
            "`{}` in `status --json` is {claimed}, but the run left no \
             `.orchestry/state.json`, so the default empty set is what it should have \
             reported (PARITY.md §14.1)",
            claimed_mask::PATH
        )];
    };
    let on_disk = &state["claimed"];

    if !claimed.is_array() {
        return vec![format!(
            "`status --json` must emit `{}` as an array (PARITY.md §14.1, decided FIX), \
             got {claimed}",
            claimed_mask::PATH
        )];
    }
    if claimed != on_disk {
        return vec![format!(
            "`{}` in `status --json` is {claimed}, but `.orchestry/state.json` holds \
             {on_disk} — the fix must report the same ids the state file does (§14.1)",
            claimed_mask::PATH
        )];
    }
    Vec::new()
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

fn compare_trees(rust: &Tree, ts: &Tree, unordered_files: &[String]) -> Vec<String> {
    let mut failures = compare_values(&rust.values, &ts.values, unordered_files);

    // A file only one side could parse is the sharpest possible signal: the
    // two implementations do not even agree that it is well-formed.
    for (path, why) in &ts.unparseable {
        if !rust.unparseable.contains_key(path) {
            failures.push(format!(
                "`.orchestry/{path}`: the oracle wrote a file our parser rejects ({why}) \
                 while r-orch's own file parses — the two are not interchangeable, whatever \
                 the values compare as"
            ));
        }
    }
    for (path, why) in &rust.unparseable {
        if !ts.unparseable.contains_key(path) {
            failures.push(format!(
                "`.orchestry/{path}`: r-orch wrote a file our parser rejects ({why}) while \
                 the oracle's parses"
            ));
        }
    }

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

fn compare_values(
    rust: &BTreeMap<String, Value>,
    ts: &BTreeMap<String, Value>,
    unordered_files: &[String],
) -> Vec<String> {
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
        let unordered = unordered_files.iter().any(|named| named == path);
        if let Some(rust_value) = rust.get(path).map(|value| as_compared(value, unordered))
            && rust_value != as_compared(ts_value, unordered)
        {
            let ts_value = &as_compared(ts_value, unordered);
            let rust_value = &rust_value;
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
fn cross_parse_failures(
    bins: &Binaries,
    project: &Project,
    scenario: &Scenario,
) -> Result<Vec<String>> {
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
    // Which `unparseable_files` declarations actually matched an unreadable
    // file. A declaration that matches nothing is stale and fails.
    let mut satisfied: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (path, relative) in absolute.iter().zip(&yaml) {
        let Some(js) = parsed.get(path) else {
            failures.push(format!(
                "js-yaml did not report on `{}`",
                relative.display()
            ));
            continue;
        };
        if !js.ok {
            // Declared as deliberately corrupt? Then being unreadable is the
            // assertion, not a failure — and it is recorded as satisfied so a
            // declaration that stops matching anything fails below.
            let relative_str = relative.to_string_lossy().replace('\\', "/");
            if let Some(prefix) = scenario
                .unparseable_files
                .iter()
                .find(|prefix| relative_str.starts_with(prefix.as_str()))
            {
                satisfied.insert(prefix.clone());
                continue;
            }
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

    for prefix in &scenario.unparseable_files {
        if !satisfied.contains(prefix) {
            failures.push(format!(
                "`unparseable_files` lists `{prefix}`, but every YAML file matching it parsed \
                 cleanly. The corruption this scenario is built on did not happen, so the \
                 masked steps are asserting nothing — fix the `@write`, or drop the entry."
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
/// A value as it is compared: identical, unless the caller declared its
/// top-level array order non-contract, in which case the elements are sorted
/// canonically so the two sides are compared as multisets.
///
/// Sorting by the rendered element is safe because both sides are normalized
/// first — ids are placeholders assigned per entity, in the same order on both
/// sides — so equal multisets sort to equal sequences.
fn as_compared(value: &Value, unordered: bool) -> Value {
    if !unordered {
        return value.clone();
    }
    match value {
        Value::Array(items) => {
            let mut sorted: Vec<Value> = items.iter().map(|item| as_compared(item, true)).collect();
            // Keyed on a *key-sorted* rendering: two sides that agree on the
            // pairs but not their order must sort to the same sequence, or
            // this would invent a difference instead of ignoring one.
            sorted.sort_by_cached_key(|item| pretty(&with_sorted_keys(item)));
            Value::Array(sorted)
        }
        // Recursive, because the array in question is not always the whole
        // document: `status --json` carries its tasks and agents under
        // `tasks`/`agents`, and those are the arrays whose order a tie in the
        // store's sort leaves undefined.
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), as_compared(value, true)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The same document with every object's keys in sorted order, so it renders
/// identically whatever insertion order produced it.
fn with_sorted_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by_key(|(key, _)| *key);
            Value::Object(
                pairs
                    .into_iter()
                    .map(|(key, value)| (key.clone(), with_sorted_keys(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(with_sorted_keys).collect()),
        other => other.clone(),
    }
}

/// Both stdouts parsed, when **both** are exactly one JSON object or array —
/// which is what every `--json` command prints.
///
/// PARITY.md §1.4: JSON object key order is **not** contract. TS emits JS
/// insertion order, which depends on when a field was first assigned — a goal
/// that gains an `assignee` after creation emits it last, after `updated_at`,
/// and `Message` emits `id`/`to_agent_id` last after an object spread — while
/// serde emits declaration order. So these are compared as parsed values
/// (`serde_json::Map` is an `IndexMap` under `preserve_order`, whose `Eq`
/// ignores order) plus the framing that *is* contract, and never as text.
///
/// A scalar (`null`, a bare number) and anything with surrounding human text
/// fall through to the text comparison: key order only exists inside a
/// container, and text is the stricter test.
fn json_documents(rust_stdout: &str, ts_stdout: &str) -> Option<(Value, Value)> {
    Some((json_document(rust_stdout)?, json_document(ts_stdout)?))
}

fn json_document(stdout: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(stdout.trim()) {
        Ok(value @ (Value::Object(_) | Value::Array(_))) => Some(value),
        _ => None,
    }
}

/// The byte-level framing both sides must share even where key order is free
/// (§6.4): 2-space indent and a trailing newline, or neither for a compact
/// document.
fn json_framing_diff(rust_stdout: &str, ts_stdout: &str) -> Option<(&'static str, String)> {
    let (rust_framing, ts_framing) = (JsonFraming::of(rust_stdout), JsonFraming::of(ts_stdout));
    (rust_framing != ts_framing).then(|| {
        (
            "json framing",
            format!(
                "indent and trailing newline are contract (§6.4)\n    ts:   \
                 {ts_framing:?}\n    rust: {rust_framing:?}"
            ),
        )
    })
}

fn json_records(stdout: &str, norm: &mut Normalizer, drop_ticks: bool) -> Vec<Value> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| match serde_json::from_str::<Value>(line) {
            Ok(value) => mask_heap_mb(norm.value(&value)),
            Err(_) => Value::String(norm.text(line)),
        })
        .filter(|record| !(drop_ticks && is_tick(record)))
        .collect()
}

fn is_tick(record: &Value) -> bool {
    record.get("event").and_then(Value::as_str) == Some("orchestrator:tick")
}

/// `orchestrator:tick`'s `heap_mb` is a memory measurement of the runtime that
/// produced it — `process.memoryUsage().heapUsed / 1 MiB` in the oracle, which
/// has no counterpart in a Rust process at all. Masked like `<PID>` and `<MS>`
/// are (§1.4): the field's presence and position are contract, its value is a
/// property of V8.
fn mask_heap_mb(mut record: Value) -> Value {
    if let Some(object) = record.as_object_mut()
        && let Some(heap) = object.get_mut("heap_mb")
        && heap.is_number()
    {
        *heap = Value::String("<HEAP_MB>".to_owned());
    }
    record
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

/// Everything stderr keeps under `@stderr-prefix-only`: the `  ✕ ` marker on
/// the first line, and nothing after it.
///
/// Deliberately not "the first line" — TS's first line already carries
/// js-yaml's reason and a `(line:col)`, which is the part that cannot match.
fn stderr_error_prefix(stderr: &str) -> String {
    let first = stderr.lines().next().unwrap_or_default();
    match first.find('\u{2715}') {
        Some(at) => first[..at + '\u{2715}'.len_utf8()].to_string(),
        // No marker at all is itself a difference worth failing on: one side
        // reported an error and the other did not.
        None => String::new(),
    }
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
    fn the_fake_agents_are_always_on_the_path_and_editors_on_request() {
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

        // Unconditional: `doctor` probes PATH and the adapters spawn from it,
        // so inheriting the developer's PATH would compare two runs against
        // whatever that machine has installed.
        let plain = scenario_with("name = \"s\"\nkind = \"differential\"\nsteps = [\"a\"]\n");
        let path = stand_in_path(&bins, &plain).unwrap();
        assert!(path.contains("fake-agents"), "{path}");
        assert!(!path.contains("fake-editors"), "{path}");

        let both = scenario_with(
            "name = \"s\"\nkind = \"differential\"\n\
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
            timed_out: false,
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

        let failures = compare_values(&rust, &ts, &[]);
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
        assert!(compare_values(&tree, &tree, &[]).is_empty());
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

        let failures = compare_trees(&rust, &ts, &[]);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("JSON framing"), "{}", failures[0]);
    }

    #[test]
    fn a_named_tree_exclusion_drops_the_path_from_both_sides() {
        let scenario = scenario_with(
            "name = \"s\"\nkind = \"differential\"\n\
             excluded_tree_paths = [\"messages/\"]\nsteps = [\"a\"]\n",
        );
        let mut rust = Tree::default();
        let mut ts = Tree::default();
        for tree in [&mut rust, &mut ts] {
            tree.values
                .insert("tasks/tsk_1.yml".to_owned(), Value::from(1));
        }
        // The excluded path differs, which without the exclusion would fail.
        rust.values
            .insert("messages/msg_1.json".to_owned(), Value::from("rust"));
        ts.values
            .insert("messages/msg_1.json".to_owned(), Value::from("ts"));

        let failures = exclude_tree_paths(&scenario, &mut rust, &mut ts);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(compare_trees(&rust, &ts, &[]).is_empty());
        // Everything outside the exclusion is still there to be compared.
        assert!(rust.values.contains_key("tasks/tsk_1.yml"));
        assert!(!rust.values.contains_key("messages/msg_1.json"));
    }

    #[test]
    fn a_stale_tree_exclusion_fails_rather_than_passing_quietly() {
        // An exclusion matching nothing is a comparison nobody is doing any
        // more for a reason nobody remembers.
        let scenario = scenario_with(
            "name = \"s\"\nkind = \"differential\"\n\
             excluded_tree_paths = [\"runs/\"]\nsteps = [\"a\"]\n",
        );
        let mut rust = Tree::default();
        let mut ts = Tree::default();
        rust.values
            .insert("tasks/tsk_1.yml".to_owned(), Value::from(1));
        ts.values
            .insert("tasks/tsk_1.yml".to_owned(), Value::from(1));

        let failures = exclude_tree_paths(&scenario, &mut rust, &mut ts);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("matches nothing"), "{}", failures[0]);
    }

    #[test]
    fn a_scenario_without_exclusions_compares_everything() {
        let scenario = scenario_with("name = \"s\"\nkind = \"differential\"\nsteps = [\"a\"]\n");
        let mut rust = Tree::default();
        let mut ts = Tree::default();
        rust.values
            .insert("messages/msg_1.json".to_owned(), Value::from("rust"));
        ts.values
            .insert("messages/msg_1.json".to_owned(), Value::from("ts"));

        assert!(exclude_tree_paths(&scenario, &mut rust, &mut ts).is_empty());
        assert_eq!(compare_trees(&rust, &ts, &[]).len(), 1);
    }

    #[test]
    fn the_toolchain_mask_applies_only_to_doctor() {
        for step in ["doctor", "doctor --json", "--ascii doctor"] {
            let argv = shell_words::split(step).unwrap();
            assert!(toolchain_mask::applies_to(&argv), "{step}");
        }
        for step in [
            "status --json",
            "task list",
            "agent list --json",
            "serve --once",
        ] {
            let argv = shell_words::split(step).unwrap();
            assert!(!toolchain_mask::applies_to(&argv), "{step}");
        }
    }

    #[test]
    fn the_toolchain_row_is_masked_in_place_and_nothing_else_is() {
        // Shaped like the real output, measured from the oracle.
        let ts = concat!(
            "\n  orch doctor · checking adapters and dependencies\n\n",
            "  ✓  claude       2.1.296 (Claude Code)\n",
            "  ✓  git          git version 2.47.3\n",
            "  ✓  node         v24.21.0\n",
            "\n  8 of 8 adapters ready\n"
        );
        let rust = ts.replace(
            "  ✓  node         v24.21.0",
            "  ✓  toolchain    rustc 1.94.1",
        );

        let (ts_masked, ts_row) = toolchain_mask::mask(ts).expect("ts row");
        let (rust_masked, rust_row) = toolchain_mask::mask(&rust).expect("rust row");

        // Same position, and the differing row is gone from both.
        assert_eq!(ts_row, rust_row);
        assert_eq!(ts_masked, rust_masked);
        assert!(ts_masked.contains("<TOOLCHAIN ROW>"));
        // Everything around it survives, so a real divergence still shows.
        assert!(ts_masked.contains("git version 2.47.3"));
        assert!(ts_masked.contains("8 of 8 adapters ready"));
        assert!(!ts_masked.contains("v24.21.0"));
        assert!(!rust_masked.contains("rustc 1.94.1"));
    }

    #[test]
    fn a_doctor_output_with_no_toolchain_row_masks_nothing() {
        // Would otherwise pass vacuously; the caller turns this into a failure.
        assert!(toolchain_mask::mask("  ✓  git  git version 2.47.3\n").is_none());
        assert!(toolchain_mask::mask("").is_none());
    }

    #[test]
    fn the_toolchain_mask_survives_the_ascii_glyphs() {
        // --ascii swaps ✓ for +, so matching on the glyph itself would make
        // the mask silently stop applying under one flag.
        let (masked, row) = toolchain_mask::mask("  +  node         v24.21.0\n").expect("row");
        assert_eq!(row, 0);
        assert!(masked.contains("<TOOLCHAIN ROW>"));
    }

    #[test]
    fn only_the_first_toolchain_row_is_masked() {
        // A second row with the same label is a real difference, not another
        // licensed exclusion.
        let two = "  ✓  node  v1\n  ✓  node  v2\n";
        let (masked, _) = toolchain_mask::mask(two).expect("row");
        assert!(masked.contains("<TOOLCHAIN ROW>"));
        assert!(masked.contains("v2"), "{masked}");
    }

    #[test]
    fn the_claimed_mask_applies_only_to_status_json() {
        let yes = [
            "status --json",
            "status --json --ascii",
            "--ascii status --json",
        ];
        let no = [
            // No `--json`: the human renderer never prints the raw Set.
            "org deploy",
            "org list",
            "run",
            "serve",
            "status",
            // Other commands also carry `state`-shaped output; the exclusion
            // §14.1 grants is for `status --json` and nothing else.
            "task list --json",
            "agent list --json",
            "config get scheduling --json",
            "doctor --json",
            "serve --once --log-format json",
        ];
        for step in yes {
            let argv = shell_words::split(step).unwrap();
            assert!(claimed_mask::applies_to(&argv), "{step}");
        }
        for step in no {
            let argv = shell_words::split(step).unwrap();
            assert!(!claimed_mask::applies_to(&argv), "{step}");
        }
    }

    #[test]
    fn the_mask_removes_exactly_one_path() {
        let mut doc: Value = serde_json::from_str(
            r#"{"tasks":[],"agents":[],"state":{"version":1,"claimed":["tsk_a"],"running":{}}}"#,
        )
        .unwrap();

        let taken = claimed_mask::take(&mut doc).expect("claimed present");
        assert_eq!(taken, serde_json::json!(["tsk_a"]));
        // Everything else survives, `running` included — it has the same JS
        // Map problem but §14.1 does not cover it, so it stays compared.
        assert_eq!(doc["state"]["version"], 1);
        assert_eq!(doc["state"]["running"], serde_json::json!({}));
        assert!(doc["state"].get("claimed").is_none());
        assert!(doc.get("tasks").is_some());

        // A second take finds nothing, which is how a `status --json` that
        // stopped emitting the field is caught instead of passing vacuously.
        assert!(claimed_mask::take(&mut doc).is_none());
    }

    #[test]
    fn masking_lets_the_set_and_the_array_through_but_nothing_else() {
        let ts = "{\n  \"state\": {\n    \"claimed\": {},\n    \"version\": 1\n  }\n}\n";
        let rust = "{\n  \"state\": {\n    \"claimed\": [\n      \"tsk_a\"\n    ],\n    \"version\": 1\n  }\n}\n";
        let mut norm = Normalizer::new();

        let (claimed, diff) = compare_status_json(rust, ts, |value| norm.value(value)).unwrap();
        assert_eq!(claimed, serde_json::json!(["tsk_a"]));
        assert!(diff.is_none(), "{diff:?}");

        // A difference anywhere else in the same document still fails.
        let changed = rust.replace("\"version\": 1", "\"version\": 2");
        let (_, diff) = compare_status_json(&changed, ts, |value| norm.value(value)).unwrap();
        assert!(
            diff.is_some(),
            "a non-masked field diverged and was not reported"
        );
    }

    #[test]
    fn status_json_framing_is_still_compared_under_the_mask() {
        // Masking works on the parsed document, so the framing that the raw
        // text comparison used to cover (§6.4) has to be checked explicitly
        // or it is silently lost.
        let ts = "{\n  \"state\": {\n    \"claimed\": {}\n  }\n}\n";
        let no_newline = "{\n  \"state\": {\n    \"claimed\": []\n  }\n}";
        let mut norm = Normalizer::new();

        let (_, diff) = compare_status_json(no_newline, ts, |value| norm.value(value)).unwrap();
        let (what, _) = diff.expect("a dropped trailing newline must be reported");
        assert_eq!(what, "status --json framing");
    }

    #[test]
    fn a_status_json_without_claimed_is_a_failure_not_a_free_pass() {
        let no_claimed = "{\n  \"state\": {\n    \"version\": 1\n  }\n}\n";
        let err =
            mask_claimed(no_claimed, "r-orch").expect_err("a missing field must not mask silently");
        assert!(err.contains("no `state.claimed` to mask"), "{err}");

        // The message names the side, so "the oracle changed" stays
        // distinguishable from "we regressed" — they need opposite fixes.
        let err = mask_claimed("{}", "the oracle").expect_err("missing");
        assert!(err.contains("the oracle's"), "{err}");
        assert!(err.contains("re-read §14.1"), "{err}");
    }

    #[test]
    fn the_masked_value_is_re_asserted_against_state_json() {
        let mut tree = Tree::default();
        tree.values.insert(
            "state.json".to_owned(),
            serde_json::json!({ "claimed": ["tsk_a", "tsk_b"] }),
        );

        // Agreeing with the state file is the pass.
        let ok = Some(serde_json::json!(["tsk_a", "tsk_b"]));
        assert!(claimed_matches_state_file(&ok, &tree).is_empty());

        // Disagreeing is caught — this is the whole point of the replacement
        // assertion, since the oracle's own value is empty by construction.
        let wrong = Some(serde_json::json!(["tsk_a"]));
        let failures = claimed_matches_state_file(&wrong, &tree);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("state.json"), "{}", failures[0]);

        // Still an object, i.e. the fix was never applied.
        let unfixed = Some(serde_json::json!({}));
        let failures = claimed_matches_state_file(&unfixed, &tree);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("must emit"), "{}", failures[0]);

        // Masked with no state.json to check against. `init` does not write
        // one, so this is the common case rather than a hole: the only correct
        // report is the empty set, and that is still asserted.
        assert!(
            claimed_matches_state_file(&Some(serde_json::json!([])), &Tree::default()).is_empty()
        );
        let failures = claimed_matches_state_file(&ok, &Tree::default());
        assert_eq!(failures.len(), 1);
        assert!(
            failures[0].contains("left no `.orchestry/state.json`"),
            "{}",
            failures[0]
        );

        // No `status --json` in the scenario means nothing to re-assert.
        assert!(claimed_matches_state_file(&None, &tree).is_empty());
    }

    #[test]
    fn a_file_only_one_side_can_parse_is_reported_not_crashed_on() {
        // The real case: a message body truncated mid-surrogate-pair is
        // written by the oracle as the escape `\ud83d`, which serde_json
        // rejects outright. Aborting the run would turn the divergence under
        // test into a harness error.
        let mut rust = Tree::default();
        rust.values
            .insert("messages/m.json".to_owned(), Value::Null);
        let mut ts = Tree::default();
        ts.values.insert("messages/m.json".to_owned(), Value::Null);
        ts.unparseable.insert(
            "messages/m.json".to_owned(),
            "unexpected end of hex escape at line 1 column 19".to_owned(),
        );

        let failures = compare_trees(&rust, &ts, &[]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].contains("our parser rejects"),
            "{}",
            failures[0]
        );
        assert!(failures[0].contains("hex escape"), "{}", failures[0]);

        // Both sides unparseable in the same way is not a divergence; the raw
        // text comparison still covers the content.
        let mut both = rust.clone();
        both.unparseable = ts.unparseable.clone();
        assert!(compare_trees(&both, &ts, &[]).is_empty());
    }

    #[test]
    fn parse_by_extension_reports_a_lone_surrogate_rather_than_accepting_it() {
        let err = parse_by_extension(Path::new("m.json"), r#"{"body":"aaa\ud83d"}"#)
            .expect_err("serde_json must reject a lone surrogate escape");
        assert!(format!("{err}").contains("hex escape"), "{err}");

        // A whole pair is fine, so the rejection is specific to the split.
        let ok = parse_by_extension(Path::new("m.json"), "{\"body\":\"\u{1F600}\"}").unwrap();
        assert_eq!(ok["body"], Value::String("\u{1F600}".to_owned()));
    }

    #[test]
    fn the_diff_labels_which_side_is_which() {
        let diff = unified_diff("expected\n", "actual\n");
        assert!(diff.contains("--- ts (oracle)"));
        assert!(diff.contains("+++ rust"));
        assert!(diff.contains("-expected"));
        assert!(diff.contains("+actual"));
    }

    #[test]
    fn a_json_document_compares_regardless_of_key_order() {
        let rust = "{\n  \"assignee\": \"agt_1\",\n  \"updated_at\": \"x\"\n}\n";
        let ts = "{\n  \"updated_at\": \"x\",\n  \"assignee\": \"agt_1\"\n}\n";
        // §1.4: the same pairs in a different order are the same document.
        assert_eq!(json_document(rust), json_document(ts));
        // …and the framing both sides must still share is unchanged by it.
        assert_eq!(JsonFraming::of(rust), JsonFraming::of(ts));
    }

    #[test]
    fn a_json_document_still_notices_a_changed_value_or_framing() {
        let base = "{\n  \"a\": 1,\n  \"b\": 2\n}\n";
        assert_ne!(
            json_document(base),
            json_document("{\n  \"b\": 2,\n  \"a\": 9\n}\n")
        );
        assert_ne!(json_document(base), json_document("{\n  \"a\": 1\n}\n"));
        // A compact document has no indent, so the framing check catches it
        // even though the parsed values match.
        let compact = "{\"a\":1,\"b\":2}\n";
        assert_eq!(json_document(base), json_document(compact));
        assert_ne!(JsonFraming::of(base), JsonFraming::of(compact));
    }

    #[test]
    fn only_a_container_takes_the_json_path() {
        assert!(json_document("null\n").is_none());
        assert!(json_document("5\n").is_none());
        assert!(json_document("  key = 5\n").is_none());
        assert!(json_document("{\"a\":1}\ntrailing\n").is_none());
        assert!(json_document("[]\n").is_some());
    }
}
