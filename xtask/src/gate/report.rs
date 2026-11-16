//! Real verdicts/statuses, fixed lifecycle diagnostics, bounded evidence.
use super::{files, inventory};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub(super) const MAX_FILES: usize = 32;
pub(super) const MAX_BUNDLE: usize = 64 * 1024 * 1024;
const MAX_STRUCTURED: usize = 1024 * 1024;
pub(super) const STEPS: &[&str] = &[
    "build",
    "test",
    "clippy",
    "fmt",
    "advisories",
    "crate-graph",
    "home-hygiene",
    "no-real-cli",
    "real-home",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Pass,
    Fail,
    EnvLimited,
}
impl Verdict {
    pub(super) fn text(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "FAIL",
            Self::EnvLimited => "ENV-LIMITED",
        }
    }
}
#[derive(Debug)]
pub(super) struct Step {
    pub name: &'static str,
    pub verdict: Verdict,
    pub exit: Option<i32>,
    pub elapsed: Duration,
    pub detail: String,
}

pub(super) fn code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}
pub(super) fn exit(steps: &[Step], evidence_nonqualifying: bool) -> i32 {
    if steps.iter().any(|s| s.verdict == Verdict::Fail) {
        1
    } else if evidence_nonqualifying || steps.iter().any(|s| s.verdict == Verdict::EnvLimited) {
        3
    } else {
        0
    }
}
pub(super) fn table(steps: &[Step]) -> String {
    let mut text = String::from("status       step             wall(s) exit detail\n");
    for s in steps {
        text.push_str(&format!(
            "{:<12} {:<16} {:>7.3} {:>4} {}\n",
            s.verdict.text(),
            s.name,
            s.elapsed.as_secs_f64(),
            s.exit.map_or("unknown".into(), |e| e.to_string()),
            diagnostic(s.detail.as_bytes())
        ));
    }
    text.push_str(&format!(
        "total-step-wall: {:.3}s\n",
        steps
            .iter()
            .fold(0.0, |total, s| total + s.elapsed.as_secs_f64())
    ));
    text
}

// Unvalidated text is never terminal control or shell syntax. No process id in
// this text becomes authority to delete, signal, wait for, or adopt a writer.
pub(super) fn diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(4096)])
        .chars()
        .flat_map(char::escape_default)
        .collect()
}
fn marker_detail(marker: &Path, diagnostics: Option<&[u8]>) -> String {
    format!(
        "marker: {}\nmarker diagnostics (unvalidated): {}\nIf a marker is present, the target may be in use or an earlier use did not finish.\ninspection (read-only): use lsof with the marker path as a single argument; diagnostics are not process authority\nDo not remove target/ with cargo clean or by hand while a process of an earlier use is alive.\n",
        diagnostic(marker.as_os_str().as_encoded_bytes()),
        diagnostics.map_or_else(|| "unavailable".into(), diagnostic)
    )
}
pub(super) fn marker_refusal(marker: &Path, diagnostics: Option<&[u8]>, why: &str) -> String {
    let why = diagnostic(why.as_bytes());
    format!(
        "REFUSED: {why}\n{}qualifying: no: {why}\n",
        marker_detail(marker, diagnostics)
    )
}
pub(super) fn kept(marker: &Path, why: &str) -> String {
    let why = diagnostic(why.as_bytes());
    format!(
        "target: kept ({why})\nqualifying: no: {why}\n{}",
        marker_detail(marker, None)
    )
}

pub(super) struct Redactor {
    replacements: Vec<(String, &'static str)>,
    sensitive: Vec<String>,
    unsupported_sensitive: bool,
}
impl Redactor {
    pub(super) fn new(repo: &Path, home: &Path, temporary: &Path, hostname: Option<&str>) -> Self {
        let mut replacements: Vec<_> = [(repo, "<REPO>"), (home, "<HOME>"), (temporary, "<TMP>")]
            .into_iter()
            .filter_map(|(p, to)| p.to_str().map(|s| (s.to_owned(), to)))
            .filter(|(s, _)| s.len() > 1)
            .collect();
        if let Some(name) = hostname.filter(|s| !s.is_empty()) {
            replacements.push((name.to_owned(), "<HOST>"));
        }
        let mut sensitive = Vec::new();
        let mut unsupported_sensitive = false;
        for (name, value) in std::env::vars_os() {
            let name = name.to_string_lossy().to_ascii_uppercase();
            if ["TOKEN", "SECRET", "PASSWORD", "API_KEY"]
                .iter()
                .any(|part| name.contains(part))
                || super::hermetic::SESSION.contains(&name.as_str())
                || (name.starts_with("XTASK_GATE_") && name != "XTASK_GATE_JUDGE_ID")
            {
                match value.to_str() {
                    Some("") => {}
                    // Replacing very short values can corrupt identities and
                    // verdict syntax. Refuse rather than silently omit them.
                    Some(value) if value.len() < 4 => unsupported_sensitive = true,
                    Some(value) => {
                        sensitive.push(value.to_owned());
                        replacements.push((value.to_owned(), "<REDACTED>"));
                    }
                    None => unsupported_sensitive = true,
                }
            }
        }
        // Build/map logs are JSON strings; result details use escape_default.
        // Replace their encoded forms before their literal prefixes/substrings.
        for (value, to) in replacements.clone() {
            let json = serde_json::to_string(&value).expect("string serialization");
            replacements.push((json[1..json.len() - 1].to_owned(), to));
            replacements.push((value.chars().flat_map(char::escape_default).collect(), to));
        }
        replacements.sort_by_key(|(s, _)| std::cmp::Reverse(s.len()));
        Self {
            replacements,
            sensitive,
            unsupported_sensitive,
        }
    }
    pub(super) fn apply(&self, bytes: &[u8]) -> Result<String> {
        ensure!(
            !self.unsupported_sensitive,
            "evidence redaction refused: nonempty sensitive value is too short or not UTF-8"
        );
        let mut text = std::str::from_utf8(bytes)
            .context("evidence is not UTF-8")?
            .to_owned();
        for (from, to) in &self.replacements {
            text = text.replace(from, to);
        }
        // Preserve line/tab layout, escape other terminal controls.
        let text: String = text
            .chars()
            .flat_map(|c| {
                if c.is_control() && !['\n', '\t'].contains(&c) {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            })
            .collect();
        self.check_sensitive(&text)?;
        Ok(text)
    }
    fn check_sensitive(&self, text: &str) -> Result<()> {
        ensure!(
            !self.contains_sensitive(text),
            "evidence redaction refused: sensitive value remains"
        );
        // Alternate JSON encodings or a colliding replacement must fail before
        // publication, never produce a decodable credential in an export.
        for line in text.lines() {
            if let Some(value) = Self::decoded_json(line)? {
                self.check_json(&value, 0)?;
            }
        }
        Ok(())
    }
    fn contains_sensitive(&self, text: &str) -> bool {
        self.sensitive.iter().any(|secret| text.contains(secret))
            || self
                .replacements
                .iter()
                .any(|(from, to)| *to == "<REDACTED>" && text.contains(from))
    }
    fn decoded_json(text: &str) -> Result<Option<serde_json::Value>> {
        match serde_json::from_str(text) {
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                // serde_json reports its recursion budget as a syntax error.
                // Treating that valid but unsupported JSON as plain text can
                // conceal another encoded string. The pinned parser's error
                // prefix is covered by an actual over-budget export control.
                ensure!(
                    !error.to_string().starts_with("recursion limit exceeded"),
                    "evidence redaction refused: JSON decoding depth budget"
                );
                Ok(None)
            }
        }
    }
    fn check_json_string(&self, text: &str, depth: usize) -> Result<()> {
        ensure!(
            !self.contains_sensitive(text),
            "evidence redaction refused: decoded sensitive value remains"
        );
        // Tools can print JSON which the build-log wrapper encodes again.
        // Inspect decoded JSON text as well, with a bounded total nesting depth.
        if let Some(value) = Self::decoded_json(text)? {
            self.check_json(&value, depth + 1)?;
        } else {
            for line in text.lines().filter(|line| *line != text) {
                if let Some(value) = Self::decoded_json(line)? {
                    self.check_json(&value, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    fn check_json(&self, value: &serde_json::Value, depth: usize) -> Result<()> {
        ensure!(
            depth <= 128,
            "evidence redaction refused: decoded nesting budget"
        );
        match value {
            serde_json::Value::String(s) => self.check_json_string(s, depth)?,
            serde_json::Value::Array(values) => {
                for value in values {
                    self.check_json(value, depth + 1)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    self.check_json_string(key, depth)?;
                    self.check_json(value, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

pub(super) fn blob(repo: &Path, bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    // Hash inputs are bounded by the evidence budgets. These small ordinary git
    // helpers do not establish target lifecycle completion or timing bounds.
    child.stdin.take().context("hash stdin")?.write_all(bytes)?;
    let output = child.wait_with_output()?;
    ensure!(output.status.success(), "git hash-object failed");
    let id = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(
        [40, 64].contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid blob id"
    );
    Ok(id)
}

fn bundle_name(name: &str) -> bool {
    [
        "result.txt",
        "inventory.tsv",
        "environment.txt",
        "tools.txt",
    ]
    .contains(&name)
        || name
            .strip_prefix("steps/")
            .and_then(|s| s.strip_suffix(".log"))
            .is_some_and(|s| STEPS.contains(&s))
}

pub(super) fn export(
    repo: &Path,
    destination: &Path,
    entries: &BTreeMap<String, Vec<u8>>,
    redactor: &Redactor,
) -> Result<String> {
    ensure!(
        entries.len() <= MAX_FILES && entries.values().map(Vec::len).sum::<usize>() <= MAX_BUNDLE,
        "evidence budget exceeded"
    );
    ensure!(
        entries.keys().all(|s| bundle_name(s)),
        "unexpected evidence file"
    );
    // Validate the complete copy before creating any externally shareable
    // directory. A late encoding/redaction failure leaves no partial bundle.
    let mut redacted = BTreeMap::new();
    let mut total = 0;
    for (name, bytes) in entries {
        let text = redactor.apply(bytes)?;
        total += text.len();
        ensure!(
            text.len() <= inventory::MAX_LOG && total <= MAX_BUNDLE,
            "redacted evidence budget exceeded"
        );
        redacted.insert(name, text);
    }
    let mut manifest = String::new();
    for (name, text) in &redacted {
        manifest.push_str(&format!("{}  {name}\n", blob(repo, text.as_bytes())?));
    }
    let digest = blob(repo, manifest.as_bytes())?;
    // Manifest names and digests are structural: redact neither. A collision
    // with sensitive material must refuse the whole export before any write.
    for structural in [&manifest, &digest, "MANIFEST\nsteps\n"] {
        redactor
            .check_sensitive(structural)
            .context("evidence redaction refused: manifest collision")?;
    }
    let parent = destination
        .parent()
        .context("evidence parent")?
        .canonicalize()?;
    files::real_directories(&parent)?;
    ensure!(
        !destination.try_exists()?,
        "evidence destination already exists"
    );
    std::fs::DirBuilder::new().create(destination)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    std::fs::create_dir(destination.join("steps"))?;
    for (name, text) in redacted {
        files::write(&destination.join(name), text.as_bytes())?;
    }
    files::write(&destination.join("MANIFEST"), manifest.as_bytes())?;
    files::sync(destination)?;
    Ok(digest)
}

pub(super) fn verify(repo: &Path, directory: &Path, commit: &str) -> Result<()> {
    ensure!(
        [40, 64].contains(&commit.len()) && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected commit must be a full object id"
    );
    // Canonicalization first would erase a linked bundle root. Check every
    // component before resolving it or reading any evidence through it.
    let directory = std::path::absolute(directory)?;
    files::real_directories(&directory)?;
    let manifest = files::text(&directory.join("MANIFEST"), 16 * 1024)?;
    let mut entries = BTreeMap::new();
    let mut total = 0;
    for line in manifest.lines() {
        let (digest, name) = line.split_once("  ").context("MANIFEST row")?;
        ensure!(
            bundle_name(name) && entries.len() < MAX_FILES && !entries.contains_key(name),
            "MANIFEST path or duplicate: {name}"
        );
        if name.starts_with("steps/") {
            files::real_directories(&directory.join("steps"))?;
        }
        let structured = [
            "result.txt",
            "environment.txt",
            "tools.txt",
            "inventory.tsv",
        ]
        .contains(&name);
        let limit = if structured && name != "inventory.tsv" {
            MAX_STRUCTURED
        } else {
            inventory::MAX_LOG
        };
        let bytes = files::read(&directory.join(name), limit)?;
        ensure!(!structured || !bytes.contains(&0), "NUL in {name}");
        total += bytes.len();
        ensure!(
            total <= MAX_BUNDLE && blob(repo, &bytes)? == digest,
            "digest mismatch: {name}"
        );
        entries.insert(name.to_owned(), String::from_utf8(bytes)?);
    }
    for name in [
        "result.txt",
        "inventory.tsv",
        "environment.txt",
        "tools.txt",
    ] {
        ensure!(entries.contains_key(name), "missing {name}");
    }
    // An unmanifested file is not evidence. Inspect just the two permitted
    // directory levels, never recurse through arbitrary entries or links.
    for (path, prefix) in [(&directory, ""), (&directory.join("steps"), "steps/")] {
        files::real_directories(path)?;
        for (count, entry) in std::fs::read_dir(path)?.enumerate() {
            ensure!(count <= MAX_FILES, "evidence file count budget");
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().context("evidence file name UTF-8")?;
            if prefix.is_empty() && ["MANIFEST", "steps"].contains(&name) {
                continue;
            }
            ensure!(
                entries.contains_key(&format!("{prefix}{name}")),
                "unexpected unmanifested file: {prefix}{name}"
            );
        }
    }
    let mut tool_fields = BTreeMap::new();
    for line in entries["tools.txt"].lines() {
        let (name, value) = line.split_once(':').context("tools.txt field")?;
        ensure!(
            !name.is_empty() && tool_fields.insert(name, value.trim()).is_none(),
            "duplicate or empty tools.txt field {name}"
        );
    }
    let result = &entries["result.txt"];
    let mut fields = BTreeMap::new();
    for line in result.lines().take_while(|s| !s.is_empty()) {
        let (name, value) = line.split_once(':').context("result field")?;
        ensure!(
            fields.insert(name, value.trim()).is_none(),
            "duplicate result field {name}"
        );
    }
    ensure!(
        fields.get("qualifying") == Some(&"yes"),
        "qualifying: evidence is nonqualifying"
    );
    ensure!(
        fields.get("commit") == Some(&commit) && fields.get("head-end") == Some(&commit),
        "commit or head-end mismatch"
    );
    for (name, expected) in [
        ("dirty", "no"),
        ("dirty-end", "no"),
        ("moved", "no"),
        ("part", "-"),
        ("lease-default", "yes"),
        ("lease-path", "default"),
        ("lease-override", "no"),
        ("lease-held-through-teardown", "yes"),
        ("lease-lost", "no"),
        ("lease-holder-live", "yes"),
        ("lease-token-match", "yes"),
        ("pinned-toolchain", "yes"),
        ("target-finished", "yes"),
    ] {
        ensure!(
            fields.get(name) == Some(&expected),
            "evidence predicate {name}"
        );
    }
    ensure!(
        matches!(
            (
                fields.get("lease").copied(),
                fields.get("lease-authority").copied()
            ),
            (Some("exclusive"), Some("kernel-exclusive"))
                | (Some("inherited"), Some("live-exclusive-holder"))
        ),
        "evidence predicate lease authority"
    );
    let target = fields.get("target").context("missing target outcome")?;
    ensure!(
        ["created", "reused", "deleted"].contains(target)
            || target
                .strip_prefix("reset: ")
                .is_some_and(|reason| !reason.is_empty()),
        "target outcome did not finish"
    );
    let tier = fields.get("tier").context("missing tier")?;
    ensure!(
        ["full", "pr"].contains(tier)
            && fields.get("verdict") == Some(&format!("PASSED (tier {tier})").as_str()),
        "verdict/tier"
    );
    ensure!(
        fields.get("judge-source-id") == Some(&super::judge::at_commit(repo, commit)?.as_str()),
        "judge-source-id mismatch"
    );
    let tree = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", &format!("{commit}^{{tree}}")])
        .output()?;
    ensure!(tree.status.success(), "expected commit tree unavailable");
    let tree = String::from_utf8(tree.stdout)?;
    for name in ["tree", "tree-start", "tree-end"] {
        ensure!(fields.get(name) == Some(&tree.trim()), "{name} mismatch");
    }
    for name in STEPS {
        ensure!(
            fields.get(format!("step-{name}-status").as_str()) == Some(&"pass"),
            "{name} status"
        );
        ensure!(
            fields.get(format!("step-{name}-exit").as_str()) == Some(&"0"),
            "{name} exit"
        );
        let wall = fields
            .get(format!("step-{name}-wall").as_str())
            .context("missing wall time")?
            .parse::<f64>()?;
        ensure!(wall.is_finite() && wall >= 0., "{name} wall");
        ensure!(
            entries.contains_key(&format!("steps/{name}.log")),
            "missing step {name} log"
        );
    }
    let rebuilt = inventory::parse(&entries["steps/test.log"], &entries["steps/build.log"], 0)?;
    ensure!(
        inventory::tsv(&rebuilt) == entries["inventory.tsv"],
        "inventory differs from step logs"
    );
    let environment = &entries["environment.txt"];
    for line in environment.lines() {
        let (action, name) = line.split_once(' ').context("environment names only")?;
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
            "environment name"
        );
        ensure!(
            action == "removed" || (action == "set" && super::hermetic::SET.contains(&name)),
            "environment allow-list"
        );
    }
    ensure!(
        super::hermetic::SET
            .iter()
            .all(|s| environment.lines().any(|line| line == format!("set {s}"))),
        "environment set names missing"
    );
    ensure!(
        entries["tools.txt"]
            .lines()
            .any(|s| s.starts_with("rustc: rustc "))
            && entries["tools.txt"]
                .lines()
                .any(|s| s.starts_with("cargo: cargo "))
            && entries["tools.txt"]
                .lines()
                .any(|s| s.starts_with("cargo-deny: cargo-deny ")),
        "tools versions missing"
    );
    ensure!(
        ["complete", "partial"].contains(
            fields
                .get("real-home-coverage")
                .context("real-home-coverage missing")?
        ),
        "real-home-coverage"
    );
    let inv = &entries["inventory.tsv"];
    ensure!(
        !inv.is_empty()
            && inv.lines().all(|s| {
                let f: Vec<_> = s.split('\t').collect();
                f.len() == 5 && ["ok", "ignored"].contains(&f[4])
            })
            && inv.lines().any(|s| s.ends_with("\tok")),
        "inventory missing, failed or no test executed"
    );
    Ok(())
}

pub(super) fn write_result(repo: &Path, text: &str) -> Result<PathBuf> {
    let target = repo.join("target");
    if !target.try_exists()? {
        std::fs::create_dir(&target)?;
    }
    files::real_directories(&target)?;
    let run = files::private_dir(&target, "gate-report")?;
    files::write(&run.join("result.txt"), text.as_bytes())?;
    let latest = target.join("gate-result.txt");
    if let Ok(meta) = std::fs::symlink_metadata(&latest) {
        ensure!(
            meta.is_file() && meta.len() <= 128 * 1024,
            "result leaf refused"
        );
    }
    // Atomic diagnostic replacement, never a target or lifecycle-state mutation.
    std::fs::copy(run.join("result.txt"), run.join("latest.tmp"))?;
    std::fs::rename(run.join("latest.tmp"), latest)?;
    files::sync(&target)?;
    Ok(run)
}

/// A report is reserved before admission. It survives failure and interruption;
/// only complete, validated old report directories are eligible for retention.
pub(super) struct Run {
    pub(super) path: PathBuf,
}
impl Run {
    pub(super) fn begin(repo: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let target = repo.join("target");
        if !target.try_exists()? {
            std::fs::DirBuilder::new().mode(0o700).create(&target)?;
        }
        files::real_directories(&target)?;
        let runs = target.join("gate-runs");
        if !runs.try_exists()? {
            std::fs::DirBuilder::new().mode(0o700).create(&runs)?;
        }
        files::real_directories(&runs)?;
        let path = files::private_dir(&runs, "run")?;
        files::write(
            &path.join("result.txt"),
            b"verdict: IN PROGRESS\nqualifying: no: run not finished\n",
        )?;
        let run = Self { path };
        run.latest(repo)?;
        Ok(run)
    }
    fn latest(&self, repo: &Path) -> Result<()> {
        let target = repo.join("target");
        files::real_directories(&target)?;
        let latest = target.join("gate-result.txt");
        if let Ok(meta) = std::fs::symlink_metadata(&latest) {
            ensure!(
                meta.is_file() && meta.len() <= 128 * 1024,
                "result leaf refused"
            );
        }
        let tmp = self.path.join("latest.tmp");
        files::write(
            &tmp,
            &files::read(&self.path.join("result.txt"), 128 * 1024)?,
        )?;
        std::fs::rename(tmp, latest)?;
        files::sync(&target)
    }
    pub(super) fn finish(
        &self,
        repo: &Path,
        entries: &BTreeMap<String, Vec<u8>>,
        commands: &str,
    ) -> Result<()> {
        files::real_directories(&self.path)?;
        std::fs::create_dir(self.path.join("steps"))?;
        for (name, bytes) in entries {
            ensure!(bundle_name(name), "run entry name");
            if name == "result.txt" {
                continue;
            }
            files::write(&self.path.join(name), bytes)?;
        }
        files::write(&self.path.join("commands.txt"), commands.as_bytes())?;
        files::write(
            &self.path.join("result.tmp"),
            entries.get("result.txt").context("result missing")?,
        )?;
        std::fs::rename(self.path.join("result.tmp"), self.path.join("result.txt"))?;
        files::write(&self.path.join("complete"), b"schema=1\n")?;
        files::sync(&self.path)?;
        self.latest(repo)?;
        self.retain()
    }
    fn retain(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let parent = self.path.parent().unwrap();
        let mut eligible = vec![];
        for entry in std::fs::read_dir(parent)? {
            let entry = entry?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if !entry.file_name().to_string_lossy().starts_with("run-")
                || !meta.is_dir()
                || meta.uid() != nix::unistd::geteuid().as_raw()
            {
                continue;
            }
            if files::text(&path.join("complete"), 64).ok().as_deref() != Some("schema=1\n") {
                continue;
            }
            let Ok(result) = files::text(&path.join("result.txt"), 128 * 1024) else {
                continue;
            };
            if result.starts_with("verdict: IN PROGRESS") {
                continue;
            }
            let mut leaves = vec![];
            let mut valid = true;
            for child in std::fs::read_dir(&path)? {
                let child = child?;
                let name = child.file_name();
                let name = name.to_string_lossy();
                let m = std::fs::symlink_metadata(child.path())?;
                if m.uid() != meta.uid() {
                    valid = false;
                    break;
                }
                if name == "steps" && m.is_dir() {
                    for leaf in std::fs::read_dir(child.path())? {
                        let leaf = leaf?;
                        let m = std::fs::symlink_metadata(leaf.path())?;
                        if !m.is_file()
                            || m.nlink() != 1
                            || m.uid() != meta.uid()
                            || !bundle_name(&format!(
                                "steps/{}",
                                leaf.file_name().to_string_lossy()
                            ))
                        {
                            valid = false;
                            break;
                        }
                        leaves.push(leaf.path());
                    }
                } else if m.is_file()
                    && m.nlink() == 1
                    && (bundle_name(&name) || name == "commands.txt" || name == "complete")
                {
                    leaves.push(child.path());
                } else {
                    valid = false;
                    break;
                }
            }
            if valid {
                eligible.push((meta.modified()?, path, leaves));
            }
        }
        eligible.sort_by_key(|(time, _, _)| *time);
        let remove = eligible.len().saturating_sub(10);
        for (_, path, leaves) in eligible.into_iter().take(remove) {
            if path == self.path {
                continue;
            }
            // Revalidate every leaf immediately before by-name deletion. D45's
            // accepted replacement race remains; no arbitrary recursive delete.
            for leaf in leaves {
                let m = std::fs::symlink_metadata(&leaf)?;
                ensure!(
                    m.is_file() && m.uid() == nix::unistd::geteuid().as_raw() && m.nlink() == 1,
                    "retention leaf changed"
                );
                std::fs::remove_file(leaf)?;
            }
            let steps = path.join("steps");
            files::real_directories(&steps)?;
            std::fs::remove_dir(steps)?;
            files::real_directories(&path)?;
            std::fs::remove_dir(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::steps::tests::Scratch;

    #[test]
    fn report_reservation_is_in_progress_and_retention_keeps_ten_complete_runs() {
        let scratch = Scratch::new();
        let pending = Run::begin(&scratch.0).unwrap();
        assert!(
            files::text(&scratch.0.join("target/gate-result.txt"), 4096)
                .unwrap()
                .contains("IN PROGRESS")
        );
        let unknown = scratch.0.join("target/gate-runs/unknown");
        std::fs::create_dir(&unknown).unwrap();
        let mut last = None;
        for _ in 0..12 {
            let run = Run::begin(&scratch.0).unwrap();
            let entries = BTreeMap::from([
                (
                    "result.txt".into(),
                    b"verdict: FAILED (part suite)\nqualifying: no: controlled failure\n".to_vec(),
                ),
                ("environment.txt".into(), Vec::new()),
                ("tools.txt".into(), Vec::new()),
                ("inventory.tsv".into(), Vec::new()),
                ("steps/test.log".into(), b"retained failure\n".to_vec()),
            ]);
            run.finish(&scratch.0, &entries, "exit=101\n").unwrap();
            last = Some(run.path);
        }
        let dirs: Vec<_> = std::fs::read_dir(scratch.0.join("target/gate-runs"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(dirs.len(), 12); // ten completed, one pending, one unknown
        assert!(pending.path.join("result.txt").is_file());
        assert!(unknown.is_dir());
        let last = last.unwrap();
        assert_eq!(
            files::text(&last.join("steps/test.log"), 4096).unwrap(),
            "retained failure\n"
        );
        assert_eq!(
            files::text(&scratch.0.join("target/gate-result.txt"), 4096).unwrap(),
            files::text(&last.join("result.txt"), 4096).unwrap()
        );
    }

    // Deliberately synthetic, file-only verifier control, NOT a qualifying run.
    #[test]
    fn synthetic_evidence_integrity_and_predicate_mutations() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let head = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(head.status.success());
        let commit = String::from_utf8(head.stdout).unwrap().trim().to_owned();
        let judge = super::super::judge::at_commit(repo, &commit).unwrap();
        let tree = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["rev-parse", "HEAD^{tree}"])
            .output()
            .unwrap();
        assert!(tree.status.success());
        let tree = String::from_utf8(tree.stdout).unwrap().trim().to_owned();
        let mut result = format!(
            "verdict: PASSED (tier pr)\ncommit: {commit}\nhead-end: {commit}\ntree: {tree}\ntree-start: {tree}\ntree-end: {tree}\ntarget: reused\nlease-path: default\nlease-override: no\nlease-held-through-teardown: yes\nlease-lost: no\nlease-holder-live: yes\nlease-token-match: yes\nlease-authority: kernel-exclusive\ntier: pr\npart: -\ndirty: no\ndirty-end: no\nmoved: no\nlease: exclusive\nlease-default: yes\npinned-toolchain: yes\ntarget-finished: yes\nreal-home-coverage: complete\njudge-source-id: {judge}\nqualifying: yes\n"
        );
        let mut entries = BTreeMap::new();
        for step in STEPS {
            result.push_str(&format!(
                "step-{step}-status: pass\nstep-{step}-exit: 0\nstep-{step}-wall: 0.1\n"
            ));
            entries.insert(format!("steps/{step}.log"), vec![]);
        }
        result.push('\n');
        let map=serde_json::json!({"reason":"compiler-artifact","package_id":"path+file:///fixture#fixture@0.1.0","profile":{"test":true},"executable":"/fixture/target/test","target":{"kind":["lib"],"name":"fixture"}}).to_string();
        let log = "Running unittests src/lib.rs (/fixture/target/test)\nrunning 1 test\ntest example ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s\n";
        entries.insert("steps/build.log".into(), map.as_bytes().to_vec());
        entries.insert("steps/test.log".into(), log.as_bytes().to_vec());
        entries.insert(
            "inventory.tsv".into(),
            b"lib\tfixture\tfixture\texample\tok\n".to_vec(),
        );
        entries.insert(
            "environment.txt".into(),
            super::super::hermetic::SET
                .iter()
                .map(|s| format!("set {s}\n"))
                .collect::<String>()
                .into_bytes(),
        );
        entries.insert(
            "tools.txt".into(),
            b"rustc: rustc 1.99.1\ncargo: cargo 1.99.1\ncargo-deny: cargo-deny synthetic\n"
                .to_vec(),
        );
        entries.insert("result.txt".into(), result.as_bytes().to_vec());
        let root = Scratch::new();
        let redactor = Redactor::new(
            Path::new("/repo"),
            Path::new("/home"),
            Path::new("/temp"),
            None,
        );
        export(repo, &root.0.join("positive"), &entries, &redactor).unwrap();
        verify(repo, &root.0.join("positive"), &commit).unwrap();
        for (i, (from, to, reason)) in [
            ("qualifying: yes", "qualifying: no: partial", "qualifying"),
            (
                "qualifying: yes",
                "qualifying: yes\nschema: 1\nschema: 1",
                "duplicate",
            ),
            ("step-test-exit: 0", "step-test-exit: 101", "test exit"),
            (
                "step-test-status: pass",
                "step-test-status: FAIL",
                "test status",
            ),
            ("head-end: ", "head-end: wrong", "head-end"),
            ("judge-source-id: ", "judge-source-id: wrong", "judge"),
            ("tree-end: ", "tree-end: wrong", "tree"),
            ("target: reused", "target: kept (unreaped child)", "target"),
            ("lease-override: no", "lease-override: yes", "lease"),
            ("lease-lost: no", "lease-lost: yes", "lease"),
            ("lease-path: default", "lease-path: /override", "lease"),
            ("lease-holder-live: yes", "lease-holder-live: no", "lease"),
            ("lease-token-match: yes", "lease-token-match: no", "lease"),
            (
                "lease-held-through-teardown: yes",
                "lease-held-through-teardown: no",
                "lease",
            ),
            (
                "lease-authority: kernel-exclusive",
                "lease-authority: diagnostic-only",
                "lease",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            entries.insert("result.txt".into(), result.replace(from, to).into_bytes());
            let path = root.0.join(format!("negative-{i}"));
            export(repo, &path, &entries, &redactor).unwrap();
            assert!(
                verify(repo, &path, &commit)
                    .unwrap_err()
                    .to_string()
                    .contains(reason)
            );
        }
        // Parser success is not suite success: a structurally valid failed
        // inventory cannot verify even when every success alias is forged green.
        entries.insert("result.txt".into(), result.as_bytes().to_vec());
        entries.insert(
            "steps/test.log".into(),
            log.replace("example ... ok", "example ... FAILED")
                .replace("ok. 1 passed; 0 failed", "FAILED. 0 passed; 1 failed")
                .into_bytes(),
        );
        entries.insert(
            "inventory.tsv".into(),
            b"lib\tfixture\tfixture\texample\tfailed\n".to_vec(),
        );
        let failed = root.0.join("failed-suite");
        export(repo, &failed, &entries, &redactor).unwrap();
        assert!(
            verify(repo, &failed, &commit)
                .unwrap_err()
                .to_string()
                .contains("process exit")
        );
        entries.insert("steps/test.log".into(), log.as_bytes().to_vec());
        entries.insert(
            "inventory.tsv".into(),
            b"lib\tfixture\tfixture\texample\tok\n".to_vec(),
        );

        let tools = String::from_utf8(entries["tools.txt"].clone()).unwrap();
        let bounded = format!(
            "{tools}git: git version {}\n",
            "x".repeat(1024 * 1024 - tools.len() - "git: git version \n".len())
        );
        assert_eq!(bounded.len(), 1024 * 1024);
        entries.insert("tools.txt".into(), bounded.as_bytes().to_vec());
        let boundary = root.0.join("tools-boundary");
        export(repo, &boundary, &entries, &redactor).unwrap();
        verify(repo, &boundary, &commit).unwrap();
        for (i, bad, reason) in [
            (
                0,
                bounded.replacen("git: git version ", "git: git version x", 1),
                "budget",
            ),
            (1, tools.clone() + "git: git version \0\n", "NUL"),
            (2, tools.clone() + "rustc: duplicate\n", "duplicate"),
        ] {
            entries.insert("tools.txt".into(), bad.as_bytes().to_vec());
            let path = root.0.join(format!("tools-negative-{i}"));
            export(repo, &path, &entries, &redactor).unwrap();
            // Export escapes NULs. An independent producer can supply raw bytes
            // and a matching manifest, which the verifier must still reject.
            std::fs::write(path.join("tools.txt"), bad).unwrap();
            let manifest = entries
                .keys()
                .map(|name| {
                    let bytes = std::fs::read(path.join(name)).unwrap();
                    format!("{}  {name}\n", blob(repo, &bytes).unwrap())
                })
                .collect::<String>();
            std::fs::write(path.join("MANIFEST"), manifest).unwrap();
            let error = format!("{:#}", verify(repo, &path, &commit).unwrap_err());
            assert!(
                error.contains("tools.txt") && error.contains(reason),
                "{error}"
            );
        }
        entries.insert("tools.txt".into(), tools.into_bytes());
        entries.insert(
            "result.txt".into(),
            result
                .replace("lease: exclusive", "lease: inherited")
                .replace(
                    "lease-authority: kernel-exclusive",
                    "lease-authority: live-exclusive-holder",
                )
                .into_bytes(),
        );
        let inherited = root.0.join("inherited");
        export(repo, &inherited, &entries, &redactor).unwrap();
        verify(repo, &inherited, &commit).unwrap();
        std::fs::write(root.0.join("positive/steps/test.log"), b"changed").unwrap();
        assert!(
            verify(repo, &root.0.join("positive"), &commit)
                .unwrap_err()
                .to_string()
                .contains("digest")
        );
    }
    #[test]
    fn real_status_times_and_exit_precedence() {
        let steps = vec![
            Step {
                name: "build",
                verdict: Verdict::Fail,
                exit: Some(101),
                elapsed: Duration::from_millis(1234),
                detail: "compile failure".into(),
            },
            Step {
                name: "advisories",
                verdict: Verdict::EnvLimited,
                exit: None,
                elapsed: Duration::ZERO,
                detail: "missing".into(),
            },
        ];
        let text = table(&steps);
        assert!(text.contains("1.234") && text.contains("101"));
        assert_eq!(exit(&steps, true), 1);
        assert_eq!(exit(&steps[1..], false), 3);
    }
    #[test]
    fn marker_text_never_promotes_diagnostics_to_authority() {
        let text = marker_refusal(
            Path::new("/fixture/target/.gate-state/warm.use"),
            Some(b"pid=1\n\x1b[0m"),
            "marker lock busy",
        );
        for word in [
            "REFUSED:",
            "unvalidated",
            "read-only",
            "cargo clean",
            "qualifying: no",
            "warm.use",
        ] {
            assert!(text.contains(word));
        }
        assert!(!text.contains('\x1b'));
        assert!(
            kept(Path::new("/fixture/marker"), "lease lost").contains("target: kept (lease lost)")
        );
    }
    #[test]
    fn export_validates_all_entries_before_creating_a_bundle() {
        let scratch = Scratch::new();
        let entries = BTreeMap::from([
            ("environment.txt".into(), b"safe first entry\n".to_vec()),
            ("tools.txt".into(), vec![0xff]),
        ]);
        let redactor = Redactor {
            replacements: vec![],
            sensitive: vec![],
            unsupported_sensitive: false,
        };
        let destination = scratch.0.join("export");
        assert!(
            export(&scratch.0, &destination, &entries, &redactor)
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
        assert!(!destination.exists(), "late refusal left partial files");
    }

    #[test]
    fn sensitive_manifest_names_refuse_without_a_partial_bundle() {
        let scratch = Scratch::new();
        let entries = BTreeMap::from([("result.txt".into(), b"safe contents\n".to_vec())]);
        let redactor = Redactor {
            replacements: vec![("result.txt".into(), "<REDACTED>")],
            sensitive: vec!["result.txt".into()],
            unsupported_sensitive: false,
        };
        let destination = scratch.0.join("export");
        assert!(
            export(&scratch.0, &destination, &entries, &redactor)
                .unwrap_err()
                .to_string()
                .contains("manifest collision")
        );
        assert!(!destination.exists());
    }

    #[test]
    fn decoded_json_secret_and_replacement_collision_refuse() {
        let redactor = Redactor {
            replacements: vec![("known-secret".into(), "<REDACTED>")],
            sensitive: vec!["known-secret".into()],
            unsupported_sensitive: false,
        };
        // A valid alternate JSON encoding is checked after literal replacement.
        for input in [
            br#"{"nested":[{"text":"known\u002dsecret"}]}"#.as_slice(),
            br#"{"known\u002dsecret":true}"#.as_slice(),
        ] {
            assert!(
                redactor
                    .apply(input)
                    .unwrap_err()
                    .to_string()
                    .contains("decoded sensitive")
            );
        }
        let collision = Redactor {
            replacements: vec![("REDACTED".into(), "<REDACTED>")],
            sensitive: vec!["REDACTED".into()],
            unsupported_sensitive: false,
        };
        assert!(
            collision
                .apply(b"REDACTED")
                .unwrap_err()
                .to_string()
                .contains("sensitive value remains")
        );
    }

    #[test]
    fn evidence_redacts_paths_and_has_budgets() {
        let r = Redactor::new(
            Path::new("/repo"),
            Path::new("/home"),
            Path::new("/temp"),
            Some("machine"),
        );
        let text = r.apply(b"/repo/x /home/y /temp/z machine\x1b").unwrap();
        for s in ["/repo", "/home", "/temp", "machine", "\x1b"] {
            assert!(!text.contains(s));
        }
        assert!(!bundle_name("../result.txt"));
        assert!(!bundle_name("steps/../../secret.log"));
    }
}
