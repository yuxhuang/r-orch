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

use crate::normalize::Normalizer;
use crate::scenario::{Kind, Runner, Scenario};

/// The version the oracle reports, so the seeded cache never looks like an
/// available update.
const ORACLE_VERSION: &str = "1.0.34";

#[derive(Debug, Clone)]
pub struct Binaries {
    /// `target/debug/orch`.
    pub rust: PathBuf,
    /// `parity/orch-ts/dist/cli.js`.
    pub oracle_entry: PathBuf,
    pub node: PathBuf,
}

impl Binaries {
    pub fn discover(repo_root: &Path) -> Result<Binaries> {
        // `CARGO_TARGET_DIR` is shared between worktrees here, so the binary
        // is not necessarily under this repo's own `target/`.
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo_root.join("target"));
        let rust = target.join("debug/orch");
        if !rust.exists() {
            bail!(
                "{} is missing — run `cargo build -p orch-cli` first",
                rust.display()
            );
        }
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
        })
    }
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
}

impl ScenarioResult {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
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

pub fn run_scenario(bins: &Binaries, scenario: &Scenario) -> Result<ScenarioResult> {
    let mut failures = Vec::new();

    if scenario.kind.is_cross_compat() {
        // One shared directory: the two binaries take turns in it. There is
        // nothing to compare *between* trees, so what is asserted is that
        // every step succeeds and that the artifacts stay readable by both —
        // which the js-yaml cross-parse below does.
        let project = Project::create(&scenario.name)?;
        let plan = scenario.plan();
        let mut outputs = Vec::new();

        for (step, runner) in scenario.steps.iter().zip(&plan) {
            let output = execute(bins, &project, scenario, step, *runner)?;
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
            let rust_out = execute(bins, &project, scenario, step, Runner::Rust)?;
            let ts_out = execute(bins, &project, scenario, step, Runner::Ts)?;

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
        });
    }
    {
        let rust = Project::create(&format!("{}-rust", scenario.name))?;
        let ts = Project::create(&format!("{}-ts", scenario.name))?;

        let mut rust_norm = Normalizer::new().with_root(&rust.root, "<ROOT>");
        let mut ts_norm = Normalizer::new().with_root(&ts.root, "<ROOT>");

        for step in &scenario.steps {
            let rust_out = execute(bins, &rust, scenario, step, Runner::Rust)?;
            let ts_out = execute(bins, &ts, scenario, step, Runner::Ts)?;

            // Learn ids in step order, so the Nth created entity maps to the
            // same placeholder on both sides.
            rust_norm.learn(&rust_out.stdout);
            rust_norm.learn(&rust_out.stderr);
            ts_norm.learn(&ts_out.stdout);
            ts_norm.learn(&ts_out.stderr);

            if rust_out.exit_code != ts_out.exit_code {
                failures.push(format!(
                    "step `{step}`: exit code {} (rust) vs {} (ts)",
                    rust_out.exit_code, ts_out.exit_code
                ));
            }
            for (channel, rust_text, ts_text) in [
                ("stdout", &rust_out.stdout, &ts_out.stdout),
                ("stderr", &rust_out.stderr, &ts_out.stderr),
            ] {
                let left = rust_norm.text(rust_text);
                let right = ts_norm.text(ts_text);
                if left != right {
                    failures.push(format!(
                        "step `{step}`: {channel} differs\n{}",
                        unified_diff(&right, &left)
                    ));
                }
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
        })
    }
}

/// Real id → placeholder, in first-seen order.
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
) -> Result<StepOutput> {
    let argv = scenario.argv(step)?;
    let (mut command, label) = match runner {
        Runner::Rust | Runner::Both => (Command::new(&bins.rust), "rust"),
        Runner::Ts => {
            let mut command = Command::new(&bins.node);
            command.arg(&bins.oracle_entry);
            (command, "ts")
        }
    };

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

    let output = command
        .output()
        .with_context(|| format!("run {label} step `{step}` in {}", project.root.display()))?;

    Ok(StepOutput {
        step: step.to_owned(),
        runner: label,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
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
