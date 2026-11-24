//! Offline bootstrap uses the selected project's Git context.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    temp: tempfile::TempDir,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("selected project");
        for path in [
            &project,
            &temp.path().join("home"),
            &temp.path().join("tmp"),
        ] {
            fs::create_dir(path).unwrap();
        }
        Self { temp, project }
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .current_dir(cwd)
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.temp.path().join("home"))
            .env("TMPDIR", self.temp.path().join("tmp"))
            .env("XDG_CONFIG_HOME", self.temp.path().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Bootstrap Test")
            .env("GIT_AUTHOR_EMAIL", "bootstrap@example.invalid")
            .env("GIT_COMMITTER_NAME", "Bootstrap Test")
            .env("GIT_COMMITTER_EMAIL", "bootstrap@example.invalid")
            .env(
                "ORCHESTRY_GLOBAL_CONFIG_PATH",
                self.temp.path().join("home/global.yml"),
            )
            .env("ORCH_PROJECT_ROOT", cwd)
            .env("ORCH_SOCKET", self.temp.path().join("unused.sock"));
        cmd
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut cmd = self.command("git", cwd);
        cmd.args(args);
        run(cmd)
    }

    fn init(&self, cwd: &Path) -> Command {
        let mut cmd = self.command(env!("CARGO_BIN_EXE_orch"), cwd);
        cmd.args(["init", "--adapter", "shell"]);
        cmd
    }

    fn repo(&self, name: &str, committed: bool) -> PathBuf {
        let root = self.temp.path().join(name);
        fs::create_dir(&root).unwrap();
        self.git(&root, &["init"]);
        fs::write(root.join("sentinel"), "synthetic content\n").unwrap();
        self.git(&root, &["add", "sentinel"]);
        if committed {
            self.git(&root, &["commit", "-m", "Fixture"]);
        }
        root
    }
}

fn run(mut cmd: Command) -> Output {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "command timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

// Include directory entries, so creating an empty Git directory is observable.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let directory = path.is_dir();
            out.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                if directory {
                    None
                } else {
                    Some(fs::read(&path).unwrap())
                },
            );
            if directory {
                visit(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn assert_unchanged(root: &Path, before: &BTreeMap<PathBuf, Option<Vec<u8>>>) {
    let after = snapshot(root);
    let changed: Vec<_> = before
        .keys()
        .chain(after.keys())
        .filter(|key| before.get(*key) != after.get(*key))
        .collect();
    assert!(
        changed.is_empty(),
        "unrelated repository changed: {changed:?}"
    );
}

#[test]
fn init_ignores_inherited_repository_routing() {
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
    ] {
        let fixture = Fixture::new();
        let decoy = fixture.repo("unrelated", false);
        let before = snapshot(&decoy);
        let mut cmd = fixture.init(&fixture.project);
        let value = match variable {
            "GIT_WORK_TREE" => decoy.clone(),
            "GIT_INDEX_FILE" => decoy.join(".git/index"),
            "GIT_OBJECT_DIRECTORY" => decoy.join(".git/objects"),
            _ => decoy.join(".git"),
        };
        cmd.env(variable, value);
        run(cmd);
        assert_unchanged(&decoy, &before);
        assert!(fixture.project.join(".git").is_dir(), "{variable}");
        fixture.git(&fixture.project, &["rev-parse", "--verify", "HEAD"]);
        let tree = fixture.git(&fixture.project, &["ls-tree", "--name-only", "HEAD"]);
        assert_eq!(
            String::from_utf8(tree.stdout).unwrap().trim(),
            ".gitignore",
            "{variable}"
        );
    }
}

#[test]
fn init_ignores_inherited_inline_git_configuration() {
    for parameters in [false, true] {
        let fixture = Fixture::new();
        let decoy = fixture.repo("unrelated", false);
        let before = snapshot(&decoy);
        let mut cmd = fixture.init(&fixture.project);
        if parameters {
            cmd.env(
                "GIT_CONFIG_PARAMETERS",
                format!("'core.worktree'='{}'", decoy.display()),
            );
        } else {
            cmd.env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "core.worktree")
                .env("GIT_CONFIG_VALUE_0", &decoy);
        }
        run(cmd);
        assert_unchanged(&decoy, &before);
        fixture.git(&fixture.project, &["rev-parse", "--verify", "HEAD"]);
        let tree = fixture.git(&fixture.project, &["ls-tree", "--name-only", "HEAD"]);
        assert_eq!(String::from_utf8(tree.stdout).unwrap().trim(), ".gitignore");
    }
}

#[test]
fn init_preserves_existing_repository_history_and_index() {
    let fixture = Fixture::new();
    let repo = fixture.repo("existing", true);
    fs::write(repo.join("staged"), "staged content\n").unwrap();
    fixture.git(&repo, &["add", "staged"]);
    let before = snapshot(&repo.join(".git"));
    run(fixture.init(&repo));
    assert_unchanged(&repo.join(".git"), &before);
    assert!(repo.join(".orchestry/config.yml").is_file());
}

#[test]
fn init_creates_first_commit_in_existing_unborn_repository() {
    let fixture = Fixture::new();
    let repo = fixture.repo("unborn", false);
    run(fixture.init(&repo));
    let tree = fixture.git(&repo, &["ls-tree", "--name-only", "HEAD"]);
    assert_eq!(
        String::from_utf8(tree.stdout).unwrap(),
        ".gitignore\nsentinel\n"
    );
}

#[test]
fn init_preserves_linked_worktree_metadata() {
    let fixture = Fixture::new();
    let repo = fixture.repo("existing", true);
    let linked = fixture.temp.path().join("linked");
    fixture.git(
        &repo,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let decoy = fixture.repo("unrelated", false);
    let decoy_before = snapshot(&decoy);
    let before = snapshot(&repo.join(".git"));
    let pointer = fs::read(linked.join(".git")).unwrap();
    let mut cmd = fixture.init(&linked);
    cmd.env("GIT_DIR", decoy.join(".git"))
        .env("GIT_WORK_TREE", &decoy);
    run(cmd);
    assert_unchanged(&decoy, &decoy_before);
    assert_unchanged(&repo.join(".git"), &before);
    assert_eq!(fs::read(linked.join(".git")).unwrap(), pointer);
    assert!(linked.join(".orchestry/config.yml").is_file());
}
