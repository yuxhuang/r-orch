//! Independent folded-suite engine. Production tiers call this only after a
//! supported native target admission; fixtures exercise the identical judgments.
use super::receipt::progress as report_progress;
use super::{
    hermetic::Hermetic,
    inventory, nextest,
    report::{self, Step, Verdict},
    steps::{LogFailure, Runner},
};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub(super) struct Suite {
    pub steps: Vec<Step>,
    pub logs: BTreeMap<String, Vec<u8>>,
    pub inventory: String,
}
impl Suite {
    fn checked(&mut self, name: &'static str, start: Instant, result: Result<String>) {
        let (verdict, detail) = match result {
            Ok(s) => (Verdict::Pass, s),
            Err(e) => (Verdict::Fail, format!("{e:#}")),
        };
        self.logs
            .insert(format!("steps/{name}.log"), detail.as_bytes().to_vec());
        self.steps.push(Step {
            name,
            verdict,
            exit: Some(if verdict == Verdict::Pass { 0 } else { 1 }),
            elapsed: start.elapsed(),
            detail,
        });
    }
}

pub(super) fn run(repo: &Path, hermetic: &Hermetic, runner: &mut Runner<'_, '_>) -> Suite {
    run_scoped(repo, hermetic, runner, None)
}

/// Resolve the one suite runner before anything is executed. There is no other
/// runner to fall back to: a missing or unpinned nextest fails the gate closed
/// with an install hint, and the rollback is to revert the change that adopted
/// it (ADR 0022).
fn resolve(hermetic: &Hermetic, runner: &mut Runner<'_, '_>) -> Result<String> {
    ensure!(
        hermetic.available("cargo-nextest"),
        "cargo-nextest is not on the suite PATH; {}",
        nextest::install_hint()
    );
    let out = runner.capture_raw(
        hermetic.program("cargo-nextest").arg("--version"),
        16 * 1024,
    )?;
    ensure!(
        out.exit.success(),
        "cargo-nextest --version exited {}; {}",
        report::code(out.exit),
        nextest::install_hint()
    );
    nextest::pinned(&String::from_utf8_lossy(&out.stdout))
}

fn scope(
    command: &mut std::process::Command,
    selection: Option<&crate::gate_selection::Selection>,
) {
    match selection {
        Some(s) if s.mode == crate::gate_selection::Mode::Packages => {
            for package in &s.packages {
                command.arg("-p").arg(package);
            }
        }
        _ => {
            command.arg("--workspace");
        }
    }
}

/// A collected suite run: the child's status, the log the inventory is rebuilt
/// from, and whether collection itself succeeded.
type Collected = (Option<i32>, Vec<u8>, Result<()>);

fn collect_logged(
    runner: &mut Runner<'_, '_>,
    command: &mut Command,
    index: usize,
) -> (Option<i32>, Vec<u8>, Result<()>) {
    match runner.logged(command, inventory::MAX_LOG) {
        Ok(log) => (Some(report::code(log.exit)), log.bytes, Ok(())),
        Err(e) => {
            let bytes = e
                .downcast_ref::<LogFailure>()
                .map_or_else(Vec::new, |f| f.bytes.clone());
            let exit = runner.statuses().get(index).map(|s| report::code(s.exit));
            (exit, bytes, Err(e))
        }
    }
}

/// The suite: one `cargo nextest run` per pass in [`nextest::PROFILES`], whose
/// isolation and split both come from `.config/nextest.toml`, then the
/// doc-tests nextest does not run, framed into one log. A runner or doc-test
/// failure to collect is never a pass: the frame keeps every status and an
/// unknown one refuses on read-back, and a pass that was never attempted keeps
/// its `null`.
fn nextest_suite(
    repo: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
    selection: Option<&crate::gate_selection::Selection>,
    map: &[u8],
) -> Collected {
    let mut passes = Vec::new();
    let mut collected = Ok(());
    for profile in nextest::PROFILES {
        if collected.is_err() {
            // A pass that was never attempted has no status; read-back refuses
            // the frame rather than judging a partial run.
            passes.push((None, Vec::new(), Vec::new()));
            continue;
        }
        let mut run = hermetic.program("cargo-nextest");
        run.args(["nextest", "run", "--locked", "--manifest-path"])
            .arg(repo.join("Cargo.toml"))
            .args([
                "--no-fail-fast",
                "--user-config-file",
                "none",
                "--config-file",
            ])
            .arg(repo.join(".config/nextest.toml"))
            .args([
                // The profile is pinned on the command line: nothing filters
                // NEXTEST_* out of the suite environment, and an inherited
                // NEXTEST_PROFILE would otherwise select different isolation rules
                // from the same configuration file. Every other setting that could
                // change a judgement is passed here too, and a flag beats the
                // environment. It also selects the pass: each profile's
                // `default-filter` is one half of the split.
                "--profile",
                profile,
                // A passing test's output stays out of the evidence whatever the
                // profile says; the canary tests rest on this.
                "--success-output",
                "never",
                "--message-format",
                "libtest-json-plus",
                "--message-format-version",
                nextest::FORMAT_VERSION,
                "--no-pager",
                "--no-input-handler",
                "--show-progress",
                "none",
                // The retained operator log names every test, whatever the
                // profile says; the inventory reads the stream, not this log.
                "--status-level",
                "all",
                // Not left to the configuration or to a default: a retry is
                // refused by this repository's rules, and an empty run is a
                // failure. The inventory refuses the resulting exit anyway.
                "--retries",
                "0",
                "--no-tests",
                "fail",
            ])
            .env(nextest::EXPERIMENT.0, nextest::EXPERIMENT.1);
        scope(&mut run, selection);
        let index = runner.statuses().len();
        // Structured results on stdout, the operator log on stderr: a test's
        // own output can never reach the stream the inventory is read from.
        match runner.capture_split(&mut run, inventory::MAX_LOG, inventory::MAX_LOG) {
            Ok(out) => passes.push((Some(report::code(out.exit)), out.stdout, out.stderr)),
            Err(e) => {
                passes.push((
                    runner.statuses().get(index).map(|s| report::code(s.exit)),
                    Vec::new(),
                    Vec::new(),
                ));
                collected = Err(e);
            }
        }
    }
    let documented = std::str::from_utf8(map)
        .map_err(anyhow::Error::from)
        .and_then(inventory::documented);
    let (doc_exit, doc_log, doc_collected) = match (&collected, &documented) {
        (Ok(()), Ok(packages)) if packages.is_empty() => (Some(0), Vec::new(), Ok(())),
        (Ok(()), Ok(packages)) => {
            let mut doc = hermetic.command(repo);
            doc.args(["test", "--locked", "--manifest-path"])
                .arg(repo.join("Cargo.toml"))
                .args(["--doc", "--no-fail-fast"]);
            for package in packages {
                doc.arg("-p").arg(package);
            }
            let index = runner.statuses().len();
            collect_logged(runner, &mut doc, index)
        }
        (Ok(()), Err(e)) => (None, Vec::new(), Err(anyhow::anyhow!("{e:#}"))),
        (Err(_), _) => (None, Vec::new(), Ok(())),
    };
    let framed: Vec<nextest::Pass<'_>> = nextest::PROFILES
        .iter()
        .zip(&passes)
        .map(|(profile, (exit, events, stderr))| nextest::Pass {
            profile,
            exit: *exit,
            events,
            stderr,
        })
        .collect();
    let log = nextest::frame(&framed, doc_exit, &doc_log);
    // The step's status is the first non-zero of the passes and the doc-tests,
    // and unknown if any of them is unknown. A pass exit of 4 is "no test
    // matched this pass's filter", which a tier that selects a subset of
    // packages produces legitimately; whether anything was owed is the
    // inventory's judgement, against the unfiltered listing, so it is not a
    // failure here. A run where *nothing* executed still fails: the inventory
    // refuses with "no test executed".
    let combined = passes
        .iter()
        .map(|(exit, _, _)| exit.map(|code| if code == 4 { 0 } else { code }))
        .chain(std::iter::once(doc_exit))
        .try_fold(0, |acc, exit| match (acc, exit?) {
            (0, code) => Some(code),
            (code, _) => Some(code),
        });
    (combined, log, collected.and(doc_collected))
}

fn run_scoped(
    repo: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
    selection: Option<&crate::gate_selection::Selection>,
) -> Suite {
    let resolved = resolve(hermetic, runner);
    let mut out = Suite {
        steps: vec![],
        logs: BTreeMap::new(),
        inventory: String::new(),
    };
    if runner.receipt_progress {
        report_progress(super::receipt::Phase::BuildStarted);
    }
    let start = Instant::now();
    // Explicitly finish the workspace's public binaries before any test can
    // exec one by path. A pre-existing target/debug/orch is never that proof.
    let mut workspace = hermetic.command(repo);
    workspace
        .args(["build", "--locked", "--manifest-path"])
        .arg(repo.join("Cargo.toml"))
        .arg("--all-targets");
    scope(&mut workspace, selection);
    let workspace_start = runner.statuses().len();
    // Fail fast: a missing or unpinned runner is decided before anything is
    // built, so the refusal costs the operator a version check, not a whole
    // workspace build.
    let workspace_status = match &resolved {
        Ok(_) => runner.logged(&mut workspace, inventory::MAX_LOG),
        Err(e) => Err(anyhow::anyhow!("{e:#}")),
    };
    let mut build = hermetic.command(repo);
    build
        .args(["test", "--quiet", "--locked", "--manifest-path"])
        .arg(repo.join("Cargo.toml"))
        .args(["--no-run", "--message-format=json"]);
    scope(&mut build, selection);
    let map_start = runner.statuses().len();
    let mapped = match &workspace_status {
        Ok(log) if log.exit.success() => runner.capture_raw(&mut build, inventory::MAX_LOG),
        Ok(log) => Err(anyhow::anyhow!(
            "workspace build exited {}",
            report::code(log.exit)
        )),
        Err(e) => Err(anyhow::anyhow!("workspace build: {e:#}")),
    };
    let build_exit = if workspace_status
        .as_ref()
        .is_ok_and(|log| log.exit.success())
    {
        runner
            .statuses()
            .get(map_start)
            .map(|s| report::code(s.exit))
    } else {
        runner
            .statuses()
            .get(workspace_start)
            .map(|s| report::code(s.exit))
    };
    let mut built = resolved
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .and_then(|_| {
            let log = mapped.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?;
            ensure!(
                log.exit.success(),
                "test map compilation failed: child exit {}",
                report::code(log.exit)
            );
            Ok(log.stdout.clone())
        });
    // nextest's own listing is the authoritative set of tests its run must
    // account for, so it is produced here, with the suite's build, and kept in
    // the same build log a receipt verifier rebuilds the inventory from.
    let mut list_exit = None;
    if built.is_ok() {
        let mut list = hermetic.program("cargo-nextest");
        list.args(["nextest", "list", "--locked", "--manifest-path"])
            .arg(repo.join("Cargo.toml"))
            .args([
                // The listing is the complete expected set the two passes
                // are judged against together, so neither pass's half of the
                // split may apply to it: the profile is pinned for everything
                // else it carries, and its `default-filter` is ignored.
                "--profile",
                "default",
                nextest::UNFILTERED,
                "--message-format",
                "json",
                "--user-config-file",
                "none",
                "--config-file",
            ])
            .arg(repo.join(".config/nextest.toml"));
        scope(&mut list, selection);
        let index = runner.statuses().len();
        built = built.and_then(|map| {
            let out = runner.capture_split(&mut list, inventory::MAX_LOG, inventory::MAX_LOG)?;
            list_exit = Some(report::code(out.exit));
            ensure!(
                out.exit.success(),
                "nextest listing exited {}: {}",
                report::code(out.exit),
                report::diagnostic(&out.stderr)
            );
            let listing = String::from_utf8(out.stdout)?;
            let mut listing = listing.trim_end_matches('\n').to_owned();
            ensure!(
                !listing.is_empty() && !listing.contains('\n'),
                "nextest listing is not one JSON record"
            );
            listing.push('\n');
            Ok([map, listing.into_bytes()].concat())
        });
        if list_exit.is_none() {
            list_exit = runner.statuses().get(index).map(|s| report::code(s.exit));
        }
    }
    let build_exit = list_exit.filter(|code| *code != 0).or(build_exit);
    let (verdict, detail) = match &built {
        Ok(_) => (
            Verdict::Pass,
            format!(
                "test executable map compiled; runner: {}",
                match &resolved {
                    Ok(version) => version.clone(),
                    Err(_) => "unresolved".into(),
                }
            ),
        ),
        Err(e) => (Verdict::Fail, format!("{e:#}")),
    };
    let workspace_log = match &workspace_status {
        Ok(log) => log.bytes.clone(),
        Err(e) => e
            .downcast_ref::<LogFailure>()
            .map_or_else(|| format!("{e:#}").into_bytes(), |f| f.bytes.clone()),
    };
    let mut build_log=serde_json::json!({"reason":"gate-build-log","text":String::from_utf8_lossy(&workspace_log)}).to_string().into_bytes();
    build_log.push(b'\n');
    if let Ok(mapped) = &mapped {
        build_log.extend(serde_json::json!({"reason":"gate-map-stderr","text":String::from_utf8_lossy(&mapped.stderr),"exit":report::code(mapped.exit)}).to_string().as_bytes());
        build_log.push(b'\n');
        // On success this is the cargo executable map plus nextest's listing,
        // which is exactly what the two inventory parsers read back.
        build_log.extend(built.as_ref().unwrap_or(&mapped.stdout));
        build_log.push(b'\n');
    }
    match &built {
        Ok(_) => {}
        Err(_) => build_log.extend(
            serde_json::json!({"reason":"gate-build-error","text":detail})
                .to_string()
                .as_bytes(),
        ),
    };
    out.logs.insert("steps/build.log".into(), build_log);
    out.steps.push(Step {
        name: "build",
        verdict,
        exit: build_exit,
        elapsed: start.elapsed(),
        detail,
    });
    if runner.receipt_progress {
        report_progress(super::receipt::Phase::BuildReturned);
        report_progress(super::receipt::Phase::TestStarted);
    }
    let start = Instant::now();
    let (exit, bytes, collection) = if built.is_err() {
        (
            None,
            Vec::new(),
            Err(anyhow::anyhow!("suite did not start: build failed")),
        )
    } else {
        let map = built.as_ref().map(Vec::clone).unwrap_or_default();
        nextest_suite(repo, hermetic, runner, selection, &map)
    };
    if runner.receipt_progress {
        report_progress(super::receipt::Phase::TestReturned);
    }
    out.logs.insert("steps/test.log".into(), bytes.clone());
    let judged = (|| -> Result<String> {
        collection?;
        let log = std::str::from_utf8(&bytes)?;
        let map = std::str::from_utf8(built.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?)?;
        let rows = nextest::inventory(log, map)?;
        out.inventory = inventory::tsv(&rows);
        let failed: Vec<_> = rows
            .iter()
            .filter(|r| r.status == "failed")
            .map(|r| format!("{}::{}", r.target, r.test))
            .collect();
        ensure!(
            exit == Some(0) && failed.is_empty(),
            "suite failed (child exit {:?}): {}",
            exit,
            failed.join(", ")
        );
        hermetic.witness()?;
        Ok(format!(
            "{} passed, 0 failed, {} ignored (same judged sentinel/shim suite)",
            rows.iter().filter(|r| r.status == "ok").count(),
            rows.iter().filter(|r| r.status == "ignored").count()
        ))
    })();
    let (verdict, detail) = match judged {
        Ok(s) => (Verdict::Pass, s),
        Err(e) => (Verdict::Fail, format!("{e:#}")),
    };
    out.steps.push(Step {
        name: "test",
        verdict,
        exit,
        elapsed: start.elapsed(),
        detail,
    });
    let start = Instant::now();
    out.checked("home-hygiene", start, hermetic.hygiene());
    let start = Instant::now();
    out.checked("no-real-cli", start, hermetic.cli());
    out
}

/// Complete step ordering, independently of target admission/publication. This
/// is not exposed as a way around the production native guard.
pub(super) fn run_all(
    repo: &Path,
    real_home: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
) -> Suite {
    run_selected(repo, real_home, hermetic, runner, None)
}

pub(super) fn run_selected(
    repo: &Path,
    real_home: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
    part: Option<super::Part>,
) -> Suite {
    let before = super::real_home::snapshot(real_home);
    if let Err(e) = &before {
        let mut suite = Suite {
            steps: vec![],
            logs: BTreeMap::new(),
            inventory: String::new(),
        };
        suite.checked(
            "real-home",
            Instant::now(),
            Err(anyhow::anyhow!("initial real-home snapshot: {e:#}")),
        );
        return suite;
    }
    let mut suite = if part == Some(super::Part::Static) {
        Suite {
            steps: vec![],
            logs: BTreeMap::new(),
            inventory: String::new(),
        }
    } else {
        run(repo, hermetic, runner)
    };
    suite.steps.truncate(2); // Re-evaluate hygiene after the static commands.
    if part != Some(super::Part::Suite) {
        static_steps(repo, hermetic, runner, &mut suite, false);
    }
    if part == Some(super::Part::Static) {
        return suite;
    }
    let start = Instant::now();
    suite.checked("home-hygiene", start, hermetic.hygiene());
    let start = Instant::now();
    suite.checked("no-real-cli", start, hermetic.cli());
    let start = Instant::now();
    suite.checked(
        "real-home",
        start,
        super::real_home::snapshot(real_home)
            .and_then(|after| super::real_home::compare(&before.unwrap(), &after))
            .map(|(text, _)| text),
    );
    suite
}

fn static_steps(
    repo: &Path,
    hermetic: &Hermetic,
    runner: &mut Runner<'_, '_>,
    suite: &mut Suite,
    fast: bool,
) {
    let names: &[&str] = if fast {
        &["fmt", "clippy", "crate-graph"]
    } else {
        &["clippy", "fmt", "advisories", "crate-graph"]
    };
    for &name in names {
        let start = Instant::now();
        if name == "advisories" && !hermetic.available("cargo-deny") {
            let detail="cargo-deny not installed; required advisories/licenses/bans/sources were not evaluated; cargo install cargo-deny --locked".to_owned();
            suite
                .logs
                .insert(format!("steps/{name}.log"), detail.as_bytes().to_vec());
            suite.steps.push(Step {
                name,
                verdict: Verdict::EnvLimited,
                exit: None,
                elapsed: start.elapsed(),
                detail,
            });
            continue;
        }
        let mut command = if name == "advisories" {
            hermetic.program("cargo-deny")
        } else {
            hermetic.command(repo)
        };
        match name {
            "clippy" => {
                command
                    .args(["clippy", "--locked", "--manifest-path"])
                    .arg(repo.join("Cargo.toml"))
                    .args(["--workspace", "--all-targets", "--", "-D", "warnings"]);
            }
            "fmt" => {
                command
                    .args(["fmt", "--manifest-path"])
                    .arg(repo.join("Cargo.toml"))
                    .args(["--all", "--check"]);
            }
            "advisories" => {
                command
                    .arg("--manifest-path")
                    .arg(repo.join("Cargo.toml"))
                    .args(["check", "advisories", "licenses", "bans", "sources"]);
            }
            "crate-graph" => {
                command
                    .current_dir(repo)
                    .args(["run", "--locked", "--quiet", "--manifest-path"])
                    .arg(repo.join("Cargo.toml"))
                    .args(["-p", "xtask", "--", "crate-graph"]);
            }
            _ => unreachable!(),
        }
        let index = runner.statuses().len();
        let result = runner.logged(&mut command, inventory::MAX_LOG);
        let (exit, bytes, verdict, detail) = match result {
            Ok(log) => {
                let exit = report::code(log.exit);
                let detail = if name == "fmt" {
                    format!(
                        "{} file(s) need reformatting",
                        String::from_utf8_lossy(&log.bytes)
                            .lines()
                            .filter(|s| s.starts_with("Diff in "))
                            .count()
                    )
                } else {
                    format!("child exit {exit}")
                };
                (
                    Some(exit),
                    log.bytes,
                    if exit == 0 {
                        Verdict::Pass
                    } else {
                        Verdict::Fail
                    },
                    detail,
                )
            }
            Err(e) => (
                runner.statuses().get(index).map(|s| report::code(s.exit)),
                e.downcast_ref::<LogFailure>()
                    .map_or_else(Vec::new, |f| f.bytes.clone()),
                Verdict::Fail,
                format!("{e:#}"),
            ),
        };
        suite.logs.insert(format!("steps/{name}.log"), bytes);
        suite.steps.push(Step {
            name,
            verdict,
            exit,
            elapsed: start.elapsed(),
            detail,
        });
    }
}

/// Local iteration uses the same native target, folded suite and judgments.
/// Documentation-only deliberately has no test or witness: it cannot qualify.
pub(super) fn fast_steps(selection: &crate::gate_selection::Selection) -> Vec<&'static str> {
    let mut steps = vec!["fmt", "clippy", "crate-graph"];
    if selection.mode == crate::gate_selection::Mode::DocsOnly {
        steps.push("build");
    } else {
        if needs_witness(selection) {
            steps.push("environment-witness");
        }
        steps.extend(["build", "test", "home-hygiene", "no-real-cli", "real-home"]);
    }
    steps
}
fn needs_witness(s: &crate::gate_selection::Selection) -> bool {
    s.mode == crate::gate_selection::Mode::Packages && !s.packages.iter().any(|p| p == "xtask")
}
pub(super) fn run_fast(
    repo: &Path,
    real_home: &Path,
    h: &Hermetic,
    runner: &mut Runner<'_, '_>,
    selection: &crate::gate_selection::Selection,
) -> Suite {
    let mut out = Suite {
        steps: vec![],
        logs: BTreeMap::new(),
        inventory: String::new(),
    };
    let before = super::real_home::snapshot(real_home);
    if let Err(e) = &before {
        out.checked(
            "real-home",
            Instant::now(),
            Err(anyhow::anyhow!("initial real-home snapshot: {e:#}")),
        );
        return out;
    }
    static_steps(repo, h, runner, &mut out, true);
    if selection.mode == crate::gate_selection::Mode::DocsOnly {
        let start = Instant::now();
        let mut command = h.command(repo);
        command
            .args(["build", "--locked", "--manifest-path"])
            .arg(repo.join("Cargo.toml"))
            .args(["--workspace", "--all-targets"]);
        let index = runner.statuses().len();
        let result = runner.logged(&mut command, inventory::MAX_LOG);
        let (bytes, judged) = match result {
            Ok(log) => (
                log.bytes,
                if log.exit.success() {
                    Ok("documentation-only build; zero suite tests".to_owned())
                } else {
                    Err(anyhow::anyhow!("build exited {}", report::code(log.exit)))
                },
            ),
            Err(e) => (
                e.downcast_ref::<LogFailure>()
                    .map_or_else(Vec::new, |f| f.bytes.clone()),
                Err(e),
            ),
        };
        out.checked("build", start, judged);
        out.steps.last_mut().unwrap().exit =
            runner.statuses().get(index).map(|s| report::code(s.exit));
        out.logs.insert("steps/build.log".into(), bytes);
    } else {
        // Most package closures exclude xtask. Run its witness target unfiltered,
        // including fixture-support tests, but not its entire package suite. Its independent inventory cannot turn an empty
        // selected suite into a nonzero successful one.
        if needs_witness(selection) {
            let start = Instant::now();
            let mut map = h.command(repo);
            map.args(["test", "--quiet", "--locked", "--manifest-path"])
                .arg(repo.join("Cargo.toml"))
                .args([
                    "-p",
                    "xtask",
                    "--test",
                    "gate_witness",
                    "--no-run",
                    "--message-format=json",
                ]);
            let mut bytes = Vec::new();
            let mut child_exit = None;
            let result = (|| -> Result<String> {
                let compiled = runner.capture_raw(&mut map, inventory::MAX_LOG)?;
                child_exit = Some(report::code(compiled.exit));
                out.logs
                    .insert("steps/witness-map.jsonl".into(), compiled.stdout.clone());
                out.logs
                    .insert("steps/witness-map-stderr.log".into(), compiled.stderr);
                ensure!(
                    compiled.exit.success(),
                    "environment witness compilation exited {}",
                    report::code(compiled.exit)
                );
                let mut command = h.command(repo);
                command
                    .args(["test", "--locked", "--manifest-path"])
                    .arg(repo.join("Cargo.toml"))
                    .args(["-p", "xtask", "--test", "gate_witness"])
                    // Only this command owns the record when xtask is unselected.
                    .env("XTASK_GATE_WITNESS_WRITER", "standalone");
                let log = runner.logged(&mut command, inventory::MAX_LOG)?;
                child_exit = Some(report::code(log.exit));
                bytes = log.bytes;
                let rows = inventory::parse(
                    std::str::from_utf8(&bytes)?,
                    std::str::from_utf8(&compiled.stdout)?,
                    report::code(log.exit),
                    inventory::Scope::All,
                )?;
                out.inventory = inventory::tsv(&rows);
                ensure!(
                    log.exit.success()
                        && rows.iter().all(|r| r.status == "ok")
                        && rows
                            .iter()
                            .filter(|r| r.test == "gate_environment_witness")
                            .count()
                            == 1,
                    "environment witness did not pass exactly once"
                );
                h.witness()?;
                Ok("one independently inventoried environment witness passed".into())
            })();
            if let Err(e) = &result
                && let Some(f) = e.downcast_ref::<LogFailure>()
            {
                bytes = f.bytes.clone();
            }
            out.checked("environment-witness", start, result);
            out.steps.last_mut().unwrap().exit = child_exit;
            out.logs
                .insert("steps/environment-witness.log".into(), bytes);
            if out.steps.last().unwrap().verdict != Verdict::Pass {
                return out;
            }
        }
        let mut selected = run_scoped(repo, h, runner, Some(selection));
        selected.steps.truncate(2); // Hygiene is judged after all selected commands.
        out.steps.extend(selected.steps);
        out.logs.extend(selected.logs);
        out.inventory.push_str(&selected.inventory);
        out.checked("home-hygiene", Instant::now(), h.hygiene());
        out.checked("no-real-cli", Instant::now(), h.cli());
        out.checked(
            "real-home",
            Instant::now(),
            super::real_home::snapshot(real_home)
                .and_then(|after| super::real_home::compare(&before.unwrap(), &after))
                .map(|(text, _)| text),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{
        files,
        lease::{Lease, Mode, Owned},
        steps::tests::Scratch,
    };
    use std::ffi::OsString;
    use std::os::unix::fs::MetadataExt;
    fn fixture(
        root: &Path,
        case: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, Hermetic, Lease) {
        let repo = root.join(case);
        std::fs::create_dir(&repo).unwrap();
        files::write(
            &repo.join("Cargo.toml"),
            b"[package]\nname=\"fold-fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[workspace]\n",
        )
        .unwrap();
        files::write(
            &repo.join("rust-toolchain.toml"),
            include_bytes!("../../../rust-toolchain.toml"),
        )
        .unwrap();
        files::write(
            &repo.join("Cargo.lock"),
            b"version = 4\n[[package]]\nname = \"fold-fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        // The gate passes `--config-file <repo>/.config/nextest.toml` and runs
        // one pass per profile, so even a one-file fixture repository needs
        // both. As strict as the real file: no retries, and the only filter is
        // the pass split itself, so nothing here can turn a planted failure or
        // an empty suite into a pass. No fixture has the alone pass's binary,
        // so that pass matches nothing and exits 4 — the same shape a tier
        // whose package selection leaves it out of scope produces, and the
        // inventory still owes a result for every listed test.
        std::fs::create_dir(repo.join(".config")).unwrap();
        files::write(
            &repo.join(".config/nextest.toml"),
            b"[profile.default]\nretries = 0\nfail-fast = false\nleak-timeout = { period = \"1s\", result = \"fail\" }\ndefault-filter = 'not test(=alone_fixture_nothing)'\n\n[profile.alone]\ninherits = \"default\"\ndefault-filter = 'test(=alone_fixture_nothing)'\n",
        )
        .unwrap();
        std::fs::create_dir(repo.join("src")).unwrap();
        let action = match case {
            "home" => {
                "std::fs::write(std::path::Path::new(&home).join(\"unexpected\"),b\"bad\").unwrap();"
            }
            "cli" => {
                "let status=std::process::Command::new(\"claude\").arg(\"--fixture\").status().unwrap();assert_eq!(status.code(),Some(127));"
            }
            "failure" => "panic!(\"folded-only fixture failure\");",
            _ => "",
        };
        let source = if case == "compile" {
            String::from("this is a planted Rust compilation error")
        } else if case == "zero" {
            String::from("pub fn no_tests() {}\n")
        } else {
            format!(
                r#"#[test] fn witness() {{
            let home=std::env::var("HOME").unwrap();
            let first=std::env::split_paths(&std::env::var_os("PATH").unwrap()).next().unwrap();
            let dir=std::env::var_os("XTASK_GATE_WITNESS_DIR").unwrap();
            let redirect=if std::env::var_os("ORCHESTRY_GLOBAL_CONFIG_PATH").is_some() {{"set"}} else {{"unset"}};
            std::fs::write(std::path::Path::new(&dir).join("witness.txt"),format!("schema=1\nhome={{home}}\nglobal_config_redirect={{redirect}}\npath_first={{}}\n",first.display())).unwrap();
            {action}
        }}"#
            )
        };
        files::write(&repo.join("src/lib.rs"), source.as_bytes()).unwrap();
        // Private homes use the already installed pinned toolchain, no install,
        // credentials, host cargo config or real HOME. Cargo needs no registry.
        let resources = root.join(format!("resources-{case}"));
        std::fs::create_dir(&resources).unwrap();
        let cargo = resources.join("cargo");
        let rustup = resources.join("rustup");
        let home = resources.join("home");
        for path in [&cargo, &rustup, &home] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::create_dir(rustup.join("toolchains")).unwrap();
        let output = std::process::Command::new("rustup")
            .args(["which", "rustc"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let compiler = std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        let toolchain = compiler.parent().unwrap().parent().unwrap();
        if case != "toolchain" {
            std::os::unix::fs::symlink(
                toolchain,
                rustup
                    .join("toolchains")
                    .join(toolchain.file_name().unwrap()),
            )
            .unwrap();
        }
        let rustup_bin = std::process::Command::new("/usr/bin/which")
            .arg("rustup")
            .output()
            .unwrap();
        assert!(rustup_bin.status.success());
        let proxy = std::path::PathBuf::from(String::from_utf8(rustup_bin.stdout).unwrap().trim())
            .parent()
            .unwrap()
            .to_path_buf();
        // The suite runs on the pinned cargo-nextest, so the fixture's own
        // PATH needs it. Absent is a setup failure with the install line.
        let nextest = std::process::Command::new("/usr/bin/which")
            .arg("cargo-nextest")
            .output()
            .unwrap();
        assert!(
            nextest.status.success(),
            "fixture setup: cargo-nextest missing from PATH; cargo install cargo-nextest --locked --version {}",
            super::nextest::VERSION
        );
        let nextest = std::path::PathBuf::from(String::from_utf8(nextest.stdout).unwrap().trim())
            .parent()
            .unwrap()
            .to_path_buf();
        let mut path = std::env::join_paths([
            proxy,
            nextest.clone(),
            toolchain.join("bin"),
            "/usr/bin".into(),
            "/bin".into(),
            "/usr/sbin".into(),
            "/sbin".into(),
        ])
        .unwrap();
        if case == "spawn" {
            // Only the runner, never cargo: the suite runner resolves, so the
            // build is still *attempted* and fails to spawn. That is what this
            // case proves — the gate never reuses an earlier child's status for
            // a build it could not start — and it would prove nothing if the
            // runner check refused first.
            let only_nextest = resources.join("only-nextest");
            std::fs::create_dir(&only_nextest).unwrap();
            std::os::unix::fs::symlink(
                nextest.join("cargo-nextest"),
                only_nextest.join("cargo-nextest"),
            )
            .unwrap();
            path = only_nextest.into_os_string();
        }
        let env: BTreeMap<OsString, OsString> = [
            ("HOME".into(), home.into_os_string()),
            ("PATH".into(), path),
            ("CARGO_HOME".into(), cargo.into_os_string()),
            ("RUSTUP_HOME".into(), rustup.into_os_string()),
        ]
        .into();
        let repo = repo.canonicalize().unwrap();
        let target = repo.join("target");
        let h = Hermetic::new(&repo, &target, root, env).unwrap();
        let locks = root.join(format!("locks-{case}"));
        std::fs::create_dir(&locks).unwrap();
        let owner = std::fs::metadata(&locks).unwrap().uid();
        let lease = Lease::Owned(
            Owned::acquire(&locks, owner, Mode::Exclusive, std::time::Duration::ZERO).unwrap(),
        );
        (repo, target, h, lease)
    }
    #[test]
    fn shared_witness_has_one_unit_writer_and_nonwriting_helpers() {
        use super::super::steps::tests::ChildGuard;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        // Regular private captures avoid pipe/EOF dependencies. Each direct
        // compiler/test child has a deadline and is reaped before assertions.
        fn child(h: &Hermetic, label: &str, command: &mut Command, success: bool) {
            let out = h.root.join(format!("{label}.out"));
            let err = h.root.join(format!("{label}.err"));
            files::write(&out, b"").unwrap();
            files::write(&err, b"").unwrap();
            let mut child = ChildGuard(
                command
                    .stdin(Stdio::null())
                    .stdout(std::fs::OpenOptions::new().write(true).open(&out).unwrap())
                    .stderr(std::fs::OpenOptions::new().write(true).open(&err).unwrap())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(30);
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= deadline {
                    child.0.kill().unwrap();
                    child.0.wait().unwrap();
                    panic!("{label}: direct child deadline");
                }
                std::thread::park_timeout(Duration::from_millis(10));
            };
            let text = format!(
                "{}{}",
                files::text(&out, 64 * 1024).unwrap(),
                files::text(&err, 64 * 1024).unwrap()
            );
            println!("{label}: {status}\n{text}");
            assert_eq!(status.success(), success, "{label}: {text}");
            if success && !label.starts_with("compile-") {
                assert!(text.contains("1 passed; 0 failed"), "{label}: {text}");
            }
        }

        let root = Scratch::new();
        let (_repo, _target, h, _lease) = fixture(&root.0, "ownership");
        let helper = h.root.join("gate_witness.rs");
        files::write(
            &helper,
            include_str!("../../tests/gate_witness.rs")
                .split("// FIXTURE SUPPORT:")
                .next()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let imported = h.root.join("imported.rs");
        files::write(&imported, b"#[path = \"gate_witness.rs\"] mod witness;\n").unwrap();
        for (label, source, package) in [
            ("standalone", &helper, "xtask"),
            ("imported", &imported, "xtask"),
            ("planted", &helper, "fixture"),
        ] {
            child(
                &h,
                &format!("compile-{label}"),
                h.program("rustc")
                    .env("CARGO_PKG_NAME", package)
                    .args(["--edition=2024", "--test", "--crate-name", label])
                    .arg(source)
                    .arg("-o")
                    .arg(h.root.join(label)),
                true,
            );
        }
        let record = h.witness.join("witness.txt");
        let helpers = [
            ("standalone", "gate_environment_witness"),
            ("imported", "witness::gate_environment_witness"),
        ];
        let exe = std::env::current_exe().unwrap();
        let unit = "gate::hermetic::tests::suite_environment_witness";
        child(
            &h,
            "unit",
            h.program(exe.to_str().unwrap()).args(["--exact", unit]),
            true,
        );
        h.witness().unwrap();
        let text = files::read(&record, 16 * 1024).unwrap();
        let metadata = std::fs::metadata(&record).unwrap();
        for (label, test) in helpers {
            child(
                &h,
                label,
                h.program(h.root.join(label).to_str().unwrap())
                    .args(["--exact", test]),
                true,
            );
            assert_eq!(files::read(&record, 16 * 1024).unwrap(), text);
            assert_eq!(std::fs::metadata(&record).unwrap().ino(), metadata.ino());
        }
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&h.witness).unwrap().mode() & 0o777, 0o700);
        child(
            &h,
            "duplicate-unit",
            h.program(exe.to_str().unwrap()).args(["--exact", unit]),
            false,
        );
        assert_eq!(files::read(&record, 16 * 1024).unwrap(), text);
        h.witness().unwrap();
        std::fs::remove_file(&record).unwrap();
        // A helper must not rescue a missing actual unit witness.
        for (label, test) in helpers {
            child(
                &h,
                &format!("missing-{label}"),
                h.program(h.root.join(label).to_str().unwrap())
                    .args(["--exact", test]),
                true,
            );
            assert!(!record.exists());
            assert!(h.witness().unwrap_err().to_string().contains("missing"));
        }
        // Mismatched evidence remains mismatched; helpers cannot repair it.
        files::write(&record, b"schema=bad\n").unwrap();
        for (label, test) in helpers {
            child(
                &h,
                &format!("bad-{label}"),
                h.program(h.root.join(label).to_str().unwrap())
                    .args(["--exact", test]),
                true,
            );
            assert_eq!(files::read(&record, 16 * 1024).unwrap(), b"schema=bad\n");
            assert!(h.witness().is_err());
        }
        std::fs::remove_file(&record).unwrap();
        child(
            &h,
            "designated-standalone",
            h.program(h.root.join("standalone").to_str().unwrap())
                .env("XTASK_GATE_WITNESS_WRITER", "standalone")
                .args(["--exact", "gate_environment_witness"]),
            true,
        );
        h.witness().unwrap();
        child(
            &h,
            "duplicate-standalone",
            h.program(h.root.join("standalone").to_str().unwrap())
                .env("XTASK_GATE_WITNESS_WRITER", "standalone")
                .args(["--exact", "gate_environment_witness"]),
            false,
        );
        h.witness().unwrap();
        std::fs::remove_file(&record).unwrap();
        child(
            &h,
            "planted",
            h.program(h.root.join("planted").to_str().unwrap())
                .args(["--exact", "gate_environment_witness"]),
            true,
        );
        h.witness().unwrap();
        std::fs::remove_dir_all(&h.root).unwrap();
    }

    #[test]
    fn actual_folded_suite_judges_failures_and_both_hygiene_checks() {
        let root = Scratch::new();
        for case in [
            "positive",
            "failure",
            "home",
            "cli",
            "zero",
            "toolchain",
            "compile",
            "spawn",
        ] {
            let (repo, _target, h, lease) = fixture(&root.0, case);
            let mut runner = Runner::without_marker(&lease).unwrap();
            if case == "spawn" {
                assert!(
                    runner
                        .run(std::process::Command::new("/bin/sh").args(["-c", "exit 0"]))
                        .unwrap()
                        .success()
                );
            }
            let suite = run(&repo, &h, &mut runner);
            if let Some(directory) = std::env::var_os("XTASK_F45_RETAIN") {
                let directory = std::path::PathBuf::from(directory);
                let retained = files::private_dir(&directory, &format!("folded-{case}")).unwrap();
                for (name, bytes) in &suite.logs {
                    files::write(&retained.join(name.rsplit('/').next().unwrap()), bytes).unwrap();
                }
                files::write(&retained.join("inventory.tsv"), suite.inventory.as_bytes()).unwrap();
                files::write(
                    &retained.join("result.txt"),
                    report::table(&suite.steps).as_bytes(),
                )
                .unwrap();
                files::write(
                    &retained.join("environment.txt"),
                    h.environment().as_bytes(),
                )
                .unwrap();
            }
            let fail = |name| {
                suite.steps.iter().find(|s| s.name == name).unwrap().verdict == Verdict::Fail
            };
            let expected = match case {
                "positive" => (false, false, false),
                "failure" => (true, false, false),
                "home" => (false, true, false),
                "cli" => (false, false, true),
                "zero" | "toolchain" | "compile" | "spawn" => (true, true, true),
                _ => unreachable!(),
            };
            assert_eq!(
                (fail("test"), fail("home-hygiene"), fail("no-real-cli")),
                expected,
                "{case}: {}",
                report::table(&suite.steps)
            );
            if case == "failure" {
                // nextest reports a test failure as 100, where libtest used 101.
                assert_eq!(suite.steps[1].exit, Some(100));
                assert!(suite.inventory.contains("witness\tfailed"));
            }
            if case == "zero" {
                assert!(
                    suite.steps[1].detail.contains("no test executed"),
                    "{}",
                    suite.steps[1].detail
                );
            }
            if case == "toolchain" {
                assert!(fail("build"));
                assert_eq!(suite.steps[0].exit, Some(1));
                assert!(
                    !String::from_utf8_lossy(&suite.logs["steps/build.log"])
                        .contains("downloading")
                );
            }
            if case == "compile" {
                assert!(fail("build"));
                assert_eq!(suite.steps[0].exit, Some(101));
                assert!(
                    String::from_utf8_lossy(&suite.logs["steps/build.log"]).contains("planted")
                );
            }
            if case == "spawn" {
                assert!(fail("build"));
                assert_eq!(
                    suite.steps[0].exit, None,
                    "no spawned build: never reuse an earlier child status"
                );
                assert!(runner.finished().is_err());
            } else {
                runner.finished().unwrap();
            }
            // No fixture worker remains; only then may scratch be removed.
        }
    }
}
