use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use super::{Change, MAX_CHANGES, MAX_INPUT, MAX_PATH, classify, process};

pub(super) fn fixture_inputs(changed: &Path, metadata: &Path) -> Result<(Vec<Change>, String)> {
    let changes = name_status(&read_file(changed)?)?;
    let metadata = String::from_utf8(read_file(metadata)?).context("non-UTF-8 metadata")?;
    Ok((changes, metadata))
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).context("cannot inspect fixture file")?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_INPUT as u64,
        "fixture is not a bounded regular file"
    );
    let file = open_input(path).context("cannot open fixture file")?;
    let opened = file.metadata()?;
    ensure!(opened.is_file(), "fixture changed type");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            metadata.dev() == opened.dev() && metadata.ino() == opened.ino(),
            "fixture replaced while opening"
        );
    }
    let mut bytes = Vec::new();
    file.take(MAX_INPUT as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_INPUT, "fixture exceeds input bound");
    Ok(bytes)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn open_input(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // Compatibility exemption: F0 forbids adding libc/nix or changing manifests.
    // These OS ABI flags are O_NOFOLLOW | O_NONBLOCK (Darwin sys/fcntl.h;
    // Linux asm-generic/fcntl.h). A raced FIFO must not block open, and a raced
    // symlink must not be followed before the regular-file/identity checks.
    #[cfg(target_os = "macos")]
    const FLAGS: i32 = 0x100 | 0x4;
    #[cfg(target_os = "linux")]
    const FLAGS: i32 = 0x20000 | 0x800;
    File::options().read(true).custom_flags(FLAGS).open(path)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn open_input(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "bounded fixture input requires macOS or Linux",
    ))
}

// Compatibility exemption (ARCHITECTURE §4.8): the frozen fixture contract is
// Git's NUL name-status protocol with raw byte paths. A line/patch parser would
// lose filenames, and no new dependency is permitted in this slice.
pub(super) fn name_status(raw: &[u8]) -> Result<Vec<Change>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(raw.len() <= MAX_INPUT, "change stream exceeds input bound");
    let mut fields = raw
        .strip_suffix(&[0])
        .context("unterminated change stream")?
        .split(|b| *b == 0);
    let mut changes = Vec::new();
    while let Some(status) = fields.next() {
        ensure!(
            !status.is_empty() && status.len() <= 4,
            "malformed change status"
        );
        let status = std::str::from_utf8(status)
            .context("non-UTF-8 status")?
            .to_owned();
        let first = fields.next().context("change path missing")?;
        let (path, old_path) = if status.starts_with('R') || status.starts_with('C') {
            (
                fields.next().context("rename/copy destination missing")?,
                Some(first.to_vec()),
            )
        } else {
            (first, None)
        };
        ensure!(
            changes.len() < MAX_CHANGES
                && path.len() <= MAX_PATH
                && old_path.as_ref().is_none_or(|p| p.len() <= MAX_PATH),
            "change count/path exceeds bound"
        );
        changes.push(Change {
            status,
            path: path.to_vec(),
            old_path,
        });
    }
    Ok(changes)
}

pub(super) fn inputs(repo: &Path, base: &str) -> Result<(Vec<Change>, String)> {
    ensure!(
        !base.is_empty() && base.len() <= MAX_PATH && !base.contains('\0'),
        "invalid base revision"
    );
    // A single deadline covers all queries, not a fresh budget for every path.
    let deadline = Instant::now() + Duration::from_secs(30);
    let top = run_git(repo, &["rev-parse", "--show-toplevel"], deadline)?;
    let top = top
        .strip_suffix(b"\n")
        .context("malformed repository root")?;
    let root = PathBuf::from(std::str::from_utf8(top).context("non-UTF-8 repository root")?);
    ensure!(root.is_absolute(), "repository root is not absolute");
    let revision = format!("{base}^{{commit}}");
    let base_id = object_id(&run_git(
        &root,
        &["rev-parse", "--verify", "--end-of-options", &revision],
        deadline,
    )?)?;
    let merge = object_id(&run_git(
        &root,
        &["merge-base", &base_id, "HEAD"],
        deadline,
    )?)?;
    let mut changes = Vec::new();
    // The net working-tree diff is required by D19. The index and unstaged
    // projections also retain an edit staged then reverted only in the worktree.
    for extra in [
        vec![merge.as_str()],
        vec!["--cached", merge.as_str()],
        Vec::new(),
    ] {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "--name-status",
            "-z",
            "-M",
        ];
        args.extend(extra.iter().copied());
        args.push("--");
        changes.extend(name_status(&run_git(&root, &args, deadline)?)?);
        let mut raw = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            "-z",
        ];
        raw.extend(extra);
        raw.push("--");
        changes.extend(special_modes(&run_git(&root, &raw, deadline)?)?);
        ensure!(changes.len() <= MAX_CHANGES, "change count exceeds bound");
    }
    changes.extend(status(&run_git(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
        deadline,
    )?)?);
    let untracked = run_git(
        &root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        deadline,
    )?;
    for path in nul_paths(&untracked)? {
        changes.push(Change {
            status: "?".into(),
            path: path.to_vec(),
            old_path: None,
        });
    }
    ensure!(changes.len() <= MAX_CHANGES, "change count exceeds bound");
    // Inspect existing components without following links. Raw modes above also
    // cover deleted/replaced symlinks and gitlinks, which no longer exist here.
    let mut special = Vec::new();
    for change in &changes {
        for bytes in std::iter::once(change.path.as_slice()).chain(change.old_path.as_deref()) {
            if let Ok(path) = classify::path(bytes)
                && special_file(&root, path)?
            {
                special.push(Change {
                    status: "T".into(),
                    path: bytes.to_vec(),
                    old_path: None,
                });
            }
        }
        ensure!(
            Instant::now() < deadline,
            "selector filesystem deadline exceeded"
        );
    }
    changes.extend(special);
    ensure!(changes.len() <= MAX_CHANGES, "change count exceeds bound");
    let mut cargo = Command::new("cargo");
    cargo
        .current_dir(&root)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .env("CARGO_NET_OFFLINE", "true")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER");
    let metadata =
        String::from_utf8(process::capture(cargo, deadline).context("cargo metadata failed")?)
            .context("non-UTF-8 metadata")?;
    Ok((changes, metadata))
}

fn run_git(repo: &Path, args: &[&str], deadline: Instant) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .current_dir(repo)
        .args([
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_EXTERNAL_DIFF",
        "GIT_DIFF_OPTS",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(name);
    }
    process::capture(command, deadline).context("git query failed")
}

fn object_id(bytes: &[u8]) -> Result<String> {
    let id = bytes
        .strip_suffix(b"\n")
        .context("unterminated Git object id")?;
    ensure!(
        matches!(id.len(), 40 | 64) && id.iter().all(u8::is_ascii_hexdigit),
        "malformed Git object id"
    );
    Ok(std::str::from_utf8(id)?.to_owned())
}

fn nul_paths(raw: &[u8]) -> Result<Vec<&[u8]>> {
    ensure!(raw.len() <= MAX_INPUT, "path stream exceeds input bound");
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for path in raw
        .strip_suffix(&[0])
        .context("unterminated path stream")?
        .split(|b| *b == 0)
    {
        ensure!(
            !path.is_empty() && path.len() <= MAX_PATH && result.len() < MAX_CHANGES,
            "empty/oversized path stream"
        );
        result.push(path);
    }
    Ok(result)
}

fn special_modes(raw: &[u8]) -> Result<Vec<Change>> {
    let paths = nul_paths(raw)?;
    ensure!(paths.len() % 2 == 0, "malformed raw diff arity");
    let mut changes = Vec::new();
    for pair in paths.as_chunks::<2>().0 {
        let header = std::str::from_utf8(pair[0]).context("non-UTF-8 raw diff header")?;
        let fields: Vec<_> = header.split(' ').collect();
        ensure!(
            fields.len() == 5 && fields[0].starts_with(':'),
            "malformed raw diff header"
        );
        ensure!(
            fields[0].len() == 7
                && fields[1].len() == 6
                && fields[0][1..]
                    .bytes()
                    .chain(fields[1].bytes())
                    .all(|b| (b'0'..=b'7').contains(&b)),
            "malformed raw diff mode"
        );
        for id in &fields[2..4] {
            ensure!(
                matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit()),
                "malformed raw diff object id"
            );
        }
        ensure!(
            matches!(fields[4], "A" | "D" | "M" | "T" | "U"),
            "unknown raw diff status"
        );
        if ![&fields[0][1..], fields[1]]
            .iter()
            .all(|mode| matches!(*mode, "000000" | "100644" | "100755"))
        {
            changes.push(Change {
                status: "T".into(),
                path: pair[1].to_vec(),
                old_path: None,
            });
        }
    }
    Ok(changes)
}

fn status(raw: &[u8]) -> Result<Vec<Change>> {
    let fields = nul_paths(raw)?;
    let mut fields = fields.into_iter();
    let mut changes = Vec::new();
    while let Some(record) = fields.next() {
        ensure!(
            record.len() >= 4 && record[2] == b' ',
            "malformed status record"
        );
        let xy = &record[..2];
        let path = record[3..].to_vec();
        let (status, old_path) = if xy == b"??" {
            ("?", None)
        } else {
            ensure!(
                xy.iter().all(|b| b" MADRCUT".contains(b)) && xy != b"  ",
                "unknown status token"
            );
            if xy.contains(&b'U') || matches!(xy, b"DD" | b"AA") {
                ("U", None)
            } else if xy.contains(&b'T') {
                ("T", None)
            } else if xy.contains(&b'R') || xy.contains(&b'C') {
                let old = fields.next().context("status rename/copy source missing")?;
                (
                    if xy.contains(&b'R') { "R100" } else { "C100" },
                    Some(old.to_vec()),
                )
            } else {
                (
                    if xy.contains(&b'D') {
                        "D"
                    } else if xy.contains(&b'A') {
                        "A"
                    } else {
                        "M"
                    },
                    None,
                )
            }
        };
        changes.push(Change {
            status: status.into(),
            path,
            old_path,
        });
    }
    Ok(changes)
}

fn special_file(root: &Path, relative: &str) -> Result<bool> {
    let mut path = root.to_path_buf();
    let components: Vec<_> = relative.split('/').collect();
    for (index, component) in components.iter().enumerate() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || (index + 1 == components.len() && !metadata.is_file())
                    || (index + 1 < components.len() && !metadata.is_dir())
                {
                    return Ok(true);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(anyhow::anyhow!("cannot inspect changed path")),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_protocol_retains_rename_copy_delete_and_hostile_paths() {
        let changes = name_status(b"R100\0docs/old.md\0crates/leaf/a b\n.rs\0C75\0crates/old/x\0crates/new/x\0D\0-leading ;$(data)\0").unwrap();
        assert_eq!(changes.len(), 3);
        assert_eq!(
            changes[0].old_path.as_deref(),
            Some(b"docs/old.md".as_slice())
        );
        assert_eq!(changes[0].path, b"crates/leaf/a b\n.rs");
        assert_eq!(changes[1].status, "C75");
        assert_eq!(changes[2].path, b"-leading ;$(data)");
        for invalid in [b"M\0".as_slice(), b"M\0x", b"R100\0old\0", b"\0x\0"] {
            assert!(name_status(invalid).is_err());
        }
    }

    #[test]
    fn non_utf8_is_retained_until_fail_closed_classification() {
        let changes = name_status(b"M\0docs/\xff.md\0").unwrap();
        let selection = super::super::select(&changes, &super::super::tests::metadata());
        assert_eq!(selection.mode, super::super::Mode::All);
        assert!(
            selection
                .reasons
                .iter()
                .any(|r| r.why.contains("non-UTF-8"))
        );
    }

    #[test]
    fn status_includes_index_worktree_untracked_and_both_rename_names() {
        let changes = status(
            b"M  crates/a/x\0 M crates/b/x\0?? docs/new.md\0R  crates/d/new\0crates/c/old\0",
        )
        .unwrap();
        assert_eq!(changes.len(), 4);
        assert_eq!(changes[2].status, "?");
        assert_eq!(
            changes[3].old_path.as_deref(),
            Some(b"crates/c/old".as_slice())
        );
        assert!(status(b"ZZ docs/x\0").is_err());
        assert!(status(b"R  new\0").is_err());
    }

    #[test]
    fn raw_modes_identify_deleted_links_submodules_and_new_special_files() {
        for (old, new) in [
            ("120000", "000000"),
            ("160000", "160000"),
            ("100644", "120000"),
        ] {
            let raw = format!(
                ":{old} {new} {} {} M\0docs/link\0",
                "a".repeat(40),
                "b".repeat(40)
            );
            assert_eq!(special_modes(raw.as_bytes()).unwrap()[0].status, "T");
        }
        let regular = format!(
            ":100644 100755 {} {} M\0docs/file\0",
            "a".repeat(40),
            "b".repeat(40)
        );
        assert!(special_modes(regular.as_bytes()).unwrap().is_empty());
        assert!(special_modes(b":100644 100755 a b M\0docs/file\0").is_err());
    }
}
