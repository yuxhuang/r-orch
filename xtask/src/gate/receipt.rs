//! Partial diagnostic integrity is separate from execution and qualification.
use super::{inventory, judge, report};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

pub(super) const STEPS: &[&str] = &["build", "test", "home-hygiene", "no-real-cli", "real-home"];

// Only the expected partial omission is exempted. Never map arbitrary exit3 to
// green: every other source, lease, lifecycle and environment reason survives.
pub(super) fn successful(steps: &[report::Step], reasons: &[&str], inventory: &str) -> bool {
    reasons == ["partial run"]
        && steps.len() == STEPS.len()
        && STEPS.iter().all(|name| {
            steps.iter().filter(|s| s.name == *name).count() == 1
                && steps.iter().any(|s| {
                    s.name == *name && s.verdict == report::Verdict::Pass && s.exit == Some(0)
                })
        })
        && !inventory.is_empty()
}

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Started,
    BuildStarted,
    BuildReturned,
    TestStarted,
    TestReturned,
    SuiteReturned,
    TargetReturned,
    Exported,
    ExportFailed,
}
impl Phase {
    fn text(self) -> &'static str {
        match self {
            Self::Started => {
                "started; final receipt/inventory pending; loss before export means incomplete diagnostics"
            }
            Self::BuildStarted => "build started; final receipt pending",
            Self::BuildReturned => "build returned; final receipt pending",
            Self::TestStarted => "test phase entered; final receipt pending",
            Self::TestReturned => "test phase returned; final receipt pending",
            Self::SuiteReturned => "suite returned; lifecycle/export pending",
            Self::TargetReturned => "target use returned; final receipt pending",
            Self::ExportFailed => {
                "final export failed; required receipt unavailable; diagnostics incomplete"
            }
            Self::Exported => {
                "final receipt exported; diagnostic only; check command status and verify-receipt"
            }
        }
    }
}
// Fixed finite messages only: never paths, child bytes, secrets, test names or
// workflow commands. Best-effort progress must not interfere with teardown or
// replace signal status when the job-log reader disappears.
pub(super) fn progress(phase: Phase) {
    use std::io::Write;
    let _ = writeln!(
        std::io::stderr().lock(),
        "gate receipt progress: {}",
        phase.text()
    );
}

pub(super) struct Summary {
    complete: bool,
    outcome: &'static str,
    safety: bool,
    tests: usize,
}
impl Summary {
    pub(super) fn text(&self) -> String {
        format!(
            "receipt verified: diagnostic only; completeness={}; outcome={}; safety={}; named-tests={}; qualifying=no; acceptance=not-granted",
            if self.complete {
                "complete"
            } else {
                "incomplete"
            },
            self.outcome,
            if self.safety {
                "pass"
            } else {
                "failed-or-unestablished"
            },
            self.tests
        )
    }
}
fn fields(text: &str) -> Result<BTreeMap<&str, &str>> {
    let mut fields = BTreeMap::new();
    for line in text.lines().take_while(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').context("receipt field")?;
        ensure!(
            !key.is_empty() && fields.insert(key, value.trim()).is_none(),
            "duplicate receipt field"
        );
    }
    Ok(fields)
}

pub(super) fn verify(repo: &Path, directory: &Path, commit: &str) -> Result<Summary> {
    ensure!(
        [40, 64].contains(&commit.len()) && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected commit must be a full object id"
    );
    let entries = report::load_bundle(repo, directory)?;
    let f = fields(&entries["result.txt"])?;
    ensure!(
        f.get("receipt") == Some(&"diagnostic")
            && f.get("tier") == Some(&"pr")
            && f.get("part") == Some(&"suite"),
        "unsupported receipt scope"
    );
    ensure!(
        f.get("qualifying").is_some_and(|s| s.starts_with("no: ")),
        "partial receipt must be nonqualifying"
    );
    ensure!(f.get("commit") == Some(&commit), "commit mismatch");
    ensure!(
        f.get("judge-source-id") == Some(&judge::at_commit(repo, commit)?.as_str()),
        "judge-source-id mismatch"
    );
    let tree = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", &format!("{commit}^{{tree}}")])
        .output()?;
    ensure!(tree.status.success(), "expected commit tree unavailable");
    let tree = String::from_utf8(tree.stdout)?;
    ensure!(
        ["tree", "tree-start"]
            .iter()
            .all(|key| f.get(key) == Some(&tree.trim())),
        "tree mismatch"
    );
    if f.get("moved") == Some(&"no") {
        ensure!(
            f.get("head-end") == Some(&commit) && f.get("tree-end") == Some(&tree.trim()),
            "end identity mismatch"
        );
    }
    let outcome = match f.get("verdict").copied() {
        Some("PASSED (part suite)") => "passed",
        Some("FAILED (part suite)") => "failed",
        Some("INTERRUPTED (part suite)") => "interrupted",
        Some("REFUSED (part suite)") => "refused",
        Some("ENV-LIMITED (part suite)") => "env-limited",
        _ => anyhow::bail!("unsupported receipt outcome"),
    };
    let safety = [
        ("dirty", "no"),
        ("dirty-end", "no"),
        ("moved", "no"),
        ("lease-default", "yes"),
        ("lease-path", "default"),
        ("lease-override", "no"),
        ("lease-held-through-teardown", "yes"),
        ("lease-lost", "no"),
        ("lease-holder-live", "yes"),
        ("lease-token-match", "yes"),
        ("pinned-toolchain", "yes"),
        ("target-finished", "yes"),
        ("judge-bootstrap", "verified"),
        ("publication", "record published"),
    ]
    .iter()
    .all(|(key, value)| f.get(key) == Some(value))
        && matches!(
            (f.get("lease").copied(), f.get("lease-authority").copied()),
            (Some("exclusive"), Some("kernel-exclusive"))
                | (Some("inherited"), Some("live-exclusive-holder"))
        );
    let mut complete = true;
    let mut steps_passed = true;
    for name in STEPS {
        ensure!(
            entries.contains_key(&format!("steps/{name}.log")),
            "missing step {name} log"
        );
        let status = f
            .get(format!("step-{name}-status").as_str())
            .context("missing step status")?;
        ensure!(
            ["pass", "FAIL", "ENV-LIMITED"].contains(status),
            "invalid step status"
        );
        let exit = f
            .get(format!("step-{name}-exit").as_str())
            .context("missing step exit")?;
        if *exit == "unknown" {
            complete = false;
        } else {
            exit.parse::<i32>()?;
        }
        steps_passed &= *status == "pass" && *exit == "0";
        let wall = f
            .get(format!("step-{name}-wall").as_str())
            .context("missing step wall")?
            .parse::<f64>()?;
        ensure!(wall.is_finite() && wall >= 0., "invalid step wall");
    }
    // There is one parser. Empty inventory is an honest incomplete diagnostic;
    // a nonempty inventory must be reproducible from the retained original logs.
    let inventory = &entries["inventory.tsv"];
    if inventory.is_empty() {
        complete = false;
    } else {
        let exit = f
            .get("step-test-exit")
            .context("test exit missing")?
            .parse::<i32>()?;
        let rebuilt = inventory::parse(
            &entries["steps/test.log"],
            &entries["steps/build.log"],
            exit,
        )?;
        ensure!(
            inventory::tsv(&rebuilt) == *inventory,
            "inventory differs from step logs"
        );
        if outcome == "passed" {
            ensure!(
                rebuilt.iter().all(|row| row.status != "failed"),
                "passed receipt contains failed tests"
            );
        }
    }
    if outcome == "passed" {
        ensure!(
            steps_passed && complete,
            "passed receipt has incomplete or failed checks"
        );
    }
    Ok(Summary {
        complete,
        outcome,
        safety,
        tests: inventory.lines().count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_success_exempts_only_expected_omission() {
        let mut steps: Vec<_> = STEPS
            .iter()
            .map(|name| report::Step {
                name,
                verdict: report::Verdict::Pass,
                exit: Some(0),
                elapsed: std::time::Duration::ZERO,
                detail: String::new(),
            })
            .collect();
        assert!(successful(&steps, &["partial run"], "named inventory\n"));
        for reason in [
            "source/head changed",
            "lease missing or lost",
            "dirty source",
            "target use not finished",
            "pinned toolchain not established",
            "judge executable is not content-stamped bootstrap output",
            "checks incomplete or failed",
            "run error",
            "interrupted",
        ] {
            assert!(!successful(
                &steps,
                &["partial run", reason],
                "named inventory\n"
            ));
        }
        assert!(!successful(&steps, &["partial run"], ""));
        steps[1].verdict = report::Verdict::Fail;
        assert!(!successful(&steps, &["partial run"], "failed inventory\n"));
        steps[1].verdict = report::Verdict::EnvLimited;
        assert!(!successful(&steps, &["partial run"], "named inventory\n"));
    }
}
