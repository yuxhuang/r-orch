use std::fs;
use std::path::Path;
use std::sync::Arc;

use orch_core::test_support::{make_agent, make_task};
use orch_core::workspace::{MergeResult, WorkspaceManager};
use orch_core::{Error, Result};
use orch_domain::{Agent, AgentConfig, DomainError, Task, WorkspaceMode, default_config};

use super::manager::sanitize_title;
use super::test_support::{Behavior, TestProcessManager};
use super::{GitWorkspaceManager, MergeStrategy};

const ROOT: &str = "/proj";

fn manager(pm: &Arc<TestProcessManager>) -> GitWorkspaceManager {
    GitWorkspaceManager::new(ROOT.into(), "/proj/.orchestry".into(), pm.clone())
}

fn ok_pm() -> Arc<TestProcessManager> {
    Arc::new(TestProcessManager::with(|_, _| Behavior::Script {
        stdout: "",
        stderr: "",
        code: 0,
    }))
}

fn failing(commands: &'static [&'static str]) -> Arc<TestProcessManager> {
    // Fail any invocation whose "cmd sub" prefix is listed.
    Arc::new(TestProcessManager::with(move |cmd, args| {
        let key = format!("{cmd} {}", args.first().map(String::as_str).unwrap_or(""));
        let code = if commands.contains(&key.as_str()) {
            1
        } else {
            0
        };
        Behavior::Script {
            stdout: "",
            stderr: "",
            code,
        }
    }))
}

fn task_with(mode: Option<WorkspaceMode>) -> Task {
    Task {
        id: "tsk_abc1234".into(),
        title: "Fix Login Bug!".into(),
        workspace_mode: mode,
        ..make_task()
    }
}

fn agent_with(mode: Option<WorkspaceMode>) -> Agent {
    Agent {
        config: AgentConfig {
            workspace_mode: mode,
            ..Default::default()
        },
        ..make_agent()
    }
}

async fn prepare(
    m: &GitWorkspaceManager,
    task: &Task,
    agent: &Agent,
) -> Result<orch_core::workspace::PrepareResult> {
    m.prepare(task, agent, &default_config()).await
}

fn workspace_error(err: &Error) -> (&str, Option<&str>) {
    match err {
        Error::Domain(DomainError::Workspace { message, hint }) => {
            (message.as_str(), hint.as_deref())
        }
        other => panic!("expected a WorkspaceError, got {other:?}"),
    }
}

// ── merge strategy ─────────────────────────────────────────────────────────

fn merge_pm(stdout: &'static str, stderr: &'static str, code: i32) -> Arc<TestProcessManager> {
    Arc::new(TestProcessManager::with(move |_, args| {
        if args.first().is_some_and(|a| a == "--abort")
            || args.get(1).is_some_and(|a| a == "--abort")
        {
            Behavior::Script {
                stdout: "",
                stderr: "",
                code: 0,
            }
        } else {
            Behavior::Script {
                stdout,
                stderr,
                code,
            }
        }
    }))
}

fn strategy(pm: &Arc<TestProcessManager>) -> MergeStrategy {
    MergeStrategy::new(ROOT.into(), pm.clone())
}

#[tokio::test]
async fn clean_merge_succeeds_with_one_spawn_and_exact_argv() {
    let pm = merge_pm("Merge made by the 'ort' strategy.", "", 0);
    assert_eq!(
        strategy(&pm).merge_back("orchestry/t1/x").await,
        MergeResult::Success
    );
    assert_eq!(
        pm.lines(),
        ["git merge --no-ff orchestry/t1/x -m Merge orchestry/t1/x"]
    );
    assert_eq!(pm.calls()[0].args[4], "Merge orchestry/t1/x");
    assert_eq!(pm.calls()[0].cwd, Path::new(ROOT));
}

#[tokio::test]
async fn conflict_on_stdout_aborts_the_merge() {
    let pm = merge_pm("CONFLICT (content): Merge conflict in a.ts", "", 1);
    let result = strategy(&pm).merge_back("b").await;
    assert_eq!(
        result.conflict_info(),
        Some("CONFLICT (content): Merge conflict in a.ts")
    );
    assert_eq!(
        pm.lines(),
        ["git merge --no-ff b -m Merge b", "git merge --abort"]
    );
}

#[tokio::test]
async fn conflict_on_stderr_is_also_a_conflict() {
    let pm = merge_pm("", "Merge conflict in src/index.ts", 1);
    let result = strategy(&pm).merge_back("b").await;
    assert!(!result.success());
    assert_eq!(pm.calls().len(), 2);
}

#[tokio::test]
async fn non_conflict_failure_does_not_abort() {
    let pm = merge_pm("", "fatal: branch not found", 128);
    let result = strategy(&pm).merge_back("b").await;
    assert_eq!(result.conflict_info(), Some("fatal: branch not found"));
    assert_eq!(pm.calls().len(), 1);
}

#[tokio::test]
async fn spawn_error_reports_its_message() {
    let pm = Arc::new(TestProcessManager::with(|_, _| {
        Behavior::SpawnError("spawn git ENOENT")
    }));
    let result = strategy(&pm).merge_back("b").await;
    assert_eq!(result.conflict_info(), Some("spawn git ENOENT"));
}

#[tokio::test]
async fn conflict_info_is_cut_to_a_thousand_chars() {
    let long: &'static str = Box::leak(format!("CONFLICT {}", "x".repeat(2000)).into_boxed_str());
    let pm = merge_pm(long, "", 1);
    let result = strategy(&pm).merge_back("b").await;
    assert_eq!(result.conflict_info().unwrap().len(), 1000);
}

#[tokio::test]
async fn a_failing_abort_still_resolves() {
    let pm = Arc::new(TestProcessManager::with(|_, args| {
        if args[1] == "--abort" {
            Behavior::SpawnError("abort failed")
        } else {
            Behavior::Script {
                stdout: "CONFLICT x",
                stderr: "",
                code: 1,
            }
        }
    }));
    let result = strategy(&pm).merge_back("b").await;
    assert_eq!(result.conflict_info(), Some("CONFLICT x"));
}

// ── mode resolution ────────────────────────────────────────────────────────

#[tokio::test]
async fn shared_mode_returns_the_project_root_without_any_git_call() {
    let pm = ok_pm();
    let r = prepare(
        &manager(&pm),
        &task_with(Some(WorkspaceMode::Shared)),
        &agent_with(None),
    )
    .await
    .unwrap();
    assert_eq!(r.path, ROOT);
    assert_eq!(r.branch, None);
    assert!(pm.calls().is_empty());
}

#[tokio::test]
async fn task_mode_beats_agent_mode_beats_config_default() {
    let pm = ok_pm();
    let m = manager(&pm);
    // task override: shared wins over the agent's isolated.
    let r = prepare(
        &m,
        &task_with(Some(WorkspaceMode::Shared)),
        &agent_with(Some(WorkspaceMode::Isolated)),
    )
    .await
    .unwrap();
    assert_eq!(r.path, ROOT);
    // agent override: shared wins over the default worktree.
    let r = prepare(
        &m,
        &task_with(None),
        &agent_with(Some(WorkspaceMode::Shared)),
    )
    .await
    .unwrap();
    assert_eq!(r.path, ROOT);
    // config default.
    let mut config = default_config();
    config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    let r = m
        .prepare(&task_with(None), &agent_with(None), &config)
        .await
        .unwrap();
    assert_eq!(r.path, ROOT);
    assert!(pm.calls().is_empty());
}

// ── worktree ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn worktree_add_uses_the_exact_argv_and_branch_name() {
    let dir = tempfile::tempdir().unwrap();
    let pm = ok_pm();
    let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join(".orchestry"), pm.clone());
    let r = prepare(
        &m,
        &task_with(Some(WorkspaceMode::Worktree)),
        &agent_with(None),
    )
    .await
    .unwrap();
    let path = dir.path().join(".orchestry/workspaces/tsk_abc1234");
    assert_eq!(r.path, path.to_string_lossy());
    assert_eq!(
        r.branch.as_deref(),
        Some("orchestry/tsk_abc1234/fix-login-bug")
    );
    assert_eq!(
        pm.lines(),
        [
            "git rev-parse --is-inside-work-tree".to_owned(),
            format!(
                "git worktree add {} -b orchestry/tsk_abc1234/fix-login-bug",
                path.display()
            ),
        ]
    );
}

#[tokio::test]
async fn titles_without_ascii_alphanumerics_fall_back_to_the_task_id() {
    for (id, title) in [("tsk_cjk1", "修复数据库连接"), ("tsk_emo1", "🚀🎉✨")] {
        let dir = tempfile::tempdir().unwrap();
        let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join("o"), ok_pm());
        let task = Task {
            id: id.into(),
            title: title.into(),
            ..task_with(None)
        };
        let r = prepare(&m, &task, &agent_with(None)).await.unwrap();
        assert_eq!(r.branch, Some(format!("orchestry/{id}/{id}")));
    }
}

#[test]
fn title_slugs_are_trimmed_then_cut_to_forty() {
    assert_eq!(sanitize_title("  --Hello,   World!!  "), "hello-world");
    let cut = sanitize_title(&format!("{} b", "a".repeat(39)));
    assert_eq!(cut, format!("{}-", "a".repeat(39)));
    assert_eq!(sanitize_title(&"a".repeat(60)).len(), 40);
}

#[tokio::test]
async fn failed_worktree_add_prunes_then_reuses_the_branch() {
    let dir = tempfile::tempdir().unwrap();
    let pm = Arc::new(TestProcessManager::with(move |_, args| {
        let is_new_branch_add = args.iter().any(|a| a == "-b");
        Behavior::Script {
            stdout: "",
            stderr: "",
            code: i32::from(is_new_branch_add),
        }
    }));
    let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join("o"), pm.clone());
    let r = prepare(
        &m,
        &task_with(Some(WorkspaceMode::Worktree)),
        &agent_with(None),
    )
    .await
    .unwrap();
    let path = r.path.clone();
    assert_eq!(
        pm.lines(),
        [
            "git rev-parse --is-inside-work-tree".to_owned(),
            format!("git worktree add {path} -b orchestry/tsk_abc1234/fix-login-bug"),
            "git worktree prune".to_owned(),
            format!("git worktree add {path} orchestry/tsk_abc1234/fix-login-bug"),
        ]
    );
}

#[tokio::test]
async fn worktree_add_failing_twice_is_a_workspace_error_with_a_hint() {
    let dir = tempfile::tempdir().unwrap();
    let pm = failing(&["git worktree"]);
    let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join("o"), pm);
    let err = prepare(&m, &task_with(None), &agent_with(None))
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), 6);
    let (message, hint) = workspace_error(&err);
    assert_eq!(message, "git worktree add failed with code 1");
    assert_eq!(
        hint,
        Some("Run: git worktree prune && git branch | grep orchestry | xargs -r git branch -D")
    );
}

#[tokio::test]
async fn an_existing_workspace_directory_is_reused_without_touching_git_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("o/workspaces/tsk_abc1234");
    fs::create_dir_all(&existing).unwrap();
    let pm = ok_pm();
    let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join("o"), pm.clone());
    let r = prepare(&m, &task_with(None), &agent_with(None))
        .await
        .unwrap();
    assert_eq!(r.path, existing.to_string_lossy());
    assert_eq!(pm.lines(), ["git rev-parse --is-inside-work-tree"]);
}

// ── isolated ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn isolated_mode_clones_locally_without_hardlinks() {
    let dir = tempfile::tempdir().unwrap();
    let pm = ok_pm();
    let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join("o"), pm.clone());
    let r = prepare(
        &m,
        &task_with(Some(WorkspaceMode::Isolated)),
        &agent_with(None),
    )
    .await
    .unwrap();
    assert_eq!(r.branch, None);
    assert_eq!(
        pm.lines()[1],
        format!("git clone --local --no-hardlinks {ROOT} {}", r.path)
    );
    assert_eq!(pm.calls().len(), 2);
}

#[tokio::test]
async fn failed_clone_falls_back_to_rsync_with_the_exclude_file() {
    let dir = tempfile::tempdir().unwrap();
    let pm = failing(&["git clone"]);
    let orchestry = dir.path().join("o");
    let m = GitWorkspaceManager::new(ROOT.into(), orchestry.clone(), pm.clone());
    let r = prepare(
        &m,
        &task_with(Some(WorkspaceMode::Isolated)),
        &agent_with(None),
    )
    .await
    .unwrap();
    assert_eq!(
        pm.lines()[2],
        format!(
            "rsync -a --exclude-from={}/workspace-exclude ./ {}/",
            orchestry.display(),
            r.path
        )
    );
}

#[tokio::test]
async fn rsync_failure_is_a_plain_error() {
    let dir = tempfile::tempdir().unwrap();
    let pm = failing(&["git clone", "rsync -a"]);
    let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join("o"), pm);
    let err = prepare(
        &m,
        &task_with(Some(WorkspaceMode::Isolated)),
        &agent_with(None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.to_string(), "rsync failed with code 1");
    assert_eq!(err.exit_code(), 1);
}

// ── git precondition ───────────────────────────────────────────────────────

#[tokio::test]
async fn non_git_projects_reject_worktree_and_isolated_with_the_hint() {
    for mode in [WorkspaceMode::Worktree, WorkspaceMode::Isolated] {
        let pm = failing(&["git rev-parse"]);
        let err = prepare(&manager(&pm), &task_with(Some(mode)), &agent_with(None))
            .await
            .unwrap_err();
        assert_eq!(err.exit_code(), 6);
        let (message, hint) = workspace_error(&err);
        assert_eq!(
            message,
            format!("workspace_mode \"{mode}\" requires a git repository")
        );
        assert_eq!(
            hint,
            Some(
                "Run: git init && git add -A && git commit -m \"Initial commit\"\n         Or set workspace_mode: shared in .orchestry/config.yml"
            )
        );
    }
}

#[tokio::test]
async fn a_positive_repo_probe_is_cached_but_a_negative_is_reprobed() {
    let dir = tempfile::tempdir().unwrap();
    let ok = ok_pm();
    let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join("o"), ok.clone());
    for id in ["tsk_a", "tsk_b"] {
        let task = Task {
            id: id.into(),
            ..task_with(None)
        };
        prepare(&m, &task, &agent_with(None)).await.unwrap();
    }
    let probes = ok
        .lines()
        .iter()
        .filter(|l| l.contains("rev-parse"))
        .count();
    assert_eq!(probes, 1);

    let bad = failing(&["git rev-parse"]);
    let m = manager(&bad);
    for _ in 0..2 {
        prepare(&m, &task_with(None), &agent_with(None))
            .await
            .unwrap_err();
    }
    assert_eq!(bad.calls().len(), 2);
}

// ── cleanup / validate ─────────────────────────────────────────────────────

#[tokio::test]
async fn cleanup_removes_the_worktree_then_deletes_the_branch_and_directory() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("o/workspaces/tsk_1");
    fs::create_dir_all(ws.join("sub")).unwrap();
    let pm = ok_pm();
    let m = GitWorkspaceManager::new(ROOT.into(), dir.path().join("o"), pm.clone());
    m.cleanup("tsk_1", Some("orchestry/tsk_1/x")).await.unwrap();
    assert_eq!(
        pm.lines(),
        [
            format!("git worktree remove --force {}", ws.display()),
            "git branch -D orchestry/tsk_1/x".to_owned(),
        ]
    );
    assert!(!ws.exists());
}

#[tokio::test]
async fn cleanup_without_branch_skips_branch_deletion_and_tolerates_missing_dirs() {
    let pm = failing(&["git worktree"]);
    let m = GitWorkspaceManager::new(ROOT.into(), "/nonexistent/o".into(), pm.clone());
    m.cleanup("tsk_1", None).await.unwrap();
    assert_eq!(pm.calls().len(), 1);
}

#[tokio::test]
async fn cleanup_rejects_path_like_ids() {
    let pm = ok_pm();
    assert!(manager(&pm).cleanup("../evil", None).await.is_err());
    assert!(pm.calls().is_empty());
}

#[test]
fn validate_requires_containment_in_the_project_root() {
    let m = manager(&ok_pm());
    let check = |w: &str| m.validate(Path::new(w), Path::new("/proj"));
    assert!(check("/proj").is_ok());
    assert!(check("/proj/.orchestry/workspaces/t").is_ok());
    assert!(check("/proj/a/../b").is_ok());
    assert!(check("/proj-other").is_err());
    assert!(check("/proj/../etc").is_err());
    assert_eq!(
        check("/elsewhere").unwrap_err().to_string(),
        "Workspace path \"/elsewhere\" is outside project root"
    );
}

#[tokio::test]
async fn merge_back_delegates_to_the_strategy() {
    let pm = merge_pm("", "", 0);
    assert!(manager(&pm).merge_back("b").await.unwrap().success());
}

// ── real git ───────────────────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    fs::write(dir.path().join("a.txt"), "base\n").unwrap();
    fs::create_dir_all(dir.path().join(".orchestry")).unwrap();
    git(dir.path(), &["add", "a.txt"]);
    git(dir.path(), &["commit", "-qm", "init"]);
    dir
}

fn real_manager(root: &Path) -> GitWorkspaceManager {
    GitWorkspaceManager::new(
        root.to_path_buf(),
        root.join(".orchestry"),
        Arc::new(TestProcessManager::real()),
    )
}

#[tokio::test]
async fn real_git_worktree_lifecycle() {
    let repo = init_repo();
    let m = real_manager(repo.path());
    let task = task_with(Some(WorkspaceMode::Worktree));
    let r = prepare(&m, &task, &agent_with(None)).await.unwrap();
    let branch = r.branch.clone().unwrap();
    let ws = Path::new(&r.path);
    assert!(ws.join("a.txt").exists());
    assert!(!ws.join(".orchestry").exists());
    assert!(git(repo.path(), &["branch", "--list", &branch]).contains(&branch));

    // Idempotent re-prepare.
    assert_eq!(
        prepare(&m, &task, &agent_with(None)).await.unwrap().path,
        r.path
    );

    // No changes yet.
    assert!(m.get_changed_files(&branch).await.unwrap().is_empty());

    fs::write(ws.join("b.txt"), "new\n").unwrap();
    fs::create_dir_all(ws.join("src")).unwrap();
    fs::write(ws.join("src/c.txt"), "new\n").unwrap();
    git(ws, &["add", "."]);
    git(ws, &["commit", "-qm", "work"]);
    assert_eq!(
        m.get_changed_files(&branch).await.unwrap(),
        ["b.txt", "src/c.txt"]
    );

    // Merge needs identity; supply it through the environment git reads.
    set_identity(repo.path());
    assert!(m.merge_back(&branch).await.unwrap().success());
    assert!(repo.path().join("b.txt").exists());
    assert!(
        git(repo.path(), &["log", "-1", "--format=%s"]).starts_with(&format!("Merge {branch}"))
    );

    m.cleanup(&task.id, Some(&branch)).await.unwrap();
    assert!(!ws.exists());
    assert!(
        git(repo.path(), &["branch", "--list", &branch])
            .trim()
            .is_empty()
    );
}

fn set_identity(repo: &Path) {
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "user.email", "t@t"]);
}

#[tokio::test]
async fn real_git_conflict_is_aborted_and_the_tree_is_left_clean() {
    let repo = init_repo();
    set_identity(repo.path());
    git(repo.path(), &["checkout", "-qb", "feature"]);
    fs::write(repo.path().join("a.txt"), "feature\n").unwrap();
    git(repo.path(), &["commit", "-qam", "feature"]);
    git(repo.path(), &["checkout", "-q", "main"]);
    fs::write(repo.path().join("a.txt"), "main\n").unwrap();
    git(repo.path(), &["commit", "-qam", "main"]);

    let result = real_manager(repo.path())
        .merge_back("feature")
        .await
        .unwrap();
    assert!(result.conflict_info().unwrap().contains("CONFLICT"));
    assert!(
        git(repo.path(), &["status", "--porcelain"])
            .trim()
            .is_empty()
    );
    assert!(!repo.path().join(".git/MERGE_HEAD").exists());
}

#[tokio::test]
async fn real_git_missing_branch_is_a_failure_without_abort() {
    let repo = init_repo();
    let result = real_manager(repo.path()).merge_back("nope").await.unwrap();
    assert!(!result.success());
    assert!(result.conflict_info().unwrap().contains("nope"));
}

#[tokio::test]
async fn real_git_clone_isolated_workspace() {
    let repo = init_repo();
    let r = prepare(
        &real_manager(repo.path()),
        &task_with(Some(WorkspaceMode::Isolated)),
        &agent_with(None),
    )
    .await
    .unwrap();
    assert!(Path::new(&r.path).join("a.txt").exists());
    assert!(!Path::new(&r.path).join(".orchestry").exists());
}

#[tokio::test]
async fn real_git_outside_a_repo_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let err = prepare(
        &real_manager(dir.path()),
        &task_with(None),
        &agent_with(None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.exit_code(), 6);
}

#[tokio::test]
async fn changed_files_is_empty_for_unknown_branches() {
    let repo = init_repo();
    assert!(
        real_manager(repo.path())
            .get_changed_files("nope")
            .await
            .unwrap()
            .is_empty()
    );
}
