//! Fail-closed change selection shared by the local gate and CI.

mod classify;
mod git;
mod graph;
mod process;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};

#[derive(Debug, Args)]
pub struct SelectArgs {
    #[arg(long, default_value = "origin/main")]
    pub base: String,
    #[arg(long, value_enum, default_value = "text")]
    pub format: Format,
    #[arg(long, requires = "metadata_from")]
    pub changed_from: Option<PathBuf>,
    #[arg(long, requires = "changed_from")]
    pub metadata_from: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Text,
    Json,
    Github,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Git name-status token, including a rename/copy score; unknown tokens fail closed.
    pub status: String,
    /// Destination/current path bytes; for a deletion, the deleted path.
    pub path: Vec<u8>,
    /// Rename/copy source bytes. Paths are never decoded lossily.
    pub old_path: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Doc,
    Crate,
    Global,
    Coupled,
    Unknown,
    Error,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Doc => "doc",
            Self::Crate => "crate",
            Self::Global => "global",
            Self::Coupled => "coupled",
            Self::Unknown => "unknown",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    DocsOnly,
    Packages,
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub path: String,
    pub class: Class,
    pub selects: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub mode: Mode,
    pub packages: Vec<String>,
    pub base: Option<String>,
    pub reasons: Vec<Reason>,
}

const MAX_INPUT: usize = 16 * 1024 * 1024;
const MAX_CHANGES: usize = 20_000;
const MAX_PATH: usize = 16 * 1024;

pub fn select(changes: &[Change], metadata_json: &str) -> Selection {
    if changes.len() > MAX_CHANGES {
        return error("", "change count exceeds bound");
    }
    let total_bytes = changes.iter().try_fold(0usize, |total, change| {
        total
            .checked_add(change.path.len())?
            .checked_add(change.old_path.as_ref().map_or(0, Vec::len))?
            .checked_add(change.status.len())
    });
    if total_bytes.is_none_or(|total| total > MAX_INPUT) {
        return error("", "changed paths exceed input bound");
    }
    let mut selection = Selection {
        mode: Mode::DocsOnly,
        packages: Vec::new(),
        base: None,
        reasons: Vec::new(),
    };
    let mut paths = BTreeSet::new();
    for change in changes {
        match classify::paths(change) {
            Ok(names) => {
                for bytes in names {
                    match classify::path(bytes) {
                        Ok(path) => {
                            paths.insert(path);
                        }
                        Err(err) => selection.reasons.push(classify::reason(
                            &classify::diagnostic_path(bytes),
                            Class::Error,
                            "all",
                            &err.to_string(),
                        )),
                    }
                }
            }
            Err(err) => {
                for bytes in
                    std::iter::once(change.path.as_slice()).chain(change.old_path.as_deref())
                {
                    selection.reasons.push(classify::reason(
                        &classify::diagnostic_path(bytes),
                        Class::Error,
                        "all",
                        &err.to_string(),
                    ));
                }
            }
        }
    }
    let graph = graph::Graph::parse(metadata_json);
    let mut packages = BTreeSet::new();
    let mut cli_paths = BTreeSet::new();
    for path in paths {
        match &graph {
            Ok(graph) => {
                let (reason, selected) = classify::classify(path, graph);
                if selected.contains("orch-cli") {
                    cli_paths.insert(path);
                }
                selection.reasons.push(reason);
                packages.extend(selected);
            }
            Err(err) => selection.reasons.push(classify::reason(
                path,
                Class::Error,
                "all",
                &format!("metadata error: {err:#}"),
            )),
        }
    }
    if changes.is_empty() {
        selection.reasons.push(classify::reason(
            "",
            Class::Error,
            "all",
            "empty change set; conservatively selecting the entire workspace",
        ));
    }
    if let Err(err) = &graph {
        selection.reasons.push(classify::reason(
            "",
            Class::Error,
            "all",
            &format!("metadata error: {err:#}"),
        ));
    }
    // D39 is evaluated once after the complete normal closure. An extra edge
    // adds only infra, never its reverse dependents; normal infra selection
    // (including a separate changed path) needs no coupling reason.
    if !selection
        .reasons
        .iter()
        .any(|r| matches!(r.class, Class::Global | Class::Unknown | Class::Error))
        && packages.contains("orch-cli")
        && !packages.contains("orch-infra")
    {
        let infra_present = graph
            .as_ref()
            .is_ok_and(|graph| graph.contains("orch-infra"));
        for path in cli_paths {
            selection.reasons.push(if infra_present {
                classify::reason(
                    path,
                    Class::Coupled,
                    "orch-infra",
                    "concurrent_index starts the selected orch-cli binary by path",
                )
            } else {
                classify::reason(
                    path,
                    Class::Error,
                    "all",
                    "package coupling names a missing orch-infra member",
                )
            });
        }
        if infra_present {
            packages.insert("orch-infra".into());
        }
    }
    selection.reasons.sort_by(|a, b| {
        (&a.path, a.class.as_str(), &a.selects, &a.why).cmp(&(
            &b.path,
            b.class.as_str(),
            &b.selects,
            &b.why,
        ))
    });
    selection.reasons.dedup();
    if selection
        .reasons
        .iter()
        .any(|r| matches!(r.class, Class::Global | Class::Unknown | Class::Error))
    {
        selection.mode = Mode::All;
    } else if !packages.is_empty() {
        selection.mode = Mode::Packages;
        selection.packages = packages.into_iter().collect();
    }
    selection
}

pub fn select_from_git(repo: &Path, base: &str) -> Selection {
    let mut selection = match git::inputs(repo, base) {
        Ok((changes, metadata)) => select(&changes, &metadata),
        Err(err) => error("", &format!("git/metadata input error: {err:#}")),
    };
    selection.base = Some(base.chars().take(MAX_PATH).collect());
    selection
}

fn error(path: &str, why: &str) -> Selection {
    Selection {
        mode: Mode::All,
        packages: Vec::new(),
        base: None,
        reasons: vec![classify::reason(path, Class::Error, "all", why)],
    }
}

fn mode(mode: Mode) -> &'static str {
    match mode {
        Mode::DocsOnly => "docs-only",
        Mode::Packages => "packages",
        Mode::All => "all",
    }
}

pub fn run_cli(repo: &Path, args: SelectArgs) -> Result<()> {
    let mut selection =
        if args.base.is_empty() || args.base.len() > MAX_PATH || args.base.contains('\0') {
            error("", "invalid base revision")
        } else {
            match (&args.changed_from, &args.metadata_from) {
                (Some(changed), Some(metadata)) => match git::fixture_inputs(changed, metadata) {
                    Ok((changes, metadata)) => select(&changes, &metadata),
                    Err(err) => error("", &format!("fixture input error: {err:#}")),
                },
                (None, None) => select_from_git(repo, &args.base),
                _ => bail!("--changed-from and --metadata-from must be supplied together"),
            }
        };
    selection.base = Some(args.base.chars().take(MAX_PATH).collect());
    let packages = serde_json::to_string(&selection.packages)?;
    match args.format {
        Format::Json => println!(
            "{}",
            serde_json::json!({
                "schema": 1, "mode": mode(selection.mode), "packages": selection.packages,
                "base": selection.base,
                "reasons": selection.reasons.iter().map(|r| serde_json::json!({
                    "path":r.path, "class":r.class.as_str(), "selects":r.selects, "why":r.why
                })).collect::<Vec<_>>()
            })
        ),
        Format::Github => {
            println!("mode={}\npackages={packages}", mode(selection.mode));
            for reason in &selection.reasons {
                eprintln!("{}", display_reason(reason));
            }
        }
        Format::Text => {
            println!("mode: {}\npackages: {packages}", mode(selection.mode));
            for reason in &selection.reasons {
                println!("{}", display_reason(reason));
            }
        }
    }
    Ok(())
}

fn display_reason(reason: &Reason) -> String {
    // JSON quoting keeps newlines, terminal controls and GitHub command prefixes
    // in filenames inert. Only the two controlled output keys go to stdout in CI.
    format!(
        "{} [{}] selects {}: {}",
        serde_json::json!(reason.path),
        reason.class.as_str(),
        reason.selects,
        reason.why
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn metadata() -> String {
        let names = [
            "orch-domain",
            "orch-infra",
            "orch-cli",
            "infra-user",
            "xtask",
        ];
        let packages: Vec<_> = names
            .iter()
            .map(|name| {
                let dependencies = match *name {
                    "orch-infra" => json!([{"name":"orch-domain", "kind": "build"}]),
                    "orch-cli" => json!([{"name":"orch-infra", "kind": null}]),
                    "infra-user" => json!([{"name":"orch-infra", "kind":"dev"}]),
                    _ => json!([]),
                };
                json!({"name":name, "id":name,
                    "manifest_path":format!("/repo/crates/{name}/Cargo.toml"),
                    "dependencies":dependencies})
            })
            .collect();
        json!({"workspace_root":"/repo", "workspace_members":names,
            "packages":packages, "version":1})
        .to_string()
    }

    fn change(status: &str, path: &str) -> Change {
        Change {
            status: status.into(),
            path: path.as_bytes().to_vec(),
            old_path: None,
        }
    }

    fn selected(paths: &[&str]) -> Selection {
        select(
            &paths.iter().map(|p| change("M", p)).collect::<Vec<_>>(),
            &metadata(),
        )
    }

    #[test]
    fn dependency_closure_includes_normal_build_and_dev_edges() {
        let selection = selected(&["crates/orch-domain/src/lib.rs"]);
        assert_eq!(selection.mode, Mode::Packages);
        assert_eq!(
            selection.packages,
            ["infra-user", "orch-cli", "orch-domain", "orch-infra"]
        );
        assert_eq!(selection.reasons[0].class, Class::Crate);
        assert_eq!(
            selected(&["crates/infra-user/src/lib.rs"]).packages,
            ["infra-user"]
        );
    }

    #[test]
    fn coupling_does_not_expand_the_coupled_packages_dependents() {
        let cli = selected(&["crates/orch-cli/src/main.rs"]);
        assert_eq!(cli.packages, ["orch-cli", "orch-infra"]);
        assert!(
            cli.reasons
                .iter()
                .any(|r| r.class == Class::Coupled && r.selects == "orch-infra")
        );
        let infra = selected(&["crates/orch-infra/src/lib.rs"]);
        assert_eq!(infra.packages, ["infra-user", "orch-cli", "orch-infra"]);
        assert!(infra.reasons.iter().all(|r| r.class != Class::Coupled));
    }

    #[test]
    fn documentation_exception_precedes_docs_and_crate_markdown_is_code() {
        assert_eq!(
            selected(&["docs/RUNBOOK.md", "README.md", "LICENSE", "NOTICE"]).mode,
            Mode::DocsOnly
        );
        assert_eq!(selected(&["docs/ARCHITECTURE.md"]).packages, ["xtask"]);
        let mixed = selected(&["docs/RUNBOOK.md", "crates/infra-user/README.md"]);
        assert_eq!(mixed.packages, ["infra-user"]);
        assert_eq!(
            mixed
                .reasons
                .iter()
                .find(|r| r.path.starts_with("crates/"))
                .unwrap()
                .class,
            Class::Crate
        );
    }

    #[test]
    fn every_shared_input_selects_the_workspace() {
        for path in [
            "Cargo.lock",
            "Cargo.toml",
            "crates/orch-cli/Cargo.toml",
            "rust-toolchain.toml",
            "clippy.toml",
            "deny.toml",
            ".gitignore",
            ".gitattributes",
            ".cargo/config.toml",
            "xtask/src/main.rs",
            ".github/workflows/ci.yml",
            ".config/nextest.toml",
            "fixtures/input",
            "skills/example",
            "examples/input",
            "crates/orch-cli/tests/common/mod.rs",
            "crates/orch-core/src/test_support/mod.rs",
            "crates/orch-cli/tests/push_support/mod.rs",
            "crates/orch-cli/tests/support.rs",
        ] {
            let selection = selected(&[path]);
            assert_eq!(selection.mode, Mode::All, "{path}");
            assert!(selection.packages.is_empty());
            assert_eq!(selection.reasons[0].class, Class::Global, "{path}");
        }
        for path in ["Makefile", "newdir/file", "crates/missing/src/lib.rs"] {
            let selection = selected(&[path]);
            assert_eq!(selection.mode, Mode::All);
            assert_eq!(selection.reasons[0].class, Class::Unknown);
        }
    }

    #[test]
    fn renames_copies_and_deletions_retain_both_owners() {
        for status in ["R100", "C075"] {
            let mut moved = change(status, "crates/infra-user/new.rs");
            moved.old_path = Some(b"crates/orch-cli/old.rs".to_vec());
            let selection = select(&[moved], &metadata());
            assert_eq!(selection.packages, ["infra-user", "orch-cli", "orch-infra"]);
            assert!(selection.reasons.iter().any(|r| r.path.ends_with("old.rs")));
            assert!(selection.reasons.iter().any(|r| r.path.ends_with("new.rs")));
        }
        assert_eq!(
            select(&[change("D", "crates/infra-user/old.rs")], &metadata()).packages,
            ["infra-user"]
        );
        let mut from_doc = change("R100", "crates/infra-user/new.rs");
        from_doc.old_path = Some(b"docs/old.md".to_vec());
        assert_eq!(select(&[from_doc], &metadata()).packages, ["infra-user"]);
    }

    #[test]
    fn malformed_status_paths_and_metadata_fail_closed_with_error_reasons() {
        let mut invalid = vec![
            change("", "README.md"),
            change("U", "README.md"),
            change("T", "README.md"),
            change("R101", "README.md"),
            change("R100", "README.md"),
            change("M100", "README.md"),
            change("M", "../README.md"),
            change("M", "/README.md"),
            change("M", "docs//x"),
        ];
        invalid.push(Change {
            status: "M".into(),
            path: vec![0xff],
            old_path: None,
        });
        let mut extra = change("M", "README.md");
        extra.old_path = Some(b"other.md".to_vec());
        invalid.push(extra);
        for record in invalid {
            let selection = select(&[record], &metadata());
            assert_eq!(selection.mode, Mode::All);
            assert!(selection.reasons.iter().any(|r| r.class == Class::Error));
        }
        for metadata in ["", "{", "{}", r#"{"workspace_members":[],"packages":[]}"#] {
            let selection = select(&[change("M", "README.md")], metadata);
            assert_eq!(selection.mode, Mode::All);
            assert!(selection.reasons.iter().any(|r| r.class == Class::Error));
        }
        assert_eq!(select(&[], &metadata()).mode, Mode::All);
    }

    #[test]
    fn reasons_and_packages_are_stable_sorted_unique_and_lossless() {
        let paths = [
            "crates/infra-user/-x ;$(touch NEVER)\n.rs",
            "docs/space name.md",
            "crates/orch-cli/main.rs",
            "docs/space name.md",
        ];
        let first = selected(&paths);
        let second = selected(&paths.into_iter().rev().collect::<Vec<_>>());
        assert_eq!(first, second);
        assert!(first.reasons.iter().any(|r| r.path == paths[0]));
        assert_eq!(
            first.reasons.iter().filter(|r| r.path == paths[1]).count(),
            1
        );
    }

    #[test]
    fn input_bounds_and_deep_json_fail_closed() {
        let oversized = change("M", &"x".repeat(MAX_PATH + 1));
        assert_eq!(select(&[oversized], &metadata()).mode, Mode::All);
        let deep = format!("{}0{}", "[".repeat(140), "]".repeat(140));
        assert_eq!(select(&[change("M", "README.md")], &deep).mode, Mode::All);
        let many = vec![change("M", "README.md"); MAX_CHANGES + 1];
        assert_eq!(select(&many, &metadata()).mode, Mode::All);
    }

    #[test]
    fn terminal_and_github_rendering_cannot_promote_a_path_to_a_command() {
        let reason = classify::reason(
            "\n::error::injected\u{1b}[31m",
            Class::Unknown,
            "all",
            "unknown path",
        );
        let rendered = display_reason(&reason);
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\u{1b}'));
        assert!(rendered.starts_with('"'));
    }
}
