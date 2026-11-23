//! Disposable repository fixtures; never assemble the checkout's release notes.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::{TempDir, tempdir};

const BEFORE: &str = "# Changelog\n\n## [Unreleased]\n\n### Legacy\n\n- Keep  two spaces.\n\n## [1.0.0] - 2026-11-14\nHistorical text.\n\n[Unreleased]: https://example.org/repo/compare/v1.0.0...HEAD\n[1.0.0]: https://example.org/repo/compare/v0.9.0...v1.0.0\n";
fn fixture() -> TempDir {
    let t = tempdir().unwrap();
    fs::write(t.path().join("CHANGELOG.md"), BEFORE).unwrap();
    fs::write(
        t.path().join("Cargo.toml"),
        "[workspace.package]\nversion = \"1.0.1\"\n",
    )
    .unwrap();
    fs::create_dir_all(t.path().join("docs/changelog/unreleased")).unwrap();
    t
}
fn note(root: &Path, name: &str, bytes: &[u8]) {
    fs::write(root.join("docs/changelog/unreleased").join(name), bytes).unwrap();
}
fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .env_clear()
        .current_dir(root)
        .arg("changelog")
        .args(args)
        .output()
        .unwrap()
}
fn assemble(root: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["assemble", "--version", "1.0.1", "--date", "2026-12-01"];
    args.extend(extra);
    run(root, &args)
}
#[test]
fn two_fragments_legacy_history_links_and_rerun() {
    let t = fixture();
    note(t.path(), "z.fixed.md", b"Fix the final item.\n");
    note(t.path(), "a.added.md", b"Add the first item.\n");
    note(t.path(), "README.md", b"Retain me.\n");
    let out = assemble(t.path(), &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = BEFORE.replace("## [Unreleased]\n\n", "## [Unreleased]\n\n## [1.0.1] - 2026-12-01\n\n")
        .replace("## [1.0.0] -", "### Added\n\n- Add the first item.\n\n### Fixed\n\n- Fix the final item.\n\n## [1.0.0] -")
        .replace("[Unreleased]: https://example.org/repo/compare/v1.0.0...HEAD", "[Unreleased]: https://example.org/repo/compare/v1.0.1...HEAD\n[1.0.1]: https://example.org/repo/compare/v1.0.0...v1.0.1");
    assert_eq!(
        fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
        expected
    );
    assert_eq!(
        fs::read_dir(t.path().join("docs/changelog/unreleased"))
            .unwrap()
            .count(),
        1
    );
    // Simulate a leftover consumed note: retrying the same release cannot duplicate it.
    note(t.path(), "z.fixed.md", b"Fix the final item.\n");
    assert!(!assemble(t.path(), &[]).status.success());
    assert_eq!(
        fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
        expected
    );
    assert!(
        t.path()
            .join("docs/changelog/unreleased/z.fixed.md")
            .exists()
    );
}
#[test]
fn golden_no_legacy() {
    let t = fixture();
    let before = "## [Unreleased]\n\n## [1.0.0] - 2026-11-14\n...\n[Unreleased]: https://github.com/yuxhuang/r-orch/compare/v1.0.0...HEAD\n[1.0.0]: https://github.com/yuxhuang/r-orch/compare/v0.13.0...v1.0.0\n";
    fs::write(t.path().join("CHANGELOG.md"), before).unwrap();
    note(
        t.path(),
        "socket-timeout.fixed.md",
        b"Fix the client socket timeout so a slow daemon reply no longer ends a command early.\n",
    );
    let out = assemble(t.path(), &[]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "changelog: assembled 1.0.1 (2026-12-01): 1 entry (Fixed 1); existing Unreleased text: none\nchangelog: wrote CHANGELOG.md\nchangelog: removed docs/changelog/unreleased/socket-timeout.fixed.md\n"
    );
    assert_eq!(
        fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
        "## [Unreleased]\n\n## [1.0.1] - 2026-12-01\n\n### Fixed\n\n- Fix the client socket timeout so a slow daemon reply no longer ends a command early.\n\n## [1.0.0] - 2026-11-14\n...\n[Unreleased]: https://github.com/yuxhuang/r-orch/compare/v1.0.1...HEAD\n[1.0.1]: https://github.com/yuxhuang/r-orch/compare/v1.0.0...v1.0.1\n[1.0.0]: https://github.com/yuxhuang/r-orch/compare/v0.13.0...v1.0.0\n"
    );
}
#[test]
fn dry_run_check_and_legacy_only() {
    let t = fixture();
    note(t.path(), "a.changed.md", b"Change a thing.\n");
    assert!(run(t.path(), &["check"]).status.success());
    let output = assemble(t.path(), &["--dry-run"]);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("- Change a thing.")
    );
    assert_eq!(
        fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
        BEFORE
    );
    fs::remove_file(t.path().join("docs/changelog/unreleased/a.changed.md")).unwrap();
    assert!(assemble(t.path(), &[]).status.success());
}
#[test]
fn validation_refusals_preserve_files() {
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("Bad.fixed.md", b"Hello\n".to_vec()),
        ("a--b.fixed.md", b"Hello\n".to_vec()),
        ("a.other.md", b"Hello\n".to_vec()),
        ("a.fixed.txt", b"Hello\n".to_vec()),
        ("a.fixed.md", b"\n".to_vec()),
        ("a.fixed.md", b"Hello".to_vec()),
        ("a.fixed.md", b"Hello\nAgain\n".to_vec()),
        ("a.fixed.md", b" Hello\n".to_vec()),
        ("a.fixed.md", b"Hello \n".to_vec()),
        ("a.fixed.md", b"Hello\r\n".to_vec()),
        ("a.fixed.md", b"# Heading\n".to_vec()),
        ("a.fixed.md", b"``code\n".to_vec()),
        ("a.fixed.md", vec![255, 10]),
        ("a.fixed.md", vec![b'A'; 1001]),
        ("a.fixed.md", "Hello\u{2028}world\n".as_bytes().to_vec()),
        ("a.fixed.md", "Hello\u{2029}world\n".as_bytes().to_vec()),
        ("a.fixed.md", "Hello\u{202a}world\n".as_bytes().to_vec()),
        ("a.fixed.md", "Hello\u{2066}world\n".as_bytes().to_vec()),
    ];
    for (name, bytes) in bad {
        let t = fixture();
        note(t.path(), name, &bytes);
        let out = run(t.path(), &["check"]);
        assert!(!out.status.success(), "accepted {name}: {bytes:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(name));
        assert!(!assemble(t.path(), &[]).status.success());
        assert_eq!(
            fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
            BEFORE
        );
        assert_eq!(
            fs::read(t.path().join("docs/changelog/unreleased").join(name)).unwrap(),
            bytes
        );
    }
}
#[test]
fn dates_versions_structure_and_empty_notes_refuse() {
    let t = fixture();
    for (v, d) in [
        ("01.0.1", "2026-12-01"),
        ("v1.0.1", "2026-12-01"),
        ("1.0.0", "2026-12-01"),
        ("1.0.2", "2026-12-01"),
        ("1.0.1", "2026-02-29"),
        ("1.0.1", "2026-13-01"),
        ("1.0.1", "0000-01-01"),
    ] {
        assert!(
            !run(t.path(), &["assemble", "--version", v, "--date", d])
                .status
                .success()
        );
    }
    for text in [
        BEFORE.replace("v1.0.0...HEAD", "v0.9.0...HEAD"),
        BEFORE.replace("### Legacy\n\n- Keep  two spaces.\n\n", ""),
        format!("## [Unreleased]\n{BEFORE}"),
    ] {
        fs::write(t.path().join("CHANGELOG.md"), &text).unwrap();
        assert!(!assemble(t.path(), &[]).status.success());
        assert_eq!(
            fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
            text
        );
    }
}
#[test]
fn exclusive_temp_failure_does_not_consume() {
    let t = fixture();
    note(t.path(), "a.fixed.md", b"Fix a thing.\n");
    fs::write(t.path().join("CHANGELOG.md.tmp"), "Existing temp").unwrap();
    refused(
        assemble(t.path(), &[]),
        "inspect and remove any stale temporary file",
    );
    assert_eq!(
        fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
        BEFORE
    );
    assert_eq!(
        fs::read_to_string(t.path().join("CHANGELOG.md.tmp")).unwrap(),
        "Existing temp"
    );
    assert!(
        t.path()
            .join("docs/changelog/unreleased/a.fixed.md")
            .exists()
    );
}
#[test]
fn directory_file_symlinks_and_limits() {
    use std::os::unix::fs::symlink;
    let t = fixture();
    let dir = t.path().join("docs/changelog/unreleased");
    fs::create_dir(dir.join("stray")).unwrap();
    assert!(!run(t.path(), &["check"]).status.success());
    fs::remove_dir(dir.join("stray")).unwrap();
    symlink(t.path().join("CHANGELOG.md"), dir.join("a.fixed.md")).unwrap();
    assert!(!run(t.path(), &["check"]).status.success());
    fs::remove_file(dir.join("a.fixed.md")).unwrap();
    for i in 0..501 {
        note(t.path(), &format!("note-{i}.fixed.md"), b"Fix a thing.\n");
    }
    assert!(!run(t.path(), &["check"]).status.success());
    fs::remove_dir_all(&dir).unwrap();
    assert!(run(t.path(), &["check"]).status.success());
    symlink(t.path(), &dir).unwrap();
    assert!(!run(t.path(), &["check"]).status.success());
}

fn refused(out: Output, reason: &str) {
    assert!(!out.status.success(), "unexpected success: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(reason),
        "expected {reason:?}: {out:?}"
    );
}

#[test]
fn subdirectories_cannot_silently_skip_validation() {
    let t = fixture();
    note(t.path(), "Bad.fixed.md", b"Hello\n");
    for sub in ["docs", "docs/changelog/unreleased"] {
        refused(
            run(&t.path().join(sub), &["check"]),
            "run from the repository root",
        );
        refused(
            assemble(&t.path().join(sub), &[]),
            "run from the repository root",
        );
    }
    refused(run(t.path(), &["check"]), "invalid slug");
}

#[test]
fn byte_and_slug_boundaries_pin_the_refusal_reason() {
    let t = fixture();
    let name = format!("{}.fixed.md", "a".repeat(64));
    let mut bytes = vec![b'A'; 999];
    bytes.push(b'\n');
    note(t.path(), &name, &bytes);
    assert!(run(t.path(), &["check"]).status.success());
    bytes.insert(0, b'A');
    note(t.path(), &name, &bytes);
    refused(run(t.path(), &["check"]), "exceeds 1000 bytes");
    fs::remove_file(t.path().join("docs/changelog/unreleased").join(name)).unwrap();
    note(
        t.path(),
        &format!("{}.fixed.md", "a".repeat(65)),
        b"Hello\n",
    );
    refused(run(t.path(), &["check"]), "invalid slug");
}

#[test]
fn same_kind_filename_byte_order_and_mode_survive_assembly() {
    use std::os::unix::fs::PermissionsExt;
    let t = fixture();
    for name in ["b", "a", "a-b"] {
        note(
            t.path(),
            &format!("{name}.fixed.md"),
            format!("Fix {name}.\n").as_bytes(),
        );
    }
    fs::set_permissions(
        t.path().join("CHANGELOG.md"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let out = assemble(t.path(), &[]);
    assert!(out.status.success(), "{out:?}");
    let text = fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap();
    assert!(text.contains("- Fix a-b.\n- Fix a.\n- Fix b.\n"));
    assert_eq!(
        fs::metadata(t.path().join("CHANGELOG.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("3 entries (Fixed 3); existing Unreleased text: present")
    );
}

#[test]
fn agreeing_manifest_versions_and_structure_refuse_for_the_right_reason() {
    let t = fixture();
    for (v, reason) in [
        ("0.9.9", "newer than previous"),
        ("1.0.01", "invalid version"),
    ] {
        fs::write(
            t.path().join("Cargo.toml"),
            format!("[workspace.package]\nversion = \"{v}\"\n"),
        )
        .unwrap();
        refused(
            run(
                t.path(),
                &["assemble", "--version", v, "--date", "2026-12-01"],
            ),
            reason,
        );
    }
    fs::write(
        t.path().join("Cargo.toml"),
        "[workspace.package]\nversion = \"1.0.1\"\n",
    )
    .unwrap();
    for (text, reason) in [
        (
            BEFORE.replace("v1.0.0...HEAD", "v0.9.0...HEAD"),
            "compare link disagrees",
        ),
        (
            format!("## [Unreleased]\n{BEFORE}"),
            "exactly one Unreleased heading",
        ),
        (
            format!("{BEFORE}[1.0.1]: https://example.org/old\n"),
            "version already present",
        ),
        (
            BEFORE.replace("### Legacy\n\n- Keep  two spaces.\n\n", " \t\n\n"),
            "no release notes",
        ),
    ] {
        fs::write(t.path().join("CHANGELOG.md"), &text).unwrap();
        refused(assemble(t.path(), &[]), reason);
        assert_eq!(
            fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
            text
        );
    }
    fs::write(
        t.path().join("CHANGELOG.md"),
        BEFORE.replace(
            "Historical text.",
            "Historical mention of [1.0.1]: as prose.",
        ),
    )
    .unwrap();
    assert!(assemble(t.path(), &[]).status.success());
}

#[test]
fn untrusted_diagnostics_are_escaped_in_captured_output() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let t = fixture();
    let dir = t.path().join("docs/changelog/unreleased");
    for name in [
        b"bad\x1b]52;c;test\x07.fixed.md".to_vec(),
        b"bad\n\t.fixed.md".to_vec(),
        "bad\u{202e}.fixed.md".as_bytes().to_vec(),
        b"bad\xff.fixed.md".to_vec(),
    ] {
        let path = dir.join(OsString::from_vec(name));
        fs::write(&path, b"Hello\n").unwrap();
        for args in [
            vec!["check"],
            vec!["assemble", "--version", "1.0.1", "--date", "2026-12-01"],
        ] {
            let out = run(t.path(), &args);
            assert!(!out.status.success());
            assert!(out.stdout.is_empty());
            let text = String::from_utf8(out.stderr).unwrap();
            assert!(text.contains(&format!("{path:?}")), "{text:?}");
            assert!(
                !text
                    .chars()
                    .any(|c| (c.is_control() && c != '\n') || c == '\u{202e}'),
                "{text:?}"
            );
        }
        assert_eq!(fs::read(&path).unwrap(), b"Hello\n");
        assert_eq!(
            fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
            BEFORE
        );
        fs::remove_file(path).unwrap();
    }
    // Parent paths and argument values are independent untrusted diagnostic sources.
    let odd = t.path().join("parent\x1b\u{202e}");
    fs::create_dir(&odd).unwrap();
    fs::copy(t.path().join("CHANGELOG.md"), odd.join("CHANGELOG.md")).unwrap();
    fs::create_dir_all(odd.join("docs/changelog")).unwrap();
    fs::write(odd.join("docs/changelog/unreleased"), b"not a directory").unwrap();
    let out = run(&odd, &["check"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("expected directory"));
    assert!(!stderr.contains(['\x1b', '\u{202e}']));
    for (option, value) in [("--version", "1.\x1b.0"), ("--date", "2026-\u{202e}-01")] {
        let mut args = vec!["assemble", "--version", "1.0.1", "--date", "2026-12-01"];
        let i = args.iter().position(|s| *s == option).unwrap();
        args[i + 1] = value;
        let out = run(t.path(), &args);
        assert!(!out.status.success());
        assert!(
            !String::from_utf8(out.stderr)
                .unwrap()
                .contains(['\x1b', '\u{202e}'])
        );
    }
}

#[test]
fn post_rename_unlink_failure_reports_each_note_and_blocks_rerun() {
    use std::os::unix::fs::PermissionsExt;
    let t = fixture();
    note(t.path(), "a.fixed.md", b"Fix a.\n");
    note(t.path(), "b.fixed.md", b"Fix b.\n");
    let dir = t.path().join("docs/changelog/unreleased");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let out = assemble(t.path(), &[]);
    // Restore fixture permissions even if the assertion below fails.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    refused(
        out.clone(),
        "changelog committed; remove remaining consumed fragments manually",
    );
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("a.fixed.md") && err.contains("b.fixed.md"));
    let text = fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap();
    assert!(text.contains("## [1.0.1] - 2026-12-01"));
    assert!(dir.join("a.fixed.md").exists() && dir.join("b.fixed.md").exists());
    refused(assemble(t.path(), &[]), "newer than previous");
    assert_eq!(
        fs::read_to_string(t.path().join("CHANGELOG.md")).unwrap(),
        text
    );
}
