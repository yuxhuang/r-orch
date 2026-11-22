//! `cargo xtask fixture-gen`: the generated 1.0 corpus under `fixtures/storage/generated/`.
//!
//! The corpus is what the SQLite importer is tested on (spec R90, O10). A seeded script drives the
//! real 1.0 file backend in a private temporary directory (`script.rs`), so the layout, the index
//! order and the event files are what 1.0 writes, and every value in them is invented. Nothing is
//! read from a real project, a home directory or a daemon.
//!
//! * `cargo xtask fixture-gen` regenerates the corpus in place.
//! * `cargo xtask fixture-gen --check` regenerates it privately and compares exact file sets and
//!   bytes with the checked-in tree. It changes nothing and exits non-zero on any difference.

mod script;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;

use script::Profile;

/// The corpus directory, relative to the repository root.
const CORPUS_DIR: &str = "fixtures/storage/generated";
/// Written by the generator and required before a regeneration replaces a directory, so a wrong
/// `--dir` cannot wipe a directory that is not a generated corpus.
const MARKER: &str = ".orch-fixture-gen";
const MARKER_CONTENT: &[u8] = b"orch fixture-gen corpus v1\n";
/// A generated corpus is small; a bigger file or a deeper tree is not one.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_FILES: usize = 10_000;
const MAX_DEPTH: usize = 12;

struct Scenario {
    name: &'static str,
    seed: u64,
    profile: Profile,
    summary: &'static str,
}

/// Add a scenario here to add a directory to the corpus. Large-scale corpora for the importer's
/// bounds (R66) are not checked in; they are made on demand from a seed by the importer's tests.
const SCENARIOS: &[Scenario] = &[Scenario {
    name: "small",
    seed: 1,
    profile: Profile {
        agents: 4,
        goals: 2,
        tasks: 12,
        deleted_tasks: 2,
        messages: 6,
        context_entries: 3,
        max_events_per_run: 6,
    },
    summary: "four agents, two goals, one team, twelve tasks with their runs and events, two deleted tasks, six messages, three context entries, a template and one attachment",
}];

#[derive(Debug, Args)]
pub struct FixtureGenArgs {
    /// Compare the corpus with a fresh generation and change nothing; exit 1 on any difference.
    #[arg(long)]
    check: bool,
    /// The corpus directory (default: fixtures/storage/generated in this repository).
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
}

/// Relative path with `/` separators to the file's bytes.
type Tree = BTreeMap<String, Vec<u8>>;

pub fn run(repo_root: &Path, args: FixtureGenArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(|| repo_root.join(CORPUS_DIR));
    let expected = generate_tree()?;
    if args.check {
        let found = read_tree(&dir)
            .with_context(|| format!("read the checked-in corpus at {}", dir.display()))?;
        let drift = compare(&expected, &found);
        if drift.is_empty() {
            println!(
                "fixture-gen --check: {} files match a fresh generation byte for byte",
                expected.len()
            );
            return Ok(());
        }
        for line in &drift {
            eprintln!("{line}");
        }
        bail!(
            "{} differs from a fresh generation in {} place(s); run `cargo xtask fixture-gen` and commit the result",
            dir.display(),
            drift.len()
        );
    }
    publish(&dir, &expected)?;
    println!(
        "fixture-gen: wrote {} files under {}",
        expected.len(),
        dir.display()
    );
    Ok(())
}

/// Every scenario, generated privately, plus the README: the exact file set the corpus must hold.
fn generate_tree() -> Result<Tree> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("start the generator's runtime")?;
    let mut tree = Tree::new();
    for scenario in SCENARIOS {
        let private = tempfile::Builder::new()
            .prefix("fixture-gen-")
            .tempdir()
            .context("create a private directory")?;
        refuse_project_ancestor(private.path())?;
        runtime
            .block_on(script::generate(
                private.path(),
                scenario.seed,
                &scenario.profile,
            ))
            .with_context(|| format!("run scenario {}", scenario.name))?;
        let files = read_tree(private.path())?;
        let root = private.path().to_string_lossy().into_owned();
        for (path, bytes) in files {
            if path
                .rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"))
            {
                bail!("scenario {} left write debris at {path}", scenario.name);
            }
            if contains(&bytes, root.as_bytes()) {
                bail!(
                    "{path} in scenario {} names the private directory",
                    scenario.name
                );
            }
            tree.insert(format!("{}/{path}", scenario.name), bytes);
        }
    }
    tree.insert("README.md".to_owned(), readme().into_bytes());
    tree.insert(MARKER.to_owned(), MARKER_CONTENT.to_vec());
    Ok(tree)
}

/// A test or a stray tempdir under a real project would make a path-walking helper find that
/// project. The stores take their root explicitly, so this guards the premise, not a code path.
fn refuse_project_ancestor(dir: &Path) -> Result<()> {
    for ancestor in dir.ancestors().skip(1) {
        if ancestor.join(".orchestry").exists() {
            bail!(
                "the private directory {} sits inside a project ({} has .orchestry); point TMPDIR elsewhere",
                dir.display(),
                ancestor.display()
            );
        }
    }
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn readme() -> String {
    let mut text = String::from(
        "# Generated 1.0 import fixtures\n\n\
         Do not edit by hand. These directories are made by `cargo xtask fixture-gen` (spec R90, O10): a seeded\n\
         script drives the real 1.0 file backend in a private temporary directory, so the layout, the index\n\
         order and the event files are what 1.0 writes. Every value is invented, and the clock and the ids\n\
         come from the seed. Nothing was read from a real project.\n\n\
         `cargo xtask fixture-gen --check` regenerates the corpus privately and compares exact file sets\n\
         and bytes with this directory without changing it. `cargo xtask fixture-gen` rewrites it. A nonempty\n\
         destination is replaced only when `.orch-fixture-gen` contains exactly `orch fixture-gen corpus v1`\n\
         followed by a newline. README.md alone does not grant replacement permission.\n\n\
         Each scenario holds synthetic records written through the 1.0 file stores, with nothing running.\n\
         This is a small serialization corpus, not a complete live-project or importer acceptance fixture.\n\
         Runs use invented relative `workspace_path` values (`workspaces/<task-id>`); no workspace directories\n\
         or repositories are created, and task proof is absent. It does not exercise live process identity,\n\
         workspace ownership, hostile documents, import bounds, dry run or resume. Those checks remain in T21.\n\n\
         | Directory | Seed | Contents |\n|---|---|---|\n",
    );
    for scenario in SCENARIOS {
        text.push_str(&format!(
            "| `{}/` | {} | {} |\n",
            scenario.name, scenario.seed, scenario.summary
        ));
    }
    text.push_str(
        "\nThe hand-written conformance corpus is `fixtures/storage-v1/`. Large corpora for the importer's\n\
         bounds remain work for T21 and are not checked in.\n",
    );
    text
}

/// Read every file under `dir` into a tree. A link, a special file, a file over the size bound, a
/// tree over the count or depth bound is an error, never followed or skipped: the corpus is plain
/// files, and a comparison that skipped one would call a changed tree equal.
fn read_tree(dir: &Path) -> Result<Tree> {
    let mut tree = Tree::new();
    let mut pending = vec![(dir.to_path_buf(), 0_usize)];
    while let Some((current, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            bail!("{} is deeper than {MAX_DEPTH} levels", current.display());
        }
        let mut entries = fs::read_dir(&current)
            .with_context(|| format!("list {}", current.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("list {}", current.display()))?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let meta =
                fs::symlink_metadata(&path).with_context(|| format!("stat {}", path.display()))?;
            if meta.is_dir() {
                pending.push((path, depth + 1));
            } else if meta.is_file() {
                if meta.len() > MAX_FILE_BYTES {
                    bail!("{} is larger than {MAX_FILE_BYTES} bytes", path.display());
                }
                if tree.len() >= MAX_FILES {
                    bail!("more than {MAX_FILES} files under {}", dir.display());
                }
                let relative = relative_name(dir, &path)?;
                let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
                tree.insert(relative, bytes);
            } else {
                bail!("{} is not a regular file or directory", path.display());
            }
        }
    }
    Ok(tree)
}

fn relative_name(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .context("a listed path left its root")?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .with_context(|| format!("{} is not UTF-8", path.display()))?
                    .to_owned(),
            ),
            _ => bail!("unexpected path component in {}", path.display()),
        }
    }
    Ok(parts.join("/"))
}

/// One line per missing file, extra file and changed file, in path order.
fn compare(expected: &Tree, found: &Tree) -> Vec<String> {
    let mut drift = Vec::new();
    for (path, want) in expected {
        match found.get(path) {
            None => drift.push(format!("missing: {path}")),
            Some(have) if have != want => {
                let at = want
                    .iter()
                    .zip(have)
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| want.len().min(have.len()));
                drift.push(format!(
                    "changed: {path} (expected {} bytes, found {}; first difference at byte {at})",
                    want.len(),
                    have.len()
                ));
            }
            Some(_) => {}
        }
    }
    for path in found.keys().filter(|path| !expected.contains_key(*path)) {
        drift.push(format!("extra: {path}"));
    }
    drift
}

/// Replace the corpus directory with `tree`. An existing nonempty directory requires the exact
/// generator/version marker. An absent or empty directory needs no ownership marker.
fn publish(dir: &Path, tree: &Tree) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {
            // Parse the whole tree first: a link or a special file inside stops the replacement
            // before anything is removed. Only the exact generator/version marker grants ownership.
            let found = read_tree(dir)
                .with_context(|| format!("check {} before replacing it", dir.display()))?;
            if fs::read_dir(dir)?.next().is_some()
                && found.get(MARKER).map(Vec::as_slice) != Some(MARKER_CONTENT)
            {
                bail!(
                    "{} exists without the exact {MARKER} version marker; it is not a generated corpus, so it is not replaced",
                    dir.display()
                );
            }
            fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))?;
        }
        Ok(_) => bail!("{} is not a directory", dir.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("stat {}", dir.display())),
    }
    for (relative, bytes) in tree {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(entries: &[(&str, &str)]) -> Tree {
        entries
            .iter()
            .map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn compare_names_missing_extra_and_changed_files() {
        let expected = tree(&[("a", "one"), ("b", "two"), ("c", "same")]);
        let found = tree(&[("a", "one"), ("b", "twO"), ("c", "same"), ("d", "new")]);
        assert_eq!(
            compare(&expected, &found),
            vec![
                "changed: b (expected 3 bytes, found 3; first difference at byte 2)".to_owned(),
                "extra: d".to_owned(),
            ]
        );
        assert_eq!(
            compare(&expected, &tree(&[("a", "one"), ("c", "same")])),
            vec!["missing: b".to_owned()]
        );
        assert!(compare(&expected, &expected).is_empty());
    }

    #[test]
    fn a_length_difference_alone_is_a_change() {
        let drift = compare(&tree(&[("a", "abc")]), &tree(&[("a", "abcd")]));
        assert_eq!(
            drift,
            vec!["changed: a (expected 3 bytes, found 4; first difference at byte 3)".to_owned()]
        );
    }

    #[test]
    fn read_tree_refuses_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real"), "x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        let err = read_tree(dir.path()).unwrap_err().to_string();
        assert!(err.contains("not a regular file"), "{err}");
    }

    #[test]
    fn publish_refuses_a_directory_without_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("precious"), "keep").unwrap();
        let err = publish(dir.path(), &tree(&[("README.md", "x")]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a generated corpus"), "{err}");
        assert!(dir.path().join("precious").is_file());
    }

    #[test]
    fn publish_preserves_unowned_nonempty_destinations() {
        for marker in [
            None,
            Some(b"".as_slice()),
            Some(b"orch fixture-gen corpus v2\n"),
            Some(b"another generator v1\n"),
            Some(b"orch fixture-gen corpus v1"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("README.md"), "unrelated project").unwrap();
            fs::create_dir(dir.path().join("nested")).unwrap();
            fs::write(dir.path().join("nested/precious"), b"keep\0bytes").unwrap();
            if let Some(bytes) = marker {
                fs::write(dir.path().join(MARKER), bytes).unwrap();
            }
            let before = read_tree(dir.path()).unwrap();
            let err = publish(dir.path(), &tree(&[("new", "replacement")])).unwrap_err();
            assert!(err.to_string().contains("not a generated corpus"), "{err}");
            assert_eq!(read_tree(dir.path()).unwrap(), before);
        }
    }

    #[test]
    fn publish_accepts_empty_absent_and_owned_destinations() {
        let private = tempfile::tempdir().unwrap();
        let expected = tree(&[
            (MARKER, std::str::from_utf8(MARKER_CONTENT).unwrap()),
            ("new", "data"),
        ]);
        for name in ["absent", "empty", "owned"] {
            let dir = private.path().join(name);
            if name != "absent" {
                fs::create_dir(&dir).unwrap();
            }
            if name == "owned" {
                fs::write(dir.join(MARKER), MARKER_CONTENT).unwrap();
                fs::write(dir.join("stale"), "remove").unwrap();
            }
            publish(&dir, &expected).unwrap();
            assert_eq!(read_tree(&dir).unwrap(), expected);
        }
    }

    #[test]
    fn publish_refuses_a_linked_marker_and_preserves_its_target() {
        let private = tempfile::tempdir().unwrap();
        let target = private.path().join("marker");
        fs::write(&target, MARKER_CONTENT).unwrap();
        let dir = private.path().join("output");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("precious"), "keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join(MARKER)).unwrap();
        assert!(publish(&dir, &tree(&[("new", "replacement")])).is_err());
        assert_eq!(fs::read(dir.join("precious")).unwrap(), b"keep");
        assert!(fs::symlink_metadata(dir.join(MARKER)).unwrap().is_symlink());
        assert_eq!(fs::read(target).unwrap(), MARKER_CONTENT);
    }

    #[test]
    fn two_generations_are_byte_identical_and_a_different_seed_is_not() {
        let first = generate_tree().unwrap();
        assert_eq!(first, generate_tree().unwrap());
        let private = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(script::generate(
                private.path(),
                SCENARIOS[0].seed + 1,
                &SCENARIOS[0].profile,
            ))
            .unwrap();
        let other = read_tree(private.path()).unwrap();
        let ours: Tree = first
            .iter()
            .filter_map(|(path, bytes)| {
                Some((path.strip_prefix("small/")?.to_owned(), bytes.clone()))
            })
            .collect();
        assert_ne!(ours, other);
    }
}
