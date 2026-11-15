//! PR1 tier dispatch. Never bypass the native D45 guard for a miniature tree.
use super::{GateArgs, Part, Tier, files, hermetic, lease, report, signals::Signals, target};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
pub(super) fn plan(repo: &Path, args: &GateArgs) -> Result<i32> {
    ensure!(
        !args.warm || args.tier == Tier::Full,
        "--warm is only valid for full"
    );
    println!(
        "tier: {}\npurpose: {}\ntarget: {}\nlease: exclusive\npart: {}\nsteps: {}\nnative-lifecycle: unavailable (D45); plan only, nothing ran",
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
fn part(p: Option<Part>) -> &'static str {
    match p {
        None => "-",
        Some(Part::Suite) => "suite",
        Some(Part::Static) => "static",
    }
}
fn identity(repo: &Path, key: &str) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or("unknown".into(), |s| s.trim().to_owned())
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
    let started = Instant::now();
    let env: BTreeMap<_, _> = std::env::vars_os().collect();
    let mut lease_state = "none";
    let mut signal = None;
    let mut signals = None;
    let result = (|| -> Result<()> {
        ensure!(
            !args.warm || args.tier == Tier::Full,
            "--warm is only valid for full"
        );
        ensure!(
            args.no_lease.is_none(),
            "configured lease required before target admission"
        );
        if args.part != Some(Part::Static) {
            hermetic::preflight(&repo, &env)?;
        }
        signals = Some(Signals::new()?); // Installed before waiting/admission.
        // CLI contract says minutes; one deadline across turnstile and lease.
        let seconds = args
            .lease_wait
            .checked_mul(60)
            .context("lease wait overflow")?;
        let lease =
            lease::Lease::configured(false, lease::Mode::Exclusive, Duration::from_secs(seconds))?;
        lease_state = if matches!(&lease, lease::Lease::Inherited(_)) {
            "inherited"
        } else {
            "exclusive"
        };
        if args.tier == Tier::Full && !args.warm {
            let mut runner = super::steps::Runner::without_marker(&lease)?;
            let output = runner.capture(
                std::process::Command::new("df").arg("-Pk").arg(&repo),
                16 * 1024,
            )?;
            target::cold_space(std::str::from_utf8(&output)?)?;
            runner.finished()?;
        }
        signal = signals.as_mut().unwrap().wait(Duration::ZERO);
        ensure!(signal.is_none(), "INTERRUPTED before target admission");
        // The predecessor's real native capability guard runs here. It refuses
        // before creation/deletion/build. No fixture or --part bypass exists.
        if let Some(expected) = std::env::var_os("XTASK_GATE_JUDGE_ID") {
            ensure!(
                expected.to_str() == Some(super::judge::current(&repo)?.as_str()),
                "judge-source-id changed during bootstrap"
            );
        }
        target::native_admission(&repo, args.tier == Tier::Full && !args.warm, &lease)?;
        anyhow::bail!("native target lifecycle is incomplete; no tier execution is supported")
    })();
    signal = signals
        .as_mut()
        .and_then(|s| s.wait(Duration::ZERO))
        .or(signal);
    let error = result.expect_err("native admission cannot complete yet");
    let code = if let Some(s) = signal {
        128 + s
    } else if error.is::<lease::Contended>() {
        75
    } else {
        1
    };
    let verdict = if signal.is_some() {
        "INTERRUPTED"
    } else {
        "REFUSED"
    };
    let why = format!("{error:#}");
    let marker = repo.join(if args.tier == Tier::Full && !args.warm {
        "target/.gate-state/cold.use"
    } else {
        "target/.gate-state/warm.use"
    });
    let mut text = format!(
        "verdict: {verdict} ({} {})\ncommit: {}\ntree: {}\ndirty: unknown\nmoved: unknown\nmode: {}\nstarted: {:?}\nfinished: {:?}\ntier: {}\npart: {}\nrun-dir: diagnostic-only\ntarget: {}\nlease: {lease_state}\nforeign-build-activity: unavailable\nreal-home-coverage: -\njudge-source-id: {}\nqualifying: no: native lifecycle unavailable; {}\n\n",
        if args.part.is_some() { "part" } else { "tier" },
        if args.part.is_some() {
            part(args.part)
        } else {
            name(args.tier)
        },
        identity(&repo, "HEAD"),
        identity(&repo, "HEAD^{tree}"),
        if args.warm {
            "developer (not a gate)"
        } else {
            name(args.tier)
        },
        std::time::SystemTime::now().checked_sub(started.elapsed()),
        std::time::SystemTime::now(),
        name(args.tier),
        part(args.part),
        if args.warm {
            "developer (not a gate)"
        } else {
            "not admitted (unchanged)"
        },
        std::env::var("XTASK_GATE_JUDGE_ID")
            .ok()
            .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()) && [40, 64].contains(&s.len()))
            .unwrap_or("absent".into()),
        report::diagnostic(why.as_bytes())
    );
    text.push_str(&report::table(&[]));
    let diagnostics = target::marker_diagnostics(&repo, args.tier == Tier::Full && !args.warm);
    text.push_str(&report::marker_refusal(
        &marker,
        diagnostics.as_deref(),
        &why,
    ));
    println!("{text}");
    // Failure to persist diagnostics is also a failure; never reuse an old green.
    let run_dir = report::write_result(&repo, &text)?;
    if let Some(destination) = &args.evidence {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME missing")?;
        let mut entries = BTreeMap::new();
        entries.insert("result.txt".into(), text.into_bytes());
        entries.insert("inventory.tsv".into(), vec![]);
        entries.insert("environment.txt".into(), vec![]);
        entries.insert(
            "tools.txt".into(),
            b"not evaluated: native admission refused\n".to_vec(),
        );
        let redactor = report::Redactor::new(&repo, &home, &std::env::temp_dir(), None);
        let digest = report::export(&repo, destination, &entries, &redactor)?;
        println!("evidence manifest: {digest}; nonqualifying");
    }
    eprintln!("diagnostic report: {}", run_dir.display());
    Ok(code)
}
