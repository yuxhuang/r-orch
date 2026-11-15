//! Whole-run ownership: the lease outlives target finish and retained reporting.
use super::{
    GateArgs, Part, Tier, files, hermetic, lease, report, signals::Signals, suite, target,
};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

pub(super) fn name(tier: Tier) -> &'static str {
    match tier {
        Tier::Full => "full",
        Tier::Pr => "pr",
        Tier::Fast => "fast",
    }
}
pub(super) fn target_path(repo: &Path, args: &GateArgs) -> PathBuf {
    repo.join(match (args.tier, args.warm) {
        (Tier::Full, true) => "target",
        (Tier::Full, false) => "target/gate",
        _ => "target/gate-warm",
    })
}
fn part(p: Option<Part>) -> &'static str {
    match p {
        None => "-",
        Some(Part::Suite) => "suite",
        Some(Part::Static) => "static",
    }
}
pub(super) fn plan(repo: &Path, args: &GateArgs) -> Result<i32> {
    ensure!(
        !args.warm || args.tier == Tier::Full,
        "--warm is only valid for full"
    );
    println!(
        "tier: {}\npurpose: {}\ntarget: {}\nlease: exclusive\npart: {}\nsteps: {}\nnative-lifecycle: checked cooperative paths; plan only, nothing ran",
        name(args.tier),
        if args.tier == Tier::Pr {
            "merge condition (after policy transition)"
        } else {
            "goal and release gate"
        },
        target_path(repo, args).display(),
        part(args.part),
        report::STEPS
            .iter()
            .filter(|s| match args.part {
                Some(Part::Static) => ["clippy", "fmt", "advisories", "crate-graph"].contains(s),
                Some(Part::Suite) => !["clippy", "fmt", "advisories", "crate-graph"].contains(s),
                None => true,
            })
            .copied()
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(0)
}
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    ensure!(out.status.success(), "git identity failed");
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}
fn dirty(repo: &Path) -> Result<bool> {
    Ok(!git(repo, &["status", "--porcelain", "--untracked-files=all"])?.is_empty())
}

pub(super) fn run(repo: &Path, args: &GateArgs) -> Result<i32> {
    if args.tier == Tier::Fast {
        eprintln!("gate: not available yet: fast belongs to PR2; nothing ran");
        return Ok(2);
    }
    if args.plan {
        return plan(repo, args);
    }
    let repo = repo.canonicalize()?;
    files::real_directories(&repo)?;
    let start = Instant::now();
    let started = SystemTime::now();
    let env: BTreeMap<_, _> = std::env::vars_os().collect();
    // Safe pinned Unix wrapper: no helper child or PATH trust for the privacy
    // datum, including reports that refuse before acquiring a machine lease.
    let hostname = nix::unistd::gethostname()
        .ok()
        .and_then(|s| s.into_string().ok())
        .filter(|s| !s.is_empty());
    let run = report::Run::begin(&repo)?;
    let head = git(&repo, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap_or_else(|_| "unknown".into());
    let dirty_start = dirty(&repo).unwrap_or(true);
    let judge = super::judge::current(&repo);
    let mut signals = Signals::new()?; // Remains alive through the final report.
    let mut held = None;
    let mut lease_state = "none";
    let mut outcome = "not admitted (unchanged)".to_owned();
    let mut publication = "not started";
    let mut target_finished = false;
    let mut admitted_use = false;
    let mut suite = None;
    let mut environment = String::new();
    let mut tools = String::new();
    let mut pinned_toolchain = false;
    let mut scratch = None;
    let observations = std::rc::Rc::new(std::cell::RefCell::new(super::contention::Sampler::new()));
    let mut command_statuses = String::new();
    let result = (|| -> Result<()> {
        ensure!(
            args.no_lease.is_none(),
            "configured lease required before target admission"
        );
        hermetic::preflight(&repo, &env)?;
        let wait = args
            .lease_wait
            .checked_mul(60)
            .context("lease wait overflow")?;
        let lease =
            lease::Lease::configured(false, lease::Mode::Exclusive, Duration::from_secs(wait))?;
        lease_state = if matches!(&lease, lease::Lease::Inherited(_)) {
            "inherited"
        } else {
            "exclusive"
        };
        held = Some(lease);
        let lease = held.as_ref().unwrap();
        if let lease::Lease::Owned(owned) = lease {
            owned.purpose(&format!("gate {} {}", name(args.tier), part(args.part)))?;
        }
        ensure!(
            signals.wait(Duration::ZERO).is_none(),
            "INTERRUPTED before target admission"
        );
        if let Some(expected) = std::env::var_os("XTASK_GATE_JUDGE_ID") {
            let current = judge.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?;
            ensure!(
                expected.to_str() == Some(current),
                "judge-source-id changed during bootstrap"
            );
        }
        let home = PathBuf::from(
            env.get(std::ffi::OsStr::new("HOME"))
                .context("HOME missing")?,
        );
        let mut work = |runner: &mut super::steps::Runner<'_, '_>, path: &Path| -> Result<()> {
            let h = hermetic::Hermetic::new(&repo, path, &std::env::temp_dir(), env.clone())?;
            scratch = Some(h.root.clone());
            environment = h.environment();
            let observation = runner.observation();
            observations.borrow_mut().record(observation);
            runner.observe(observations.clone());
            for (label, program, args) in [
                ("git", "git", vec!["--version"]),
                ("rustc", "rustc", vec!["-Vv"]),
                ("cargo", "cargo", vec!["-V"]),
                ("cargo-deny", "cargo-deny", vec!["--version"]),
            ] {
                if !h.available(program) {
                    tools.push_str(&format!("{label}: unavailable\n"));
                    continue;
                }
                let out = runner.capture_raw(h.program(program).args(args), 16 * 1024)?;
                tools.push_str(&format!(
                    "{label}: {}\n",
                    String::from_utf8_lossy(&out.stdout)
                        .trim()
                        .replace('\n', "; ")
                ));
            }
            pinned_toolchain = tools
                .lines()
                .any(|s| s.starts_with(&format!("rustc: rustc {} ", h.pin)))
                && tools
                    .lines()
                    .any(|s| s.starts_with(&format!("cargo: cargo {} ", h.pin)));
            tools.push_str(&format!(
                "Cargo.lock: {}\n",
                report::blob(
                    &repo,
                    &files::read(&repo.join("Cargo.lock"), 16 * 1024 * 1024)?
                )?
            ));
            suite = Some(match args.part {
                None => suite::run_all(&repo, &home, &h, runner),
                p => suite::run_selected(&repo, &home, &h, runner, p),
            });
            let observation = runner.observation();
            observations.borrow_mut().record(observation);
            Ok(())
        };
        if args.warm {
            files::real_directories(&repo.join("target"))?;
            outcome = "developer (not a gate)".into();
            let mut runner = super::steps::Runner::without_marker(lease)?;
            let work_result = work(&mut runner, &repo.join("target"));
            let finished = runner.finished();
            for status in runner.statuses().iter().chain(runner.probe_statuses()) {
                command_statuses.push_str(&format!(
                    "exit={} wall-ms={} {}\n",
                    report::code(status.exit),
                    status.elapsed.as_millis(),
                    report::diagnostic(status.command.as_bytes())
                ));
            }
            work_result?;
            finished?;
            publication = "developer target (not a gate)";
        } else {
            let cold = args.tier == Tier::Full;
            if cold {
                let mut runner = super::steps::Runner::without_marker(lease)?;
                target::cold_space(std::str::from_utf8(
                    &runner.capture(Command::new("df").arg("-Pk").arg(&repo), 16 * 1024)?,
                )?)?;
                runner.finished()?;
            }
            let admitted = target::native_admission(&repo, cold, lease)?;
            admitted_use = true;
            let use_ = admitted.begin(lease)?;
            outcome = match use_.decision() {
                "reset: source content changed" => {
                    "reset: content changed without a newer modification time"
                }
                "reset: different worktree" => "reset: another worktree",
                other => other,
            }
            .into();
            let finished = match use_.run(lease, args.keep, work) {
                Ok(finished) => finished,
                Err(error) => {
                    if let Some(failure) = error.downcast_ref::<target::UseFailure>() {
                        for status in &failure.statuses {
                            command_statuses.push_str(&format!(
                                "reaped-exit={} wall-ms={} {}\n",
                                report::code(status.exit),
                                status.elapsed.as_millis(),
                                report::diagnostic(status.command.as_bytes())
                            ));
                        }
                    }
                    return Err(error);
                }
            };
            for s in &finished.statuses {
                command_statuses.push_str(&format!(
                    "exit={} wall-ms={} {}\n",
                    report::code(s.exit),
                    s.elapsed.as_millis(),
                    report::diagnostic(s.command.as_bytes())
                ));
            }
            command_statuses.push_str(&format!("finished-ns={}\n", finished.end_ns));
            publication = finished.publication;
            target_finished = true;
            if cold {
                outcome = if args.keep {
                    "kept by request"
                } else {
                    "deleted"
                }
                .into();
            }
            finished.result?;
        }
        Ok(())
    })();
    if suite.is_none()
        && admitted_use
        && let Err(error) = &result
    {
        let mut unstarted = suite::Suite {
            steps: vec![],
            logs: BTreeMap::new(),
            inventory: String::new(),
        };
        for name in ["build", "test", "home-hygiene", "no-real-cli", "real-home"] {
            let detail = format!("not evaluated: suite did not start: {error:#}");
            unstarted
                .logs
                .insert(format!("steps/{name}.log"), detail.as_bytes().to_vec());
            unstarted.steps.push(report::Step {
                name,
                verdict: report::Verdict::Fail,
                exit: None,
                elapsed: Duration::ZERO,
                detail,
            });
        }
        suite = Some(unstarted);
    }
    let interrupted = signals.wait(Duration::ZERO);
    if admitted_use && !target_finished && outcome != "not admitted (unchanged)" {
        outcome = format!(
            "kept ({})",
            result
                .as_ref()
                .err()
                .map_or("unfinished use".into(), |e| report::diagnostic(
                    format!("{e:#}").as_bytes()
                ))
        );
    }
    let lease_good = held.as_ref().is_some_and(|l| l.validate().is_ok());
    if held.is_some() && !lease_good {
        lease_state = "lost";
    }
    let head_end = git(&repo, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "unknown".into());
    let tree_end = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap_or_else(|_| "unknown".into());
    let dirty_end = dirty(&repo).unwrap_or(true);
    let judge_same = judge
        .as_ref()
        .ok()
        .zip(super::judge::current(&repo).ok())
        .is_some_and(|(a, b)| a == &b);
    let moved = head != head_end || tree != tree_end;
    let steps = suite.as_ref().map_or(&[][..], |s| s.steps.as_slice());
    let coverage = steps
        .iter()
        .find(|s| s.name == "real-home")
        .map_or("-", |s| {
            if s.detail.contains("partial") {
                "partial"
            } else {
                "complete"
            }
        });
    let mut reasons = vec![];
    if args.part.is_some() {
        reasons.push("partial run");
    }
    if args.warm {
        reasons.push("developer target is not a gate");
    }
    if args.keep {
        reasons.push("target kept by request");
    }
    if !pinned_toolchain {
        reasons.push("pinned toolchain not established");
    }
    if dirty_start || dirty_end {
        reasons.push("dirty source");
    }
    if moved || !judge_same {
        reasons.push("source/head changed");
    }
    if !lease_good {
        reasons.push("lease missing or lost");
    }
    let default = std::env::var_os("XTASK_GATE_LEASE_DIR").is_none();
    if !default {
        reasons.push("lease directory override");
    }
    if !target_finished {
        reasons.push("target use not finished");
    }
    if !["record published", "cold target removed"].contains(&publication) {
        reasons.push(publication);
    }
    if steps.len() != report::STEPS.len()
        || steps.iter().any(|s| s.verdict != report::Verdict::Pass)
    {
        reasons.push("checks incomplete or failed");
    }
    if result.is_err() {
        reasons.push("run error");
    }
    if interrupted.is_some() {
        reasons.push("interrupted");
    }
    let bootstrap_verified = judge.as_ref().ok().is_some_and(|id| {
        std::env::var("XTASK_GATE_JUDGE_ID").ok().as_ref() == Some(id)
            && std::env::current_exe()
                .ok()
                .is_some_and(|exe| super::judge::stamped(&repo, id, &exe).is_ok())
    });
    if !bootstrap_verified {
        reasons.push("judge executable is not content-stamped bootstrap output");
    }
    if hostname.is_none() {
        reasons.push("hostname unavailable for evidence redaction");
    }
    let qualifying = reasons.is_empty();
    let base_code = report::exit(steps, args.evidence.is_some() && !qualifying);
    let code = if let Some(sig) = interrupted {
        128 + sig
    } else if result.as_ref().is_err_and(|e| e.is::<lease::Contended>()) {
        75
    } else if result.is_err() {
        1
    } else {
        base_code
    };
    let verdict = if interrupted.is_some() {
        "INTERRUPTED"
    } else if result.is_err() {
        "REFUSED"
    } else if steps.iter().any(|s| s.verdict == report::Verdict::Fail) {
        "FAILED"
    } else if steps
        .iter()
        .any(|s| s.verdict == report::Verdict::EnvLimited)
    {
        "ENV-LIMITED"
    } else {
        "PASSED"
    };
    let kind = if args.part.is_some() { "part" } else { "tier" };
    let subject = if args.part.is_some() {
        part(args.part)
    } else {
        name(args.tier)
    };
    let mut fields = vec![
        (
            "judge-bootstrap",
            if bootstrap_verified {
                "verified"
            } else {
                "absent or unsupported"
            }
            .into(),
        ),
        ("verdict", format!("{verdict} ({kind} {subject})")),
        ("commit", head.clone()),
        ("head-end", head_end),
        ("tree", tree.clone()),
        ("tree-start", tree),
        ("tree-end", tree_end),
        ("dirty", if dirty_start { "yes" } else { "no" }.into()),
        ("dirty-end", if dirty_end { "yes" } else { "no" }.into()),
        ("moved", if moved { "yes" } else { "no" }.into()),
        ("tier", name(args.tier).into()),
        ("part", part(args.part).into()),
        (
            "mode",
            if args.warm {
                "developer (not a gate)"
            } else {
                name(args.tier)
            }
            .into(),
        ),
        ("run-dir", run.path.display().to_string()),
        ("started", format!("{started:?}")),
        ("finished", format!("{:?}", SystemTime::now())),
        ("target", outcome),
        (
            "target-finished",
            if target_finished { "yes" } else { "no" }.into(),
        ),
        ("publication", publication.into()),
        ("lease", lease_state.into()),
        ("lease-default", if default { "yes" } else { "no" }.into()),
        (
            "lease-path",
            if default { "default" } else { "override" }.into(),
        ),
        ("lease-override", if default { "no" } else { "yes" }.into()),
        (
            "lease-held-through-teardown",
            if lease_good { "yes" } else { "no" }.into(),
        ),
        (
            "lease-lost",
            if lease_state == "lost" { "yes" } else { "no" }.into(),
        ),
        (
            "lease-holder-live",
            if lease_good { "yes" } else { "no" }.into(),
        ),
        (
            "lease-token-match",
            if lease_good { "yes" } else { "no" }.into(),
        ),
        (
            "lease-authority",
            if lease_state == "inherited" {
                "live-exclusive-holder"
            } else if lease_state == "exclusive" {
                "kernel-exclusive"
            } else {
                "none"
            }
            .into(),
        ),
        (
            "pinned-toolchain",
            if pinned_toolchain { "yes" } else { "no" }.into(),
        ),
        (
            "judge-source-id",
            judge.as_ref().map_or("absent".into(), Clone::clone),
        ),
        (
            "foreign-build-activity",
            observations.borrow().foreign.into(),
        ),
        ("load-one-minute-max", observations.borrow().load()),
        (
            "foreign-build-count-max",
            observations.borrow().max_foreign.to_string(),
        ),
        (
            "contention-sampling",
            format!(
                "start/end and every 15s; {} samples",
                observations.borrow().samples
            ),
        ),
        ("thermal-pressure", observations.borrow().thermal.clone()),
        ("real-home-coverage", coverage.into()),
        (
            "host-label",
            args.host_label
                .as_ref()
                .map_or("-".into(), |s| report::diagnostic(s.as_bytes())),
        ),
        (
            "qualifying",
            if qualifying {
                "yes".into()
            } else {
                format!("no: {}", reasons.join("; "))
            },
        ),
        (
            "total-wall-ms",
            steps
                .iter()
                .map(|s| s.elapsed.as_millis())
                .sum::<u128>()
                .to_string(),
        ),
        ("run-wall-ms", start.elapsed().as_millis().to_string()),
    ];
    let mut text = String::new();
    for (key, value) in fields.drain(..) {
        text.push_str(&format!("{key}: {value}\n"));
    }
    for s in steps {
        text.push_str(&format!(
            "step-{}-status: {}\nstep-{}-exit: {}\nstep-{}-wall: {:.6}\n",
            s.name,
            s.verdict.text(),
            s.name,
            s.exit.map_or("unknown".into(), |n| n.to_string()),
            s.name,
            s.elapsed.as_secs_f64()
        ));
    }
    text.push('\n');
    text.push_str(&report::table(steps));
    for s in steps {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\tsteps/{}.log\t{}\n",
            s.name,
            s.verdict.text(),
            s.exit.map_or("unknown".into(), |e| e.to_string()),
            s.elapsed.as_millis(),
            s.name,
            report::diagnostic(s.detail.as_bytes())
        ));
    }
    if let Err(e) = &result {
        let why = if e
            .downcast_ref::<target::Fault>()
            .is_some_and(|f| matches!(f, target::Fault::Busy))
        {
            format!(
                "{} target in use; {e:#}",
                if args.tier == Tier::Full {
                    "cold"
                } else {
                    "warm"
                }
            )
        } else {
            format!("{e:#}")
        };
        text.push_str(&report::marker_refusal(
            &repo.join(if args.tier == Tier::Full {
                "target/.gate-state/cold.use"
            } else {
                "target/.gate-state/warm.use"
            }),
            target::marker_diagnostics(&repo, args.tier == Tier::Full).as_deref(),
            &why,
        ));
    }
    text.push_str(&environment);
    let inventory = suite
        .as_ref()
        .map_or_else(Vec::new, |s| s.inventory.as_bytes().to_vec());
    let mut entries = suite.map_or_else(BTreeMap::new, |s| s.logs);
    entries.insert("result.txt".into(), text.as_bytes().to_vec());
    entries.insert("environment.txt".into(), environment.into_bytes());
    entries.insert("tools.txt".into(), tools.into_bytes());
    entries.insert("inventory.tsv".into(), inventory);
    command_statuses.push_str(&observations.borrow().log);
    run.finish(&repo, &entries, &command_statuses)?;
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME missing")?);
    let redactor = report::Redactor::new(&repo, &home, &std::env::temp_dir(), hostname.as_deref());
    println!("{}", redactor.apply(text.as_bytes())?);
    if let Some(destination) = &args.evidence {
        ensure!(
            hostname.is_some(),
            "hostname unavailable; evidence export refused"
        );
        let digest = report::export(&repo, destination, &entries, &redactor)?;
        println!("evidence manifest: {digest}; qualifying: {qualifying}");
    }
    // Keep unresolved scratch; successful finish leaves no directly owned child.
    if target_finished && let Some(path) = scratch {
        std::fs::remove_dir_all(path)?;
    }
    drop(held);
    Ok(code)
}
