//! Path resolution for `.orchestry/`.
//!
//! All path construction goes through here, and every id embedded in a path
//! goes through [`sanitize_id`]. That is the path-traversal defence and it
//! **rejects** rather than sanitizes.

use std::path::{Component, Path, PathBuf};

use orch_core::error::{Error, Result};
use orch_domain::DomainError;

/// The project-root-relative directory everything lives in.
pub const ORCHESTRY_DIR: &str = ".orchestry";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    project_root: PathBuf,
}

impl Paths {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Paths {
            project_root: project_root.into(),
        }
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// `<root>/.orchestry`
    pub fn root(&self) -> PathBuf {
        self.project_root.join(ORCHESTRY_DIR)
    }

    pub fn config_path(&self) -> PathBuf {
        self.root().join("config.yml")
    }

    pub fn remotes_path(&self) -> PathBuf {
        self.root().join("remotes.yml")
    }

    pub fn remotes_dir(&self) -> PathBuf {
        self.root().join("remotes")
    }

    pub fn peer_id_path(&self) -> PathBuf {
        self.root().join("peer-id")
    }

    pub fn state_path(&self) -> PathBuf {
        self.root().join("state.json")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root().join("orchestry.lock")
    }

    pub fn tasks_dir(&self) -> PathBuf {
        self.root().join("tasks")
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.root().join("agents")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.root().join("runs")
    }

    pub fn templates_dir(&self) -> PathBuf {
        self.root().join("templates")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root().join("logs")
    }

    pub fn context_dir(&self) -> PathBuf {
        self.root().join("context")
    }

    pub fn messages_dir(&self) -> PathBuf {
        self.root().join("messages")
    }

    pub fn goals_dir(&self) -> PathBuf {
        self.root().join("goals")
    }

    pub fn teams_dir(&self) -> PathBuf {
        self.root().join("teams")
    }

    pub fn attachments_dir(&self) -> PathBuf {
        self.root().join("attachments")
    }

    pub fn workspaces_dir(&self) -> PathBuf {
        self.root().join("workspaces")
    }

    pub fn gitignore_path(&self) -> PathBuf {
        self.root().join(".gitignore")
    }

    pub fn workspace_exclude_path(&self) -> PathBuf {
        self.root().join("workspace-exclude")
    }

    pub fn default_template_path(&self) -> PathBuf {
        self.templates_dir().join("default.md")
    }

    pub fn task_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.tasks_dir().join(format!("{}.yml", sanitize_id(id)?)))
    }

    pub fn agent_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.agents_dir().join(format!("{}.yml", sanitize_id(id)?)))
    }

    pub fn goal_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.goals_dir().join(format!("{}.yml", sanitize_id(id)?)))
    }

    pub fn team_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.teams_dir().join(format!("{}.yml", sanitize_id(id)?)))
    }

    pub fn context_path(&self, key: &str) -> Result<PathBuf> {
        Ok(self
            .context_dir()
            .join(format!("{}.json", sanitize_id(key)?)))
    }

    pub fn message_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self
            .messages_dir()
            .join(format!("{}.json", sanitize_id(id)?)))
    }

    pub fn run_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.runs_dir().join(format!("{}.json", sanitize_id(id)?)))
    }

    pub fn run_events_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.runs_dir().join(format!("{}.jsonl", sanitize_id(id)?)))
    }

    pub fn task_attachments_dir(&self, task_id: &str) -> Result<PathBuf> {
        Ok(self.attachments_dir().join(sanitize_path_segment(task_id)?))
    }

    pub async fn is_initialized(&self) -> bool {
        tokio::fs::metadata(self.root()).await.is_ok()
    }

    /// `NotInitializedError` (exit 3) when `.orchestry/` is absent.
    pub async fn require_init(&self) -> Result<()> {
        if self.is_initialized().await {
            Ok(())
        } else {
            Err(Error::Domain(DomainError::NotInitialized))
        }
    }
}

/// Validate an identifier for use in a file path: `^[A-Za-z0-9._-]+$`.
///
/// Rejects — never rewrites — anything else, so a context key containing `/`
/// or `..` is an error rather than a silently different key. The message is
/// contract: `Invalid identifier: "<id>"`.
pub fn sanitize_id(id: &str) -> Result<&str> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if valid {
        Ok(id)
    } else {
        Err(Error::other(format!("Invalid identifier: \"{id}\"")))
    }
}

/// `sanitize_id`, additionally rejecting `.` and `..` (security C2).
///
/// For the builders that append an **extension**, `sanitize_id` is enough: an
/// id of `..` yields `tasks/...yml`, an odd file name and nothing more.
/// `task_attachments_dir` appends no extension, so `..` would resolve to the
/// *parent* directory — and `attachments/..` is `.orchestry/`.
///
/// Unreachable today, because every id is generated: the regex
/// `^[A-Za-z0-9._-]+$` accepts `..` but nanoid never produces it. That makes
/// this a guard against a future caller passing a user-supplied id, which is
/// exactly the kind of assumption that stops holding quietly.
fn sanitize_path_segment(id: &str) -> Result<&str> {
    let id = sanitize_id(id)?;
    if id == "." || id == ".." {
        return Err(Error::other(format!(
            "Invalid identifier: \"{id}\" would escape its directory"
        )));
    }
    Ok(id)
}

/// Reject a workspace path outside the project root.
///
/// Uses lexical normalization rather than `canonicalize`, because it must
/// not touch the filesystem and the path may not exist yet.
pub fn validate_workspace_path(workspace_path: &Path, project_root: &Path) -> Result<()> {
    let resolved = lexically_resolve(workspace_path);
    let root = lexically_resolve(project_root);

    if resolved == root || resolved.starts_with(&root) {
        Ok(())
    } else {
        Err(Error::other(format!(
            "Workspace path \"{}\" is outside project root",
            workspace_path.display()
        )))
    }
}

/// `path.resolve`: make absolute against the cwd, then collapse `.` and `..`
/// without consulting the filesystem.
fn lexically_resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };

    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Walk up from `start_dir` looking for `.orchestry/`; return `start_dir`
/// resolved when none is found, which is what `init` needs.
///
/// Not memoized: a CLI process runs one command and exits, so a cache would
/// never be hit a second time.
pub fn find_project_root(start_dir: &Path) -> PathBuf {
    let start = lexically_resolve(start_dir);
    let mut dir = start.as_path();
    loop {
        if dir.join(ORCHESTRY_DIR).exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => return start,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths::new("/tmp/project")
    }

    #[test]
    fn the_extension_less_builders_reject_dot_and_dotdot() {
        // `sanitize_id` accepts both, because `.` is a legal id character —
        // harmless where an extension follows (`tasks/...yml`), a traversal
        // where none does: `attachments/..` is `.orchestry/` itself.
        let paths = Paths::new(Path::new("/tmp/project"));
        for id in [".", ".."] {
            assert!(
                paths.task_attachments_dir(id).is_err(),
                "attachments/{id} must be refused"
            );
            // Still fine for the builders that append an extension.
            assert!(sanitize_id(id).is_ok(), "{id} is a valid id generally");
        }
        assert!(paths.task_attachments_dir("tsk_abc1234").is_ok());
        // And a nested traversal was already impossible: `/` is not allowed.
        assert!(paths.task_attachments_dir("../etc").is_err());
    }

    #[test]
    fn every_path_hangs_off_the_orchestry_dir() {
        let p = paths();
        assert_eq!(p.root(), Path::new("/tmp/project/.orchestry"));
        assert_eq!(
            p.config_path(),
            Path::new("/tmp/project/.orchestry/config.yml")
        );
        assert_eq!(
            p.state_path(),
            Path::new("/tmp/project/.orchestry/state.json")
        );
        assert_eq!(
            p.lock_path(),
            Path::new("/tmp/project/.orchestry/orchestry.lock")
        );
        assert_eq!(p.tasks_dir(), Path::new("/tmp/project/.orchestry/tasks"));
        assert_eq!(p.agents_dir(), Path::new("/tmp/project/.orchestry/agents"));
        assert_eq!(p.goals_dir(), Path::new("/tmp/project/.orchestry/goals"));
        assert_eq!(p.teams_dir(), Path::new("/tmp/project/.orchestry/teams"));
        assert_eq!(p.runs_dir(), Path::new("/tmp/project/.orchestry/runs"));
        assert_eq!(p.logs_dir(), Path::new("/tmp/project/.orchestry/logs"));
        assert_eq!(
            p.context_dir(),
            Path::new("/tmp/project/.orchestry/context")
        );
        assert_eq!(
            p.messages_dir(),
            Path::new("/tmp/project/.orchestry/messages")
        );
        assert_eq!(
            p.templates_dir(),
            Path::new("/tmp/project/.orchestry/templates")
        );
        assert_eq!(
            p.attachments_dir(),
            Path::new("/tmp/project/.orchestry/attachments")
        );
        assert_eq!(
            p.workspaces_dir(),
            Path::new("/tmp/project/.orchestry/workspaces")
        );
        assert_eq!(
            p.gitignore_path(),
            Path::new("/tmp/project/.orchestry/.gitignore")
        );
        assert_eq!(
            p.workspace_exclude_path(),
            Path::new("/tmp/project/.orchestry/workspace-exclude")
        );
        assert_eq!(
            p.default_template_path(),
            Path::new("/tmp/project/.orchestry/templates/default.md")
        );
    }

    #[test]
    fn entity_paths_use_the_documented_extensions() {
        let p = paths();
        assert_eq!(p.task_path("tsk_1").unwrap().extension().unwrap(), "yml");
        assert_eq!(p.agent_path("agt_1").unwrap().extension().unwrap(), "yml");
        assert_eq!(p.goal_path("goal_1").unwrap().extension().unwrap(), "yml");
        assert_eq!(p.team_path("team_1").unwrap().extension().unwrap(), "yml");
        assert_eq!(p.context_path("k").unwrap().extension().unwrap(), "json");
        assert_eq!(
            p.message_path("msg_1").unwrap().extension().unwrap(),
            "json"
        );
        assert_eq!(p.run_path("run_1").unwrap().extension().unwrap(), "json");
        assert_eq!(
            p.run_events_path("run_1").unwrap().extension().unwrap(),
            "jsonl"
        );
        assert_eq!(
            p.task_path("tsk_1").unwrap(),
            Path::new("/tmp/project/.orchestry/tasks/tsk_1.yml")
        );
    }

    /// V30 — the path-traversal table.
    #[test]
    fn sanitize_id_rejects_path_traversal() {
        for id in ["tsk_abc", "agt_creator", "a.b", "A-Z_0-9", "."] {
            assert_eq!(sanitize_id(id).unwrap(), id, "{id} should be accepted");
        }
        for id in [
            "",
            "../etc/passwd",
            "..\\etc",
            "/etc/passwd",
            "a/b",
            "a\\b",
            "a b",
            "a:b",
            "a*b",
            "üü",
            "a\0b",
            "a\nb",
        ] {
            let err = sanitize_id(id).expect_err("should be rejected");
            assert_eq!(err.to_string(), format!("Invalid identifier: \"{id}\""));
            assert_eq!(err.exit_code(), 1);
        }
    }

    /// `..` passes `^[A-Za-z0-9._-]+$` — `.` is in the character class — so the
    /// id check **accepts** it. Verified against the regex rather than
    /// assumed.
    ///
    /// It is harmless everywhere an extension is appended (`..` + `.yml` is
    /// the ordinary filename `...yml`). The one builder that appends nothing
    /// is `task_attachments_dir`, where `..` would resolve to the parent
    /// directory — a latent traversal, unreachable because
    /// task ids are generated, never user-supplied. Recorded here.
    #[test]
    fn dot_dot_is_accepted_by_the_id_regex() {
        assert_eq!(sanitize_id("..").unwrap(), "..");
        let paths = paths();
        assert_eq!(
            paths.task_path("..").unwrap(),
            Path::new("/tmp/project/.orchestry/tasks/...yml")
        );
        assert_eq!(
            paths.context_path("..").unwrap(),
            Path::new("/tmp/project/.orchestry/context/...json")
        );
        // The one builder with no extension to absorb it **used** to return
        // `attachments/..`, i.e. `.orchestry/` itself. The id-level
        // acceptance above still follows the regex — that is what this test
        // exists to record — but the extension-less builder now refuses it
        // (security C2). The acceptance and the guard are different things,
        // and both are asserted.
        assert!(paths.task_attachments_dir("..").is_err());
    }

    #[test]
    fn an_invalid_id_fails_the_path_builder_rather_than_escaping() {
        let p = paths();
        assert!(p.task_path("../../etc/passwd").is_err());
        assert!(p.context_path("a/b").is_err());
    }

    #[test]
    fn workspace_paths_must_be_inside_the_project_root() {
        let root = Path::new("/tmp/project");
        assert!(validate_workspace_path(Path::new("/tmp/project"), root).is_ok());
        assert!(validate_workspace_path(Path::new("/tmp/project/sub"), root).is_ok());
        assert!(
            validate_workspace_path(Path::new("/tmp/project/.orchestry/workspaces/t"), root)
                .is_ok()
        );
        // `..` is collapsed lexically, so this escapes and is rejected.
        assert!(validate_workspace_path(Path::new("/tmp/project/../other"), root).is_err());
        assert!(validate_workspace_path(Path::new("/tmp/other"), root).is_err());
        // A sibling with the root as a string prefix is not inside it.
        assert!(validate_workspace_path(Path::new("/tmp/project-other"), root).is_err());

        let err = validate_workspace_path(Path::new("/tmp/other"), root).expect_err("outside");
        assert_eq!(
            err.to_string(),
            "Workspace path \"/tmp/other\" is outside project root"
        );
    }

    #[test]
    fn find_project_root_walks_up_and_falls_back_to_the_start() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("proj");
        let nested = root.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(root.join(ORCHESTRY_DIR)).unwrap();

        assert_eq!(find_project_root(&nested), lexically_resolve(&root));
        assert_eq!(find_project_root(&root), lexically_resolve(&root));

        // No project anywhere above → the resolved start directory.
        let orphan = temp.path().join("orphan");
        std::fs::create_dir_all(&orphan).unwrap();
        let found = find_project_root(&orphan);
        // One expected value, not a disjunction: an assertion that accepts
        // either answer cannot fail if the fallback changes (code m4). With
        // no `.orchestry` anywhere above it, `find_project_root` falls back
        // to the directory it started from.
        assert_eq!(found, lexically_resolve(&orphan));
    }

    #[tokio::test]
    async fn require_init_fails_with_exit_3_when_absent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = Paths::new(temp.path());
        assert!(!paths.is_initialized().await);
        let err = paths.require_init().await.expect_err("not initialized");
        assert_eq!(err.exit_code(), 3);
        assert_eq!(err.to_string(), "Not initialized");
        assert_eq!(err.hint().as_deref(), Some("Run: orch init"));

        std::fs::create_dir_all(paths.root()).unwrap();
        assert!(paths.is_initialized().await);
        assert!(paths.require_init().await.is_ok());
    }
}
