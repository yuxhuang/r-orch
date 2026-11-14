//! Conservative PR1 selector seam. G replaces this stub without registry edits.

use std::path::{Path, PathBuf};

use anyhow::Result;
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

// These public types are the compiled handoff to G, even before the stub uses them.
#[allow(
    dead_code,
    reason = "F0 declares the frozen selector seam for the subsequent G implementation"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Git name-status token, including a rename/copy score; unknown tokens fail closed.
    pub status: String,
    /// Destination/current path bytes; for a deletion, the deleted path.
    pub path: Vec<u8>,
    /// Rename/copy source bytes. Paths are never decoded lossily.
    pub old_path: Option<Vec<u8>>,
}

#[allow(
    dead_code,
    reason = "F0 declares all wire classes for the subsequent G implementation"
)]
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

#[allow(
    dead_code,
    reason = "F0 declares every mode for the subsequent G implementation"
)]
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

pub fn select(_changes: &[Change], _metadata_json: &str) -> Selection {
    Selection {
        mode: Mode::All,
        // Mode::All means the entire workspace, never an empty package selection.
        packages: Vec::new(),
        base: None,
        reasons: vec![Reason {
            path: String::new(),
            class: Class::Error,
            selects: "all".into(),
            why: "selector not available yet; conservatively selecting the entire workspace".into(),
        }],
    }
}

pub fn select_from_git(_repo: &Path, base: &str) -> Selection {
    let mut selection = select(&[], "");
    selection.base = Some(base.into());
    selection
}

pub fn run_cli(repo: &Path, args: SelectArgs) -> Result<()> {
    // Fixture paths are parsed but intentionally not read until G implements selection.
    let selection = select_from_git(repo, &args.base);
    match args.format {
        Format::Json => println!(
            "{}",
            serde_json::json!({
                "schema": 1,
                "mode": "all",
                "packages": selection.packages,
                "base": selection.base,
                "reasons": selection.reasons.iter().map(|r| serde_json::json!({
                    "path": r.path, "class": r.class.as_str(), "selects": r.selects, "why": r.why
                })).collect::<Vec<_>>()
            })
        ),
        Format::Github => {
            println!("mode=all\npackages=[]");
            eprintln!("{}", selection.reasons[0].why);
        }
        Format::Text => println!("mode: all\n{}", selection.reasons[0].why),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_never_narrows_even_on_invalid_inputs() {
        let changes = [Change {
            status: "invalid".into(),
            path: vec![0xff],
            old_path: None,
        }];
        for selection in [
            select(&changes, "invalid"),
            select_from_git(Path::new("/absent"), "bad"),
        ] {
            assert_eq!(selection.mode, Mode::All);
            assert!(
                selection
                    .reasons
                    .iter()
                    .any(|r| r.why.contains("not available yet"))
            );
        }
    }
}
