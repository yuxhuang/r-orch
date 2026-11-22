//! Judge identity includes untracked, nonignored tooling inputs. Source links are
//! refused; git's symlink blob hashes the link text, not the bytes rustc reads.
use super::{files, report};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

pub(super) const INPUTS: &[&str] = &[
    "xtask",
    // fixture-gen compiles this normal workspace dependency closure. Keep the
    // bootstrap list in gate.sh in sync, including embedded non-Rust assets.
    "crates/orch-core",
    "crates/orch-domain",
    "crates/orch-infra",
    // Cargo resolves the workspace together; other member manifests can affect
    // resolution even though their sources are not compiled into this judge.
    // Use literal paths: ls-tree does not expand the ls-files wildcard syntax.
    // Update both lists when workspace membership changes.
    "crates/orch-cli/Cargo.toml",
    "crates/orch-client/Cargo.toml",
    "crates/orch-daemon/Cargo.toml",
    "crates/orch-engine/Cargo.toml",
    "crates/orch-peer/Cargo.toml",
    "crates/orch-proto/Cargo.toml",
    "crates/orch-tui/Cargo.toml",
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

/// A direct invocation may run development checks but cannot attest that its
/// executable came from these source bytes. Only the content-stamped bootstrap
/// path supplies that producer provenance; this is not malicious-user auth.
pub(super) fn stamped(repo: &Path, id: &str, executable: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let boot = repo.join("target/gate-xtask");
    files::real_directories(&boot.join("debug"))?;
    ensure!(
        executable == boot.join("debug/xtask"),
        "judge executable is not bootstrap output"
    );
    for path in [boot.join("judge-id"), executable.to_path_buf()] {
        let meta = std::fs::symlink_metadata(&path)?;
        ensure!(
            meta.is_file() && meta.uid() == nix::unistd::geteuid().as_raw() && meta.nlink() == 1,
            "judge stamp/executable structure refused"
        );
    }
    ensure!(
        files::text(&boot.join("judge-id"), 128)?.trim() == id,
        "completed judge stamp mismatch"
    );
    Ok(())
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    use crate::gate::steps::tests::Scratch;
    #[test]
    fn dependency_identity_agrees_at_commit_and_detects_each_input() {
        let scratch = Scratch::new();
        let repo = &scratch.0;
        let write = |name: &str, bytes: &[u8]| {
            let path = repo.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "Fixture")
                .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
                .env("GIT_COMMITTER_NAME", "Fixture")
                .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        };
        git(&["init", "--quiet"]);
        for input in ["xtask/gate.sh", "Cargo.lock"] {
            write(input, b"synthetic baseline");
        }
        git(&["add", "."]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ]);
        let mut previous = current(repo).unwrap();
        assert_eq!(previous, at_commit(repo, "HEAD").unwrap());
        for input in [
            "crates/orch-core/Cargo.toml",
            "crates/orch-core/src/lib.rs",
            "crates/orch-domain/src/lib.rs",
            "crates/orch-domain/src/shop/roles/example.md",
            "crates/orch-infra/src/lib.rs",
            "crates/orch-infra/src/template/system.liquid",
            "crates/orch-infra/build.rs",
            "crates/orch-engine/Cargo.toml",
        ] {
            write(input, b"synthetic compile input");
            let live = current(repo).unwrap();
            assert_ne!(live, previous, "omitted untracked {input}");
            assert_eq!(previous, at_commit(repo, "HEAD").unwrap());
            git(&["add", "."]);
            git(&[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "input",
            ]);
            assert_eq!(live, at_commit(repo, "HEAD").unwrap());
            write(input, b"changed compile input");
            assert_ne!(live, current(repo).unwrap(), "omitted changed {input}");
            git(&["checkout", "--", input]);
            previous = live;
        }
        write("crates/orch-engine/src/lib.rs", b"outside closure");
        assert_eq!(previous, current(repo).unwrap());
        let input = repo.join("crates/orch-infra/src/lib.rs");
        std::fs::remove_file(&input).unwrap();
        assert!(current(repo).is_err());
        std::os::unix::fs::symlink("system.liquid", &input).unwrap();
        assert!(current(repo).is_err());
        git(&["add", "."]);
        git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "link",
        ]);
        assert!(at_commit(repo, "HEAD").is_err());
    }

    #[test]
    fn arbitrary_hardlinked_stamp_or_executable_remains_nonqualifying() {
        for component in ["judge-id", "debug/xtask"] {
            let scratch = Scratch::new();
            let boot = scratch.0.join("target/gate-xtask");
            std::fs::create_dir_all(boot.join("debug")).unwrap();
            let executable = boot.join("debug/xtask");
            files::write(&executable, b"fixture executable bytes").unwrap();
            let id = "a".repeat(40);
            files::write(&boot.join("judge-id"), format!("{id}\n").as_bytes()).unwrap();
            stamped(&scratch.0, &id, &executable).unwrap();
            let alias = scratch.0.join("outside-alias");
            std::fs::hard_link(boot.join(component), &alias).unwrap();
            let before = std::fs::read(&alias).unwrap();
            let error = stamped(&scratch.0, &id, &executable).unwrap_err();
            assert!(error.to_string().contains("structure refused"));
            assert_eq!(std::fs::read(&alias).unwrap(), before);
        }
    }

    #[test]
    fn absent_or_special_executable_cannot_qualify_with_a_matching_stamp() {
        let scratch = Scratch::new();
        let boot = scratch.0.join("target/gate-xtask");
        std::fs::create_dir_all(boot.join("debug")).unwrap();
        let executable = boot.join("debug/xtask");
        let id = "a".repeat(40);
        files::write(&boot.join("judge-id"), format!("{id}\n").as_bytes()).unwrap();
        assert!(stamped(&scratch.0, &id, &executable).is_err());
        let _socket = std::os::unix::net::UnixListener::bind(&executable).unwrap();
        assert!(
            stamped(&scratch.0, &id, &executable)
                .unwrap_err()
                .to_string()
                .contains("structure refused")
        );
    }

    #[test]
    fn stamp_requires_matching_identity_and_executable_and_refuses_links() {
        let scratch = Scratch::new();
        let boot = scratch.0.join("target/gate-xtask");
        std::fs::create_dir_all(boot.join("debug")).unwrap();
        let executable = boot.join("debug/xtask");
        files::write(&executable, b"fixture executable bytes").unwrap();
        let id = "a".repeat(40);
        assert!(stamped(&scratch.0, &id, &executable).is_err());
        files::write(&boot.join("judge-id"), format!("{id}\n").as_bytes()).unwrap();
        stamped(&scratch.0, &id, &executable).unwrap();
        assert!(stamped(&scratch.0, &"b".repeat(40), &executable).is_err());
        files::write(&scratch.0.join("old-xtask"), b"stale fixture executable").unwrap();
        assert!(stamped(&scratch.0, &id, &scratch.0.join("old-xtask")).is_err());
        std::fs::rename(boot.join("judge-id"), boot.join("outside")).unwrap();
        std::os::unix::fs::symlink("outside", boot.join("judge-id")).unwrap();
        assert!(stamped(&scratch.0, &id, &executable).is_err());
    }
}
