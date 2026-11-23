//! Bounded, deterministic assembly of contributor release notes.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};

const KINDS: [&str; 6] = [
    "added",
    "changed",
    "deprecated",
    "removed",
    "fixed",
    "security",
];
const DIR: &str = "docs/changelog/unreleased";

#[derive(Debug, Args)]
pub(crate) struct ChangelogArgs {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// Validate contributor fragments without changing files.
    Check,
    /// Assemble a release after bumping workspace.package.version.
    Assemble {
        #[arg(long)]
        version: String,
        #[arg(long)]
        date: String,
        #[arg(long)]
        dry_run: bool,
    },
}

struct Fragment {
    path: PathBuf,
    kind: usize,
    text: String,
    bytes: String,
}

// This deliberately small grammar is the binding compatibility contract (ADR 0021),
// rather than a general Markdown, TOML, SemVer or calendar parser.
fn version(s: &str) -> Result<[u64; 3]> {
    let parts: Vec<_> = s.split('.').collect();
    ensure!(parts.len() == 3, "invalid version: {s}");
    let mut out = [0; 3];
    for (i, p) in parts.iter().enumerate() {
        ensure!(
            !p.is_empty()
                && p.bytes().all(|c| c.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0')),
            "invalid version: {s}"
        );
        out[i] = p.parse().context("version component overflow")?;
    }
    Ok(out)
}
fn date(s: &str) -> Result<()> {
    ensure!(
        s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-',
        "invalid date: {s}"
    );
    ensure!(
        s.bytes()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()),
        "invalid date: {s}"
    );
    let y: u32 = s[..4].parse()?;
    let m: usize = s[5..7].parse()?;
    let d: u32 = s[8..].parse()?;
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    ensure!(
        y > 0 && (1..=12).contains(&m) && d > 0 && d <= days[m - 1],
        "invalid date: {s}"
    );
    Ok(())
}
fn read(path: &Path, limit: u64) -> Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "{}: expected regular file",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "{}: exceeds {limit} bytes",
        path.display()
    );
    String::from_utf8(bytes).with_context(|| format!("{}: invalid UTF-8", path.display()))
}
fn fragments(root: &Path) -> Result<Vec<Fragment>> {
    // Refuse symlinked ancestors too, including when the final directory is missing.
    let mut dir = root.to_path_buf();
    for part in ["docs", "changelog", "unreleased"] {
        dir.push(part);
        match fs::symlink_metadata(&dir) {
            Ok(m) => ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "{}: expected directory",
                dir.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        }
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&dir)? {
        entries.push(entry?);
        ensure!(entries.len() <= 501, "{DIR}: more than 500 fragments");
    }
    entries.sort_by_key(|e| e.file_name());
    let mut result = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let path = entry.path();
        let parsed = (|| -> Result<Option<Fragment>> {
            ensure!(entry.file_type()?.is_file(), "expected regular file");
            let name = entry.file_name();
            let name = name.to_str().context("non-UTF-8 filename")?;
            if name == "README.md" {
                return Ok(None);
            }
            let parts: Vec<_> = name.split('.').collect();
            ensure!(
                parts.len() == 3 && parts[2] == "md",
                "expected <slug>.<kind>.md"
            );
            let slug = parts[0];
            ensure!(
                slug.len() <= 64
                    && slug.split('-').all(|p| !p.is_empty()
                        && p.bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())),
                "invalid slug"
            );
            let kind = KINDS
                .iter()
                .position(|k| *k == parts[1])
                .context("invalid kind")?;
            let bytes = read(&path, 1000)?;
            let text = bytes.strip_suffix('\n').context("missing final LF")?;
            ensure!(
                !text.is_empty() && text.trim() == text,
                "empty line or edge whitespace"
            );
            ensure!(!text.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')), "control or bidi character");
            ensure!(
                text.as_bytes()[0].is_ascii_alphabetic()
                    || (text.starts_with('`') && !text.starts_with("``")),
                "invalid first character"
            );
            Ok(Some(Fragment {
                path: path.clone(),
                kind,
                text: text.to_owned(),
                bytes,
            }))
        })();
        match parsed {
            Ok(Some(f)) => result.push(f),
            Ok(None) => {}
            Err(e) => errors.push(format!("{}: {e:#}", path.display())),
        }
    }
    ensure!(result.len() <= 500, "more than 500 fragments");
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    Ok(result)
}

pub(crate) fn run(root: &Path, args: ChangelogArgs) -> Result<()> {
    let fragments = fragments(root)?;
    match args.command {
        Command::Check => println!("changelog: checked {} fragments", fragments.len()),
        Command::Assemble {
            version,
            date,
            dry_run,
        } => assemble(root, &fragments, &version, &date, dry_run)?,
    }
    Ok(())
}
fn assemble(root: &Path, fragments: &[Fragment], next: &str, day: &str, dry: bool) -> Result<()> {
    let next_v = version(next)?;
    date(day)?;
    let manifest = read(&root.join("Cargo.toml"), 1024 * 1024)?;
    let mut section = "";
    let mut versions = Vec::new();
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            section = line;
        }
        if section == "[workspace.package]"
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "version"
        {
            versions.push(value.trim().trim_matches('"'));
        }
    }
    ensure!(
        versions == [next],
        "version must equal workspace.package.version"
    );
    let path = root.join("CHANGELOG.md");
    let original = read(&path, 8 * 1024 * 1024)?;
    let lines: Vec<_> = original.split_inclusive('\n').collect();
    let unreleased: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_end_matches('\n') == "## [Unreleased]")
        .map(|(i, _)| i)
        .collect();
    ensure!(
        unreleased.len() == 1,
        "expected exactly one Unreleased heading"
    );
    let start = unreleased[0];
    let end = (start + 1..lines.len())
        .find(|i| lines[*i].starts_with("## ["))
        .context("missing previous release heading")?;
    let previous = lines[end]
        .strip_prefix("## [")
        .and_then(|s| s.split_once("] - "))
        .map(|(v, _)| v)
        .context("invalid previous release heading")?;
    ensure!(
        next_v > version(previous)?,
        "version must be newer than previous release"
    );
    ensure!(
        !original.contains(&format!("## [{next}]")) && !original.contains(&format!("[{next}]:")),
        "version already present"
    );
    let links: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("[Unreleased]:"))
        .collect();
    ensure!(
        links.len() == 1 && links[0].0 > end,
        "expected exactly one trailing Unreleased compare link"
    );
    let old_link = links[0].1.trim_end_matches('\n');
    let url = old_link
        .strip_prefix("[Unreleased]: ")
        .context("invalid compare link")?;
    let suffix = format!("/compare/v{previous}...HEAD");
    let base = url
        .strip_suffix(&suffix)
        .context("compare link disagrees with previous release")?;
    ensure!(!base.is_empty(), "empty compare base");
    let body = lines[start + 1..end].concat();
    let body = body.trim_matches('\n');
    ensure!(
        !body.is_empty() || !fragments.is_empty(),
        "no release notes"
    );
    let mut release = format!("## [{next}] - {day}\n\n");
    if !body.is_empty() {
        release.push_str(body);
        release.push_str("\n\n");
    }
    for (kind, name) in KINDS.iter().enumerate() {
        if !fragments.iter().any(|f| f.kind == kind) {
            continue;
        }
        let title = format!("{}{}", name[..1].to_ascii_uppercase(), &name[1..]);
        release.push_str(&format!("### {title}\n\n"));
        for f in fragments.iter().filter(|f| f.kind == kind) {
            release.push_str(&format!("- {}\n", f.text));
        }
        release.push('\n');
    }
    if dry {
        print!("{release}");
        return Ok(());
    }
    let mut output = lines[..=start].concat();
    output.push('\n');
    output.push_str(&release);
    for (i, line) in lines.iter().enumerate().skip(end) {
        if i == links[0].0 {
            output.push_str(&format!("[Unreleased]: {base}/compare/v{next}...HEAD\n[{next}]: {base}/compare/v{previous}...v{next}\n"));
        } else {
            output.push_str(line);
        }
    }
    let tmp = root.join("CHANGELOG.md.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .context("create exclusive CHANGELOG.md.tmp")?;
    let publish = (|| -> Result<()> {
        file.set_permissions(fs::metadata(&path)?.permissions())?;
        file.write_all(output.as_bytes())?;
        file.sync_all()?;
        ensure!(
            read(&path, 8 * 1024 * 1024)? == original,
            "changelog changed during assembly"
        );
        for f in fragments {
            ensure!(
                read(&f.path, 1000)? == f.bytes,
                "{} changed during assembly",
                f.path.display()
            );
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if let Err(e) = publish {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    println!(
        "changelog: assembled {next} ({day}): {} entries; wrote CHANGELOG.md",
        fragments.len()
    );
    let mut failures = Vec::new();
    for f in fragments {
        let remove = (|| -> Result<()> {
            ensure!(
                read(&f.path, 1000)? == f.bytes,
                "changed after assembly; remove consumed note manually"
            );
            fs::remove_file(&f.path)?;
            Ok(())
        })();
        if let Err(e) = remove {
            failures.push(format!("{}: {e:#}", f.path.display()));
        } else {
            println!(
                "changelog: removed {}",
                f.path.strip_prefix(root)?.display()
            );
        }
    }
    if !failures.is_empty() {
        bail!(
            "changelog committed; remove remaining consumed fragments manually before another release:\n{}",
            failures.join("\n")
        );
    }
    Ok(())
}
