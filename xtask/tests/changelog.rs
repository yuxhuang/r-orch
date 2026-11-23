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
    assert!(assemble(t.path(), &[]).status.success());
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
    assert!(!assemble(t.path(), &[]).status.success());
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
