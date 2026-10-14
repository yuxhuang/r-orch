use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use orch_core::process::ProcessManager;
use orch_core::workspace::{MergeResult, PrepareResult, WorkspaceManager};
use orch_core::{Error, Result};
use orch_domain::{Agent, DomainError, OrchestratorConfig, Task, WorkspaceMode};

use super::{MergeStrategy, run, run_for_code};

const NOT_A_GIT_REPO_HINT: &str = "Run: git init && git add -A && git commit -m \"Initial commit\"\n         Or set workspace_mode: shared in .orchestry/config.yml";
const WORKTREE_ADD_HINT: &str =
    "Run: git worktree prune && git branch | grep orchestry | xargs -r git branch -D";
const MAX_SLUG_LEN: usize = 40;

pub struct GitWorkspaceManager {
    project_root: PathBuf,
    orchestry_dir: PathBuf,
    process_manager: Arc<dyn ProcessManager>,
    merge_strategy: MergeStrategy,
    /// Only a positive probe is cached: the user may `git init` later.
    git_repo_confirmed: AtomicBool,
}

impl GitWorkspaceManager {
    pub fn new(
        project_root: PathBuf,
        orchestry_dir: PathBuf,
        process_manager: Arc<dyn ProcessManager>,
    ) -> Self {
        let merge_strategy = MergeStrategy::new(project_root.clone(), Arc::clone(&process_manager));
        Self {
            project_root,
            orchestry_dir,
            process_manager,
            merge_strategy,
            git_repo_confirmed: AtomicBool::new(false),
        }
    }

    fn resolve_mode(task: &Task, agent: &Agent, config: &OrchestratorConfig) -> WorkspaceMode {
        task.workspace_mode
            .or(agent.config.workspace_mode)
            .unwrap_or(config.defaults.agent.workspace_mode)
    }

    fn workspace_path(&self, task_id: &str) -> Result<PathBuf> {
        Ok(self
            .orchestry_dir
            .join("workspaces")
            .join(sanitize_path_segment(task_id)?))
    }

    async fn code(&self, command: &str, args: &[&str]) -> i32 {
        let code = run_for_code(&*self.process_manager, &self.project_root, command, args).await;
        tracing::debug!(command, ?args, code, "workspace command finished");
        code
    }

    async fn require_git_repo(&self, mode: WorkspaceMode) -> Result<()> {
        if self.git_repo_confirmed.load(Ordering::Relaxed) {
            return Ok(());
        }
        if self
            .code("git", &["rev-parse", "--is-inside-work-tree"])
            .await
            == 0
        {
            self.git_repo_confirmed.store(true, Ordering::Relaxed);
            return Ok(());
        }
        Err(workspace_error(
            format!("workspace_mode \"{mode}\" requires a git repository"),
            NOT_A_GIT_REPO_HINT,
        ))
    }

    async fn prepare_worktree(&self, task: &Task) -> Result<PrepareResult> {
        let workspace = self.workspace_path(&task.id)?;
        ensure_parent(&workspace).await?;

        let slug = sanitize_title(&task.title);
        let slug = if slug.is_empty() {
            sanitize_id(&task.id)?.to_owned()
        } else {
            slug
        };
        let branch = format!("orchestry/{}/{slug}", sanitize_id(&task.id)?);
        let result = PrepareResult {
            path: workspace.to_string_lossy().into_owned(),
            branch: Some(branch.clone()),
        };

        // Idempotent: a retry after a failure reuses the directory untouched.
        if tokio::fs::metadata(&workspace).await.is_ok() {
            return Ok(result);
        }

        let path = result.path.as_str();
        if self
            .code("git", &["worktree", "add", path, "-b", &branch])
            .await
            != 0
        {
            // The branch may survive from a failed run: prune stale metadata
            // and attach the worktree to the existing branch.
            self.code("git", &["worktree", "prune"]).await;
            let reuse = self.code("git", &["worktree", "add", path, &branch]).await;
            if reuse != 0 {
                tracing::error!(task = %task.id, %branch, code = reuse, "git worktree add failed");
                return Err(workspace_error(
                    format!("git worktree add failed with code {reuse}"),
                    WORKTREE_ADD_HINT,
                ));
            }
        }

        remove_nested_orchestry(&workspace).await;
        Ok(result)
    }

    async fn prepare_isolated(&self, task: &Task) -> Result<PathBuf> {
        let workspace = self.workspace_path(&task.id)?;
        ensure_parent(&workspace).await?;
        let target = workspace.to_string_lossy().into_owned();
        let root = self.project_root.to_string_lossy().into_owned();

        let cloned = self
            .code(
                "git",
                &["clone", "--local", "--no-hardlinks", &root, &target],
            )
            .await
            == 0;
        if !cloned {
            tracing::warn!(task = %task.id, "git clone failed; falling back to rsync");
            let exclude = format!(
                "--exclude-from={}",
                self.orchestry_dir.join("workspace-exclude").display()
            );
            let rsync = self
                .code("rsync", &["-a", &exclude, "./", &format!("{target}/")])
                .await;
            if rsync != 0 {
                return Err(Error::other(format!("rsync failed with code {rsync}")));
            }
        }

        remove_nested_orchestry(&workspace).await;
        Ok(workspace)
    }
}

#[async_trait]
impl WorkspaceManager for GitWorkspaceManager {
    async fn prepare(
        &self,
        task: &Task,
        agent: &Agent,
        config: &OrchestratorConfig,
    ) -> Result<PrepareResult> {
        let mode = Self::resolve_mode(task, agent, config);
        if mode != WorkspaceMode::Shared {
            self.require_git_repo(mode).await?;
        }
        match mode {
            WorkspaceMode::Shared => Ok(PrepareResult {
                path: self.project_root.to_string_lossy().into_owned(),
                branch: None,
            }),
            WorkspaceMode::Worktree => self.prepare_worktree(task).await,
            WorkspaceMode::Isolated => Ok(PrepareResult {
                path: self
                    .prepare_isolated(task)
                    .await?
                    .to_string_lossy()
                    .into_owned(),
                branch: None,
            }),
        }
    }

    async fn merge_back(&self, branch: &str) -> Result<MergeResult> {
        Ok(self.merge_strategy.merge_back(branch).await)
    }

    async fn cleanup(&self, task_id: &str, branch: Option<&str>) -> Result<()> {
        let workspace = self.workspace_path(task_id)?;
        let path = workspace.to_string_lossy().into_owned();

        // Removes the .git/worktrees metadata; the exit code is deliberately ignored.
        self.code("git", &["worktree", "remove", "--force", &path])
            .await;

        let delete_branch = async {
            if let Some(branch) = branch {
                self.code("git", &["branch", "-D", branch]).await;
            }
        };
        let remove_dir = async {
            if let Err(error) = tokio::fs::remove_dir_all(&workspace).await
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %workspace.display(), %error, "workspace directory removal failed");
            }
        };
        tokio::join!(delete_branch, remove_dir);
        Ok(())
    }

    fn validate(&self, workspace_path: &Path, project_root: &Path) -> Result<()> {
        let resolved = lexical_resolve(workspace_path);
        let root = lexical_resolve(project_root);
        if resolved.starts_with(&root) {
            Ok(())
        } else {
            Err(Error::other(format!(
                "Workspace path \"{}\" is outside project root",
                workspace_path.display()
            )))
        }
    }

    async fn get_changed_files(&self, branch: &str) -> Result<Vec<String>> {
        let base_args = [
            "merge-base".to_owned(),
            "HEAD".to_owned(),
            branch.to_owned(),
        ];
        let Ok(base) = run(
            &*self.process_manager,
            &self.project_root,
            "git",
            &base_args,
        )
        .await
        else {
            return Ok(Vec::new());
        };
        let merge_base = base.stdout.trim();
        if merge_base.is_empty() {
            return Ok(Vec::new());
        }
        let diff_args = [
            "diff".to_owned(),
            "--name-only".to_owned(),
            format!("{merge_base}...{branch}"),
        ];
        let Ok(diff) = run(
            &*self.process_manager,
            &self.project_root,
            "git",
            &diff_args,
        )
        .await
        else {
            return Ok(Vec::new());
        };
        let listing = diff.stdout.trim();
        if diff.code != 0 || listing.is_empty() {
            return Ok(Vec::new());
        }
        Ok(listing
            .split('\n')
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }
}

fn workspace_error(message: String, hint: &str) -> Error {
    Error::Domain(DomainError::Workspace {
        message,
        hint: Some(hint.to_owned()),
    })
}

/// `sanitize_id`, additionally rejecting `.` and `..` (security C2).
///
/// No extension is appended here, so `..` would resolve to the parent of
/// `workspaces/` — and this path is handed to `git worktree add` and to
/// `remove_dir_all`. Unreachable today because ids are generated, which is
/// precisely why it is worth pinning.
fn sanitize_path_segment(id: &str) -> Result<&str> {
    let id = sanitize_id(id)?;
    if id == "." || id == ".." {
        return Err(Error::other(format!(
            "Invalid identifier: \"{id}\" would escape its directory"
        )));
    }
    Ok(id)
}

/// `^[A-Za-z0-9._-]+$` — keeps ids from escaping `.orchestry/workspaces/`.
fn sanitize_id(id: &str) -> Result<&str> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(id)
    } else {
        Err(Error::other(format!("Invalid identifier: \"{id}\"")))
    }
}

/// Lowercase, collapse non-alphanumeric runs to `-`, strip edge dashes, cut to
/// 40. The cut comes last, so a slug can end in `-`.
pub(super) fn sanitize_title(title: &str) -> String {
    let mut slug = String::new();
    // A trailing separator run is never flushed, which is the `/-$/` strip.
    let mut pending_dash = false;
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending_dash {
                slug.push('-');
                pending_dash = false;
            }
            slug.push(c);
        } else if !slug.is_empty() {
            pending_dash = true;
        }
    }
    slug.chars().take(MAX_SLUG_LEN).collect()
}

async fn ensure_parent(workspace: &Path) -> Result<()> {
    let parent = workspace.parent().unwrap_or(workspace);
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| Error::io("creating the workspaces directory", parent, e))
}

/// Strip `.orchestry/` from the new workspace so nested state and workspaces
/// cannot recurse. Best effort, like the TS `.catch(() => {})`.
async fn remove_nested_orchestry(workspace: &Path) {
    let nested = workspace.join(".orchestry");
    if let Err(error) = tokio::fs::remove_dir_all(&nested).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %nested.display(), %error, "could not remove nested .orchestry");
    }
}

/// `path.resolve` without touching the filesystem.
fn lexical_resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}
