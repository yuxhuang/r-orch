//! Scenario definitions — `fixtures/scenarios/*.toml`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// How a scenario divides its steps between the two binaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Both binaries run **every** step, each in its own tmpdir; the two
    /// resulting trees are compared. This is the plain differential case.
    Differential,
    /// The TypeScript binary runs steps `1..=split_at`, then r-orch runs the
    /// rest **in the same directory**.
    TsThenRust,
    /// The reverse: r-orch writes first, the TypeScript binary continues.
    /// This is the direction that actually proves the shared-format claim,
    /// because a quoting bug in our writer is invisible to a same-binary test.
    RustThenTs,
    /// Alternate binaries per step, starting with r-orch.
    Interleaved,
}

impl Kind {
    pub fn is_cross_compat(self) -> bool {
        matches!(
            self,
            Kind::TsThenRust | Kind::RustThenTs | Kind::Interleaved
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub kind: Kind,
    /// Number of leading steps run by the first binary. Required for
    /// `ts_then_rust` and `rust_then_ts`, ignored otherwise.
    #[serde(default)]
    pub split_at: Option<usize>,
    /// Each step is an argv line, shell-quoted (`task add 'Some title'`).
    pub steps: Vec<String>,
    /// Read-only steps run by **both** binaries over the finished tree, whose
    /// output must match exactly.
    ///
    /// This is what gives a cross-compat scenario teeth. Without it the only
    /// assertions are "every step exited 0" and "both YAML parsers agree",
    /// neither of which notices that the second binary *misread* a value it
    /// could parse. Both binaries read the same directory here, so even the
    /// ids and timestamps must agree.
    #[serde(default)]
    pub verify: Vec<String>,
    /// Why this scenario exists, printed with the results.
    #[serde(default)]
    pub description: Option<String>,
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Scenario> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read scenario {}", path.display()))?;
        let scenario: Scenario =
            toml::from_str(&text).with_context(|| format!("parse scenario {}", path.display()))?;
        scenario
            .validate()
            .with_context(|| format!("invalid scenario {}", path.display()))?;
        Ok(scenario)
    }

    fn validate(&self) -> Result<()> {
        if self.steps.is_empty() {
            bail!("a scenario needs at least one step");
        }
        if self.kind.is_cross_compat() && self.verify.is_empty() {
            bail!(
                "a cross-compat scenario needs at least one `verify` step, or it only \
                 asserts that the steps exited 0"
            );
        }
        for step in &self.verify {
            let argv = self.argv(step)?;
            let command = argv.first().map(String::as_str).unwrap_or("");
            let sub = argv.get(1).map(String::as_str).unwrap_or("");
            // A `verify` step runs twice, once per binary, over one shared
            // tree. A mutating step would therefore fail its second run on a
            // duplicate — so the allowlist is a guard against a scenario that
            // reports a divergence it actually caused itself.
            if !READ_ONLY_VERIFY_STEPS.contains(&(command, sub)) {
                bail!(
                    "verify step `{step}` is not known to be read-only; `verify` steps run \
                     once per binary over the same directory, so a mutating step fails its \
                     second run. Allowed: {}",
                    READ_ONLY_VERIFY_STEPS
                        .iter()
                        .map(|(c, s)| format!("`{c} {s}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        for step in self.steps.iter().chain(&self.verify) {
            let argv = self.argv(step)?;
            if argv.is_empty() {
                bail!("step {step:?} parses to an empty argv");
            }
        }
        match (self.kind, self.split_at) {
            (Kind::TsThenRust | Kind::RustThenTs, None) => {
                bail!("{:?} requires `split_at`", self.kind)
            }
            (Kind::TsThenRust | Kind::RustThenTs, Some(at))
                if at == 0 || at >= self.steps.len() =>
            {
                bail!(
                    "`split_at` must leave work for both binaries: got {at} with {} steps",
                    self.steps.len()
                )
            }
            _ => Ok(()),
        }
    }

    pub fn argv(&self, step: &str) -> Result<Vec<String>> {
        shell_words::split(step).with_context(|| format!("split step {step:?}"))
    }

    /// Which binary runs each step, as a list the runner can walk.
    pub fn plan(&self) -> Vec<Runner> {
        match self.kind {
            Kind::Differential => vec![Runner::Both; self.steps.len()],
            Kind::TsThenRust => {
                let at = self.split_at.expect("validated");
                (0..self.steps.len())
                    .map(|i| if i < at { Runner::Ts } else { Runner::Rust })
                    .collect()
            }
            Kind::RustThenTs => {
                let at = self.split_at.expect("validated");
                (0..self.steps.len())
                    .map(|i| if i < at { Runner::Rust } else { Runner::Ts })
                    .collect()
            }
            Kind::Interleaved => (0..self.steps.len())
                .map(|i| if i % 2 == 0 { Runner::Rust } else { Runner::Ts })
                .collect(),
        }
    }
}

/// `(command, subcommand)` pairs that mutate nothing, so they are safe to run
/// once per binary over a shared tree. Extend as WP-5 lands more read-only
/// commands (`task show`, `agent list`, `status`, `config get`, …).
const READ_ONLY_VERIFY_STEPS: &[(&str, &str)] = &[("task", "list"), ("agent", "list")];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    /// Run in both tmpdirs and compare this step's output.
    Both,
    Rust,
    Ts,
}

/// Every scenario under a directory, sorted by file name for a stable order.
pub fn load_all(dir: &Path) -> Result<Vec<(std::path::PathBuf, Scenario)>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("read scenario dir {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();

    paths
        .into_iter()
        .map(|path| Scenario::load(&path).map(|s| (path, s)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn a_differential_scenario_runs_every_step_in_both_trees() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            r#"
name = "s"
kind = "differential"
steps = ["init --adapter shell", "task list"]
"#,
        );
        let scenario = Scenario::load(&path).unwrap();
        assert_eq!(scenario.plan(), vec![Runner::Both, Runner::Both]);
        assert_eq!(
            scenario.argv(&scenario.steps[0]).unwrap(),
            vec![
                "init".to_owned(),
                "--adapter".to_owned(),
                "shell".to_owned()
            ]
        );
    }

    #[test]
    fn quoted_arguments_survive_splitting() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            r#"
name = "s"
kind = "differential"
steps = ["task add 'A title with spaces' --assignee shelly"]
"#,
        );
        let scenario = Scenario::load(&path).unwrap();
        assert_eq!(
            scenario.argv(&scenario.steps[0]).unwrap(),
            vec![
                "task".to_owned(),
                "add".to_owned(),
                "A title with spaces".to_owned(),
                "--assignee".to_owned(),
                "shelly".to_owned()
            ]
        );
    }

    #[test]
    fn a_cross_compat_scenario_splits_its_steps() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            r#"
name = "s"
kind = "rust_then_ts"
split_at = 2
steps = ["a", "b", "c"]
verify = ["task list --json"]
"#,
        );
        let scenario = Scenario::load(&path).unwrap();
        assert!(scenario.kind.is_cross_compat());
        assert_eq!(
            scenario.plan(),
            vec![Runner::Rust, Runner::Rust, Runner::Ts]
        );
    }

    #[test]
    fn ts_then_rust_is_the_mirror_image() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"ts_then_rust\"\nsplit_at = 1\nsteps = [\"a\", \"b\"]\nverify = [\"task list\"]\n",
        );
        assert_eq!(
            Scenario::load(&path).unwrap().plan(),
            vec![Runner::Ts, Runner::Rust]
        );
    }

    #[test]
    fn interleaved_alternates_starting_with_rust() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"interleaved\"\nsteps = [\"a\", \"b\", \"c\"]\nverify = [\"task list\"]\n",
        );
        assert_eq!(
            Scenario::load(&path).unwrap().plan(),
            vec![Runner::Rust, Runner::Ts, Runner::Rust]
        );
    }

    #[test]
    fn a_cross_compat_scenario_without_a_usable_split_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        for body in [
            "name = \"s\"\nkind = \"rust_then_ts\"\nsteps = [\"a\"]\nverify = [\"task list\"]\n",
            "name = \"s\"\nkind = \"rust_then_ts\"\nsplit_at = 0\nsteps = [\"a\", \"b\"]\nverify = [\"task list\"]\n",
            "name = \"s\"\nkind = \"rust_then_ts\"\nsplit_at = 2\nsteps = [\"a\", \"b\"]\nverify = [\"task list\"]\n",
            "name = \"s\"\nkind = \"rust_then_ts\"\nsplit_at = 1\nsteps = [\"a\", \"b\"]\n",
        ] {
            let path = write(temp.path(), "s.toml", body);
            assert!(Scenario::load(&path).is_err(), "{body}");
        }
    }

    #[test]
    fn a_mutating_verify_step_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"rust_then_ts\"\nsplit_at = 1\nsteps = [\"a\", \"b\"]\n\
             verify = [\"agent add dup --adapter shell --command x\"]\n",
        );
        let err = Scenario::load(&path).expect_err("rejected");
        assert!(
            format!("{err:#}").contains("not known to be read-only"),
            "{err:#}"
        );
    }

    #[test]
    fn a_read_only_verify_step_is_accepted() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"rust_then_ts\"\nsplit_at = 1\nsteps = [\"a\", \"b\"]\n\
             verify = [\"task list --json\", \"agent list\"]\n",
        );
        assert_eq!(Scenario::load(&path).unwrap().verify.len(), 2);
    }

    #[test]
    fn an_empty_scenario_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"differential\"\nsteps = []\n",
        );
        assert!(Scenario::load(&path).is_err());
    }

    #[test]
    fn load_all_returns_scenarios_in_a_stable_order() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["b.toml", "a.toml", "notes.md"] {
            write(
                temp.path(),
                name,
                "name = \"x\"\nkind = \"differential\"\nsteps = [\"a\"]\n",
            );
        }
        let loaded = load_all(temp.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded[0].0.ends_with("a.toml"));
        assert!(loaded[1].0.ends_with("b.toml"));
    }
}
