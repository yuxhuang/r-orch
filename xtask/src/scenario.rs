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
    /// Rust subcommands this scenario needs, as they appear on the command
    /// line (`"task approve"`, `"run"`, `"serve"`).
    ///
    /// The harness probes the Rust binary for each and **skips** the scenario
    /// when one is missing, so a scenario written against a command WP-5 has
    /// not landed yet goes live the moment it does — nobody has to remember to
    /// un-ignore it. The oracle has every command, so only our side gates.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Put `fixtures/fake-editors/` first on `PATH` and set `EDITOR` to this
    /// script, making `task edit` and `agent edit` reachable.
    ///
    /// Those two subcommands take no flags at all — they round-trip the entity
    /// through `$EDITOR` — so without a scriptable editor they are the only
    /// part of PARITY §2.2 a scenario cannot touch.
    #[serde(default)]
    pub fake_editor: Option<String>,
    /// Per-step wall-clock budget. A `run` or `serve --once` that hangs must
    /// fail the scenario rather than the whole harness.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Extra environment for every step, applied to both binaries. Used for
    /// the fake agents' knobs (`ORCH_FAKE_*`).
    #[serde(default)]
    pub env: indexmap::IndexMap<String, String>,
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
    /// Files under `.orchestry/` whose top-level JSON **array** order is not
    /// contract, relative to that directory (`messages/_index.json`).
    ///
    /// The same exemption as the `@unordered:<reason>` step directive, for the
    /// file the step reads: `_index.json` is maintained in last-write order,
    /// which PARITY.md §6.10 states is not contract. Keep this list to the
    /// files a scenario genuinely cannot order — every other `_index.json`
    /// stays compared element by element, which is what catches an
    /// implementation that appends on update instead of moving to the end.
    ///
    /// Narrower than [`Self::excluded_tree_paths`], which drops a path from
    /// the comparison entirely: this one still compares every element, count
    /// and field.
    #[serde(default)]
    pub unordered_files: Vec<String>,
    /// Paths under `.orchestry/` whose contents the contract licenses this
    /// scenario not to compare, as prefixes of the normalized tree path.
    ///
    /// A **named** exclusion, like the §14.1 and §13 masks: it exists only
    /// where a clause says the two implementations legitimately differ, and
    /// the clause belongs in the scenario's description. The harness fails if
    /// an exclusion matches nothing, so a stale entry cannot sit here quietly
    /// switching off a comparison that had started working.
    #[serde(default)]
    pub excluded_tree_paths: Vec<String>,
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
            let known = READ_ONLY_VERIFY_STEPS.contains(&(command, sub))
                // `status`/`logs` take no verb, so slot 1 holds a flag or an id.
                || READ_ONLY_VERIFY_STEPS.contains(&(command, ""));
            if !known {
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
            // `@unordered` only means anything for a JSON document, and
            // without `--json` the output is a table whose row order *is*
            // contract — so a marker there would silence a real difference.
            if Scenario::unordered(step) && !argv.iter().any(|arg| arg == "--json") {
                bail!(
                    "step {step:?} is marked `@unordered` but does not pass `--json`; \
                     only a JSON array is compared as a multiset"
                );
            }
            if argv[0] == WRITE_STEP {
                if argv.len() != 3 {
                    bail!(
                        "`{WRITE_STEP} <path> <content>` takes exactly two arguments; \
                         got {} in {step:?}",
                        argv.len() - 1
                    );
                }
                let path = std::path::Path::new(&argv[1]);
                // The content goes into the scenario's tmpdir, so a path that
                // escapes it would write into the developer's checkout.
                if path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
                    bail!("`{WRITE_STEP}` needs a relative path inside the project: {step:?}");
                }
            }
        }
        for path in &self.unordered_files {
            if path.is_empty() || path.starts_with('/') || path.contains("..") {
                bail!("`unordered_files` entry {path:?} must be a plain path under .orchestry/");
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

    /// A step's argv, with any leading directives stripped and `${repeat:…}`
    /// expanded.
    pub fn argv(&self, step: &str) -> Result<Vec<String>> {
        let argv = shell_words::split(Directives::parse(step).1)
            .with_context(|| format!("split step {step:?}"))?;
        argv.iter()
            .map(|arg| expand_repeats(arg))
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("expand step {step:?}"))
    }

    /// `@kill-after:<ms> <argv…>` runs the step and kills it after `<ms>`.
    ///
    /// Needed for crash recovery (V25, V35): there is no argv line that means
    /// "a `serve --once` that dies mid-run". Pair it with a **cross-compat**
    /// scenario so the kill lands once in one shared tree — in two separate
    /// trees the kill would land at two different points and the trees would
    /// legitimately diverge.
    pub fn kill_after(step: &str) -> Option<std::time::Duration> {
        Directives::parse(step).0.kill_after
    }

    /// Whether only this step's exit code is compared, its output being L2.
    pub fn exit_only(step: &str) -> bool {
        Directives::parse(step).0.exit_only
    }

    /// Whether the author gave a reason for `@exit-only` (`@exit-only:<why>`).
    pub fn exit_only_reason(step: &str) -> bool {
        Directives::parse(step).0.exit_only_reason
    }

    /// Whether `orchestrator:tick` records are dropped from this step's
    /// comparison (`@ticks-vary:<why>`).
    pub fn ticks_vary(step: &str) -> bool {
        Directives::parse(step).0.ticks_vary
    }

    /// The `@stop-after:<ms>` budget, after which the step is SIGTERMed.
    pub fn stop_after(step: &str) -> Option<std::time::Duration> {
        Directives::parse(step).0.stop_after
    }

    /// Whether this step's JSON array is compared as a multiset
    /// (`@unordered:<why>`).
    pub fn unordered(step: &str) -> bool {
        Directives::parse(step).0.unordered
    }

    /// The per-step budget, defaulting to 30 s — enough for a fake agent to
    /// run and short enough that a hang is caught rather than waited out.
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.timeout_secs.unwrap_or(30))
    }

    /// Whether a step's stdout is a stream of JSON records rather than human
    /// text — `serve --log-format json`.
    ///
    /// Those records carry wall-clock timestamps and measured durations, so
    /// comparing them parsed (after normalization) is both stricter about the
    /// fields and tolerant of the values, which is what §11.1 asks for: the
    /// record *fields* are L0, the text format is L2.
    pub fn stdout_is_json_records(&self, step: &str) -> bool {
        let Ok(argv) = self.argv(step) else {
            return false;
        };
        argv.windows(2)
            .any(|pair| pair[0] == "--log-format" && pair[1] == "json")
            || (argv.first().map(String::as_str) == Some("serve")
                && !argv.iter().any(|arg| arg == "--log-format"))
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

/// Expand `${repeat:<text>:<count>}` in one argument.
///
/// The truncation limits are 200 characters for a subject and 4000 for a body,
/// so testing them needs arguments of that length. Written out literally they
/// would make the scenario unreadable and hide the one character that matters
/// — the multi-byte one sitting astride the cut.
///
/// This is deliberately separate from `${task.N}`: a repeat depends on nothing
/// the tree produced, so it expands at parse time and the loader and the lint
/// both see the real argument.
fn expand_repeats(arg: &str) -> Result<String> {
    const MARKER: &str = "${repeat:";
    let mut out = String::new();
    let mut rest = arg;

    while let Some(start) = rest.find(MARKER) {
        out.push_str(&rest[..start]);
        let after = &rest[start + MARKER.len()..];
        let Some(end) = after.find('}') else {
            bail!("unterminated `{MARKER}` in {arg:?}");
        };
        let (text, count) = after[..end]
            .rsplit_once(':')
            .with_context(|| format!("`{MARKER}` needs `<text>:<count>` in {arg:?}"))?;
        let count: usize = count
            .parse()
            .with_context(|| format!("`{MARKER}` count must be a number in {arg:?}"))?;
        // A scenario that asks for a megabyte is a typo, not an intention:
        // argv has an OS limit and the diff would be unreadable either way.
        if count * text.len() > 64 * 1024 {
            bail!("`{MARKER}{text}:{count}}}` would expand past 64 KiB in {arg:?}");
        }
        out.push_str(&text.repeat(count));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Leading `@name[:arg]` tokens on a step, which modify how it is run and
/// compared.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Directives {
    pub kill_after: Option<std::time::Duration>,
    /// `@exit-only` — compare the exit code and ignore stdout/stderr.
    ///
    /// For the handful of steps whose *output* is explicitly L2 while the exit
    /// code is L0: Commander's usage errors and help layout (PARITY.md §1.3,
    /// §3). Clap will never reproduce Commander's wording, so comparing the
    /// text would make those steps permanently red for a reason the contract
    /// says not to care about. Marking the step also declares the usage error
    /// as deliberate, exempting it from the vacuity guard.
    pub exit_only: bool,
    /// `@exit-only:<reason>` — the same, with the author stating why.
    ///
    /// The reason exists for the lint's benefit. A bare `@exit-only` on a step
    /// the oracle happily runs is almost always someone silencing a real diff,
    /// so the lint objects to it; a reason is the author saying "no, this
    /// output genuinely is L2", which the lint then accepts. It is also the
    /// one place a reader of the scenario finds out which L2 clause applies.
    pub exit_only_reason: bool,
    /// `@unordered:<reason>` — this step prints a JSON **array** whose element
    /// order is not contract, so the two sides are compared as multisets.
    ///
    /// The one case that needs it today is a `broadcast` fan-out: the oracle
    /// saves the messages with `Promise.all`, so the order they reach
    /// `_index.json` is a libuv scheduling artifact, and `MessageStore.list`
    /// sorts only by `created_at` — identical for every message in one
    /// fan-out — leaving `_index.json` order as the stable-sort tie-break.
    /// PARITY.md §6.10 states that index order is not contract.
    ///
    /// A bare `@unordered` is not accepted: the reason is the whole point, so
    /// it falls through as an unknown token and breaks the step loudly.
    pub unordered: bool,
    /// `@stop-after:<ms>` — run it, then **SIGTERM** it and wait for it to
    /// shut down.
    ///
    /// The graceful counterpart of `@kill-after`, for the scenarios whose
    /// subject is a watch-mode orchestrator: `serve` without `--once` never
    /// returns, and `serve --once` never returns either when a task lands in
    /// `review`, which is not terminal. A SIGTERM leaves the tree exactly as
    /// a real operator's Ctrl+C would — agents cleaned up, lock released —
    /// so the steps after it compare something meaningful, which a SIGKILL
    /// mid-write does not.
    pub stop_after: Option<std::time::Duration>,
    /// `@ticks-vary:<reason>` — drop `orchestrator:tick` records from this
    /// step's comparison.
    ///
    /// The tick stream is a heartbeat on a wall clock: how many ticks fire
    /// before a run completes, and which tasks are running when one does,
    /// differ between two independently-timed processes (measured: a
    /// `running: 2` tick on one side against `running: 1` on the other, and
    /// one extra tick before a stall is detected). Every *other* record in
    /// the stream is deterministic, so this drops exactly the heartbeat and
    /// keeps the lifecycle: dispatch, completion, retries, stalls, shutdown.
    /// The tick record's own shape is covered by the structured logger's unit
    /// tests.
    pub ticks_vary: bool,
}

impl Directives {
    /// Split leading directives from the argv. Unknown `@…` tokens are left in
    /// place so they surface as a bogus first argument rather than being
    /// silently ignored.
    pub fn parse(step: &str) -> (Directives, &str) {
        let mut directives = Directives::default();
        let mut rest = step.trim_start();

        loop {
            let (token, tail) = match rest.split_once(' ') {
                Some((token, tail)) => (token, tail.trim_start()),
                None => (rest, ""),
            };
            match token {
                "@exit-only" => directives.exit_only = true,
                _ if token.starts_with("@exit-only:") => {
                    directives.exit_only = true;
                    directives.exit_only_reason = true;
                }
                _ if token.starts_with("@unordered:") => directives.unordered = true,
                _ if token.starts_with("@ticks-vary:") => directives.ticks_vary = true,
                _ if token.starts_with("@stop-after:") => {
                    match token["@stop-after:".len()..].parse::<u64>() {
                        Ok(millis) => {
                            directives.stop_after = Some(std::time::Duration::from_millis(millis));
                        }
                        Err(_) => return (directives, rest),
                    }
                }
                _ => match token.strip_prefix("@kill-after:") {
                    Some(millis) => match millis.parse::<u64>() {
                        Ok(millis) => {
                            directives.kill_after = Some(std::time::Duration::from_millis(millis));
                        }
                        Err(_) => return (directives, rest),
                    },
                    None => return (directives, rest),
                },
            }
            rest = tail;
        }
    }
}

/// A step that writes a file into the project instead of running a binary.
///
/// Some states are only reachable by putting bytes on disk: a lock file
/// holding garbage, for instance, which no command will produce but which
/// decides whether the orchestrator starts (PARITY.md §14). Written into both
/// trees identically, so a differential scenario stays differential.
pub const WRITE_STEP: &str = "@write";

/// Whether the oracle rejected a step before running it.
///
/// Commander writes its own errors as `error: …` on stderr, while every ORCH
/// domain error is rendered with the `✕` glyph by `printError`, so the prefix
/// is an unambiguous discriminator. Only the oracle is consulted: if
/// *Commander* never reached the command body then the step exercises argument
/// parsing and nothing else, whatever clap made of the same argv.
pub fn is_usage_error(stderr: &str) -> bool {
    stderr.lines().any(|line| line.starts_with("error: "))
}

/// `(command, subcommand)` pairs that mutate nothing, so they are safe to run
/// once per binary over a shared tree. Extend as WP-5 lands more read-only
/// commands (`task show`, `agent list`, `status`, `config get`, …).
const READ_ONLY_VERIFY_STEPS: &[(&str, &str)] = &[
    ("task", "list"),
    ("task", "show"),
    ("agent", "list"),
    ("agent", "status"),
    ("goal", "list"),
    ("goal", "show"),
    ("team", "list"),
    ("team", "show"),
    ("msg", "list"),
    ("msg", "inbox"),
    ("context", "list"),
    ("context", "get"),
    ("config", "get"),
    ("org", "list"),
    // `status` and `logs` take no subcommand, so the second argv slot is a
    // flag or an id rather than a verb; match on the command alone.
    ("status", ""),
    ("logs", ""),
];

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
    fn a_kill_after_directive_is_stripped_from_the_argv() {
        assert_eq!(
            Scenario::kill_after("@kill-after:500 serve --once"),
            Some(std::time::Duration::from_millis(500))
        );
        assert_eq!(Scenario::kill_after("serve --once"), None);

        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"differential\"\nsteps = [\"@kill-after:500 serve --once\"]\n",
        );
        let scenario = Scenario::load(&path).unwrap();
        assert_eq!(
            scenario.argv(&scenario.steps[0]).unwrap(),
            vec!["serve".to_owned(), "--once".to_owned()]
        );
    }

    #[test]
    fn a_write_step_needs_a_safe_relative_path() {
        let temp = tempfile::tempdir().unwrap();
        let body =
            |step: &str| format!("name = \"s\"\nkind = \"differential\"\nsteps = [{step:?}]\n");

        // The content lands in the scenario's tmpdir, so a path that escapes
        // it would write into the developer's own checkout.
        for bad in [
            "@write /etc/passwd oops",
            "@write ../../escape oops",
            "@write .orchestry/../../escape oops",
            "@write .orchestry/orchestry.lock",
            "@write .orchestry/orchestry.lock one two",
        ] {
            let path = write(temp.path(), "s.toml", &body(bad));
            assert!(Scenario::load(&path).is_err(), "{bad}");
        }

        let path = write(
            temp.path(),
            "s.toml",
            &body("@write .orchestry/orchestry.lock not-a-pid-at-all"),
        );
        let scenario = Scenario::load(&path).unwrap();
        assert_eq!(
            scenario.argv(&scenario.steps[0]).unwrap(),
            vec![
                "@write".to_owned(),
                ".orchestry/orchestry.lock".to_owned(),
                "not-a-pid-at-all".to_owned()
            ]
        );

        // An empty content argument is legal — an empty lock file reads the
        // same way as a garbage one, and the scenario covers both.
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"differential\"\n\
             steps = [\"@write .orchestry/orchestry.lock ''\"]\n",
        );
        let scenario = Scenario::load(&path).unwrap();
        assert_eq!(scenario.argv(&scenario.steps[0]).unwrap()[2], "");
    }

    #[test]
    fn exit_only_marks_a_step_whose_output_is_l2() {
        assert!(Scenario::exit_only("@exit-only task nosuchsub"));
        assert!(!Scenario::exit_only("task list"));

        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"differential\"\nsteps = [\"@exit-only task nosuchsub\"]\n",
        );
        let scenario = Scenario::load(&path).unwrap();
        assert_eq!(
            scenario.argv(&scenario.steps[0]).unwrap(),
            vec!["task".to_owned(), "nosuchsub".to_owned()]
        );
    }

    #[test]
    fn repeat_expands_at_parse_time() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"differential\"\n\
             steps = [\"msg send a '${repeat:x:5}\u{1F600}' -s '${repeat:y:3}'\"]\n",
        );
        let scenario = Scenario::load(&path).unwrap();
        let argv = scenario.argv(&scenario.steps[0]).unwrap();
        assert_eq!(argv[3], "xxxxx\u{1F600}");
        assert_eq!(argv[5], "yyy");
    }

    #[test]
    fn a_malformed_repeat_is_an_error_not_a_literal() {
        for arg in [
            "${repeat:x}",
            "${repeat:x:notanumber}",
            "${repeat:x:5",
            // 64 KiB guard: a count this large is a typo, not an intention.
            "${repeat:x:99999}",
        ] {
            assert!(expand_repeats(arg).is_err(), "{arg}");
        }
        assert_eq!(
            expand_repeats("no markers here").unwrap(),
            "no markers here"
        );
        // A zero count is legal and expands to nothing.
        assert_eq!(expand_repeats("a${repeat:x:0}b").unwrap(), "ab");
    }

    #[test]
    fn exit_only_can_carry_a_reason() {
        // The reason is what tells the lint apart a declared L2 output from
        // someone silencing a diff they did not want to look at.
        assert!(Scenario::exit_only("@exit-only:help-layout-is-l2 --help"));
        assert!(Scenario::exit_only_reason(
            "@exit-only:help-layout-is-l2 --help"
        ));
        assert!(!Scenario::exit_only_reason("@exit-only --help"));

        let (_, argv) = Directives::parse("@exit-only:help-layout-is-l2 --help");
        assert_eq!(argv, "--help");
    }

    #[test]
    fn directives_combine_in_either_order() {
        for step in [
            "@exit-only @kill-after:500 serve --once",
            "@kill-after:500 @exit-only serve --once",
        ] {
            let (directives, argv) = Directives::parse(step);
            assert!(directives.exit_only, "{step}");
            assert_eq!(
                directives.kill_after,
                Some(std::time::Duration::from_millis(500)),
                "{step}"
            );
            assert_eq!(argv, "serve --once", "{step}");
        }
    }

    #[test]
    fn an_unknown_directive_is_left_in_the_argv() {
        // Silently ignoring it would let a typo'd directive disable the
        // behaviour it was meant to request.
        let (directives, argv) = Directives::parse("@exit-onlyy task list");
        assert_eq!(directives, Directives::default());
        assert_eq!(argv, "@exit-onlyy task list");
    }

    #[test]
    fn commander_usage_errors_are_told_apart_from_domain_errors() {
        // Measured against the oracle: Commander writes `error: …`, while
        // every ORCH domain error is rendered with the `✕` glyph.
        for stderr in [
            "error: unknown option '--nosuchflag'\n",
            "error: unknown command 'nosuchsub'\n",
            "error: required option '--adapter <adapter>' not specified\n",
        ] {
            assert!(is_usage_error(stderr), "{stderr:?}");
        }
        for stderr in [
            "  ✕ Task not found: tsk_nosuch\n",
            "  ✕ Priority must be an integer between 1 and 4\n",
            "  ✕ Not initialized\n    Run: orch init\n",
            // A bare command group prints its help to stderr (measured —
            // PARITY.md §3 says stdout). It is not a Commander `error:` line.
            "Usage: orchestry task [options] [command]\n",
            "",
        ] {
            assert!(!is_usage_error(stderr), "{stderr:?}");
        }
    }

    #[test]
    fn a_malformed_kill_after_directive_is_rejected_at_load() {
        let temp = tempfile::tempdir().unwrap();
        let path = write(
            temp.path(),
            "s.toml",
            "name = \"s\"\nkind = \"differential\"\nsteps = [\"@kill-after:soon serve\"]\n",
        );
        // It falls through to argv parsing, which keeps the directive as a
        // bogus first argument rather than silently ignoring it.
        let scenario = Scenario::load(&path).unwrap();
        assert_eq!(
            scenario.argv(&scenario.steps[0]).unwrap()[0],
            "@kill-after:soon"
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
