//! Judge identity includes untracked, nonignored tooling inputs. Source links are
//! refused; git's symlink blob hashes the link text, not the bytes rustc reads.
use super::{files, report};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

pub(super) const INPUTS: &[&str] = &[
    "xtask",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
];
fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    ensure!(
        output.status.success() && output.stdout.len() <= 4 * 1024 * 1024,
        "judge git input failed or exceeds budget"
    );
    Ok(output.stdout)
}
fn path(bytes: &[u8]) -> Result<&str> {
    let s = std::str::from_utf8(bytes)?;
    ensure!(
        !s.is_empty()
            && !s.chars().any(char::is_control)
            && !s.starts_with('/')
            && !s.split('/').any(|s| [".", "..", ""].contains(&s)),
        "judge source path unsupported"
    );
    Ok(s)
}
fn finish(repo: &Path, entries: BTreeMap<String, String>) -> Result<String> {
    ensure!(
        entries.contains_key("xtask/gate.sh")
            && entries.contains_key("Cargo.lock")
            && entries.len() < 4096,
        "incomplete judge source set"
    );
    let list: String = entries
        .into_iter()
        .map(|(path, id)| format!("{id} {path}\n"))
        .collect();
    report::blob(repo, list.as_bytes())
}
pub(super) fn current(repo: &Path) -> Result<String> {
    let mut args = vec![
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    args.extend(INPUTS);
    let paths = git(repo, &args)?;
    let mut entries = BTreeMap::new();
    for p in paths.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let p = path(p)?;
        files::real_directories(repo.join(p).parent().context("source parent")?)?;
        let bytes = files::read(&repo.join(p), 16 * 1024 * 1024)?;
        ensure!(
            entries
                .insert(p.to_owned(), report::blob(repo, &bytes)?)
                .is_none(),
            "duplicate judge path"
        );
    }
    finish(repo, entries)
}
pub(super) fn at_commit(repo: &Path, commit: &str) -> Result<String> {
    let mut args = vec!["ls-tree", "-rz", commit, "--"];
    args.extend(INPUTS);
    let tree = git(repo, &args)?;
    let mut entries = BTreeMap::new();
    for record in tree.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let at = record
            .iter()
            .position(|b| *b == b'\t')
            .context("judge tree entry")?;
        let fields: Vec<_> = std::str::from_utf8(&record[..at])?
            .split_whitespace()
            .collect();
        ensure!(
            fields.len() == 3 && ["100644", "100755"].contains(&fields[0]) && fields[1] == "blob",
            "judge source link or special unsupported"
        );
        entries.insert(path(&record[at + 1..])?.to_owned(), fields[2].to_owned());
    }
    finish(repo, entries)
}
