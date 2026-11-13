//! Offline project creation. The serving lifecycle never calls this module.
use crate::DaemonError;
use orch_core::error::{Error, Result};
use orch_domain::{WorkspaceMode, default_agents, default_config};
use orch_infra::storage::Paths;
use orch_infra::storage::fs_utils::{
    atomic_write, ensure_dir, owner_only, path_exists, write_yaml_with_mode,
};
use std::path::{Path, PathBuf};

/// The combined default prompt template, from WP-3's
/// `orch_infra::template`.
///
/// WP-0b carried its own byte-exact copy while WP-3 was building in parallel;
/// that copy is gone now the real module has landed, because two sources of
/// truth for a prompt input is exactly the kind of drift that is
/// expensive. Verified byte-identical before the switch.
fn default_prompt_template() -> String {
    orch_infra::template::default_prompt_template()
}

/// `.orchestry/.gitignore` before the daemon's block.
const GITIGNORE_LEGACY: &str = "\
# Runtime state
state.json
*.lock

# Logs and runs
runs/
logs/

# Agent workspaces
workspaces/
";

/// `.orchestry/.gitignore`: the legacy entries, then the daemon's runtime
/// files from the one list `orch daemon start` also ensures (spec R2), so a
/// fresh project needs no append.
fn gitignore() -> String {
    let mut content = format!("{GITIGNORE_LEGACY}\n{}\n", crate::RUNTIME_GITIGNORE_HEADER);
    for entry in crate::RUNTIME_GITIGNORE_ENTRIES {
        content.push_str(entry);
        content.push('\n');
    }
    content
}

/// `.orchestry/workspace-exclude` — the rsync exclude list for `isolated`
/// mode.
const WORKSPACE_EXCLUDE: &str = "\
.orchestry
node_modules
.env
.env.*
dist
build
.next
__pycache__
*.pyc
.venv
";

/// Options of `orch init`, the only project-creation path. Mirrors the CLI's
/// existing `InitArgs`.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub project_root: PathBuf,
    /// `--name`.
    pub name: Option<String>,
    /// `--adapter`.
    pub adapter: Option<String>,
    /// `--prompt-mode`; omitted writes `normal`.
    pub prompt_mode: Option<orch_domain::PromptMode>,
    /// Both stdout and stdin are a TTY (gates the multi-adapter prompt in the
    /// CLI; the daemon API itself never prompts).
    pub interactive: bool,
}

/// What [`initialize`] did, so the CLI can print the start hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// Re-run on an initialized project: nothing was overwritten.
    pub already_initialized: bool,
    pub adapter: String,
}

/// Init-only existence query; no stores or daemon are opened.
pub async fn is_initialized(project_root: &Path) -> bool {
    path_exists(&Paths::new(project_root).root()).await
}

/// Create the project before any daemon exists, without starting one.
pub async fn initialize(options: InitOptions) -> std::result::Result<InitOutcome, DaemonError> {
    initialize_project(options).await.map_err(DaemonError::from)
}

async fn initialize_project(options: InitOptions) -> Result<InitOutcome> {
    let project_root = options.project_root.as_path();
    let paths = Paths::new(project_root);
    if is_initialized(project_root).await {
        return Ok(InitOutcome {
            already_initialized: true,
            adapter: options.adapter.unwrap_or_else(|| "claude".to_owned()),
        });
    }
    let adapter = options
        .adapter
        .clone()
        .unwrap_or_else(|| "claude".to_owned());
    for dir in [
        paths.tasks_dir(),
        paths.agents_dir(),
        paths.goals_dir(),
        paths.runs_dir(),
        paths.templates_dir(),
        paths.logs_dir(),
    ] {
        ensure_dir(&dir).await?;
    }

    // Must happen before the config is written: without git, worktree mode is
    // unusable and the default falls back to `shared`.
    let git_available = ensure_git_repo(project_root).await;

    let mut config = default_config();
    config.project.name = options.name.clone().unwrap_or_else(|| {
        project_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "my-project".to_owned())
    });
    config.defaults.agent.adapter = adapter.clone();
    config.defaults.agent.prompt_mode = options.prompt_mode.unwrap_or_default();
    if !git_available {
        config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    }

    let agents = default_agents(&adapter);

    // Owner-only, like every later write of these two files: `config.yml`
    // carries adapter commands and API-key environment names, and an agent
    // file is the role prompt an AI CLI is run with (security review C3).
    // `init` had its own write path, so routing the stores through
    // `OWNER_ONLY` left the files `init` created at the umask's 0664 — the
    // mode of the file as *first written* is the one that matters, because
    // nothing rewrites them until they are edited.
    write_yaml_with_mode(&paths.config_path(), &config, owner_only()).await?;
    atomic_write(&paths.gitignore_path(), &gitignore()).await?;
    atomic_write(&paths.workspace_exclude_path(), WORKSPACE_EXCLUDE).await?;
    atomic_write(&paths.default_template_path(), &default_prompt_template()).await?;
    for agent in &agents {
        write_yaml_with_mode(&paths.agent_path(&agent.id)?, agent, owner_only()).await?;
    }

    // Keep `.orchestry` out of the repo so worktrees do not nest recursively.
    ensure_root_gitignore(project_root).await?;

    if git_available {
        ensure_git_commit(project_root).await;
    }

    Ok(InitOutcome {
        already_initialized: false,
        adapter,
    })
}

/// Make sure the project is a git repository, running `git init` when it is
/// not. Returns false only when the `git` binary is unavailable.
async fn ensure_git_repo(project_root: &Path) -> bool {
    if git(project_root, &["rev-parse", "--is-inside-work-tree"]).await {
        return true;
    }
    git(project_root, &["init"]).await
}

/// `git worktree add` needs at least one commit, so create one if the repo has
/// none. A failure here is non-fatal: it usually means `user.name` is unset.
async fn ensure_git_commit(project_root: &Path) {
    if git(project_root, &["rev-parse", "HEAD"]).await {
        return;
    }
    if git(project_root, &["add", "-A"]).await {
        let _ = git(
            project_root,
            &["commit", "-m", "Initial commit", "--allow-empty"],
        )
        .await;
    }
}

async fn git(cwd: &Path, args: &[&str]) -> bool {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// Append `.orchestry` to the project's root `.gitignore`, creating the file
/// when it does not exist.
async fn ensure_root_gitignore(project_root: &Path) -> Result<()> {
    let path = project_root.join(".gitignore");
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            if content.lines().any(|line| line.trim() == ".orchestry") {
                return Ok(());
            }
            let separator = if content.ends_with('\n') { "" } else { "\n" };
            let appended = format!("{content}{separator}\n# Orchestry state\n.orchestry\n");
            // A whole-file write keeps the atomic-write discipline.
            atomic_write(&path, &appended).await
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            atomic_write(&path, "# Orchestry state\n.orchestry\n").await
        }
        Err(err) => Err(Error::io(format!("read {}", path.display()), &path, err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_infra::storage::fs_utils::clear_ensured_dirs;
    async fn init(root: &Path, args: InitOptions) -> InitOutcome {
        clear_ensured_dirs();
        initialize(InitOptions {
            project_root: root.to_path_buf(),
            ..args
        })
        .await
        .unwrap()
    }

    fn shell_args() -> InitOptions {
        InitOptions {
            name: Some("proj".into()),
            adapter: Some("shell".into()),
            prompt_mode: None,
            interactive: false,
            project_root: Default::default(),
        }
    }

    #[tokio::test]
    async fn creates_exactly_the_documented_files() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path(), shell_args()).await;
        let paths = Paths::new(temp.path());

        // `init` creates only these.
        for path in [
            paths.config_path(),
            paths.gitignore_path(),
            paths.workspace_exclude_path(),
            paths.default_template_path(),
        ] {
            assert!(path_exists(&path).await, "missing {}", path.display());
        }
        // And deliberately no index files.
        assert!(!path_exists(&paths.tasks_dir().join("_index.json")).await);
        assert!(!path_exists(&paths.agents_dir().join("_index.json")).await);
        assert!(!path_exists(&paths.state_path()).await);

        // The directories exist but are empty.
        for dir in [
            paths.tasks_dir(),
            paths.goals_dir(),
            paths.runs_dir(),
            paths.logs_dir(),
        ] {
            assert!(path_exists(&dir).await, "missing {}", dir.display());
        }
    }

    #[tokio::test]
    async fn shell_gets_no_bootstrap_agent_and_other_adapters_do() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path(), shell_args()).await;
        let paths = Paths::new(temp.path());
        assert!(!path_exists(&paths.agent_path("agt_creator").unwrap()).await);

        let temp = tempfile::tempdir().unwrap();
        init(
            temp.path(),
            InitOptions {
                name: Some("p".into()),
                adapter: Some("claude".into()),
                prompt_mode: None,
                interactive: false,
                project_root: Default::default(),
            },
        )
        .await;
        let paths = Paths::new(temp.path());
        assert!(path_exists(&paths.agent_path("agt_creator").unwrap()).await);

        // Security review C3: the *first* write is the one that matters.
        // These two files are created by `init`'s own path, not by the stores,
        // so routing the stores through OWNER_ONLY was not enough — the review
        // found both at 0664 after a fresh init.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                paths.config_path(),
                paths.agent_path("agt_creator").unwrap(),
            ] {
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(
                    mode,
                    0o600,
                    "{} is {mode:o}, not owner-only",
                    path.display()
                );
            }
        }
    }

    #[tokio::test]
    async fn the_bootstrap_agent_file_keeps_its_role_byte_exact() {
        let temp = tempfile::tempdir().unwrap();
        init(
            temp.path(),
            InitOptions {
                name: Some("p".into()),
                adapter: Some("claude".into()),
                prompt_mode: None,
                interactive: false,
                project_root: Default::default(),
            },
        )
        .await;
        let paths = Paths::new(temp.path());

        let agent: orch_domain::Agent =
            orch_infra::storage::fs_utils::read_yaml(&paths.agent_path("agt_creator").unwrap())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(agent.role.as_deref(), Some(orch_domain::AGENT_CREATOR_ROLE));
        // 300000 here, not the 600000 project default.
        assert_eq!(agent.config.stall_timeout_ms, Some(300_000));
        assert_eq!(
            agent.config.model.as_deref(),
            Some(orch_domain::default_model_for_adapter("claude"))
        );
    }

    #[tokio::test]
    async fn the_config_records_the_chosen_adapter_and_project_name() {
        let temp = tempfile::tempdir().unwrap();
        init(
            temp.path(),
            InitOptions {
                name: Some("proj".into()),
                adapter: Some("codex".into()),
                prompt_mode: None,
                interactive: false,
                project_root: Default::default(),
            },
        )
        .await;

        let content = tokio::fs::read_to_string(Paths::new(temp.path()).config_path())
            .await
            .unwrap();
        assert!(content.contains("name: proj"), "{content}");
        assert!(content.contains("adapter: codex"), "{content}");
    }

    #[tokio::test]
    async fn the_config_records_the_prompt_mode_and_defaults_to_normal() {
        for (mode, expected) in [
            (None, "prompt_mode: normal"),
            (Some(orch_domain::PromptMode::Eco), "prompt_mode: eco"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            init(
                temp.path(),
                InitOptions {
                    prompt_mode: mode,
                    ..shell_args()
                },
            )
            .await;
            let content = tokio::fs::read_to_string(Paths::new(temp.path()).config_path())
                .await
                .unwrap();
            assert!(content.contains(expected), "{content}");
        }
    }

    #[tokio::test]
    async fn the_project_name_defaults_to_the_directory_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("my-dir");
        std::fs::create_dir_all(&root).unwrap();
        init(
            &root,
            InitOptions {
                name: None,
                adapter: Some("shell".into()),
                prompt_mode: None,
                interactive: false,
                project_root: Default::default(),
            },
        )
        .await;

        let content = tokio::fs::read_to_string(Paths::new(&root).config_path())
            .await
            .unwrap();
        assert!(content.contains("name: my-dir"), "{content}");
    }

    #[tokio::test]
    async fn the_static_files_are_byte_exact() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path(), shell_args()).await;
        let paths = Paths::new(temp.path());

        assert_eq!(
            tokio::fs::read_to_string(paths.gitignore_path())
                .await
                .unwrap(),
            "# Runtime state\nstate.json\n*.lock\n\n# Logs and runs\nruns/\nlogs/\n\n# Agent workspaces\nworkspaces/\n\n# Daemon runtime\nrun/\ndaemon.json\ndaemon.log\ndaemon.log.1\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(paths.workspace_exclude_path())
                .await
                .unwrap(),
            ".orchestry\nnode_modules\n.env\n.env.*\ndist\nbuild\n.next\n__pycache__\n*.pyc\n.venv\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(paths.default_template_path())
                .await
                .unwrap(),
            default_prompt_template()
        );
        // The prompt template is a prompt input, so its size is pinned.
        assert_eq!(default_prompt_template().len(), 5113);
    }

    #[tokio::test]
    async fn a_fresh_init_ignores_the_daemon_runtime_files() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path(), shell_args()).await;
        let content = tokio::fs::read_to_string(Paths::new(temp.path()).gitignore_path())
            .await
            .unwrap();
        for entry in ["run/", "daemon.json", "daemon.log", "daemon.log.1"] {
            assert_eq!(
                content.lines().filter(|line| *line == entry).count(),
                1,
                "{entry} in {content}"
            );
        }
        // Nothing for the daemon's first start to append.
        assert_eq!(crate::ensure_runtime_gitignore(temp.path()), Ok(0));
    }

    #[tokio::test]
    async fn re_running_init_warns_and_changes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path(), shell_args()).await;
        let before = tokio::fs::read_to_string(Paths::new(temp.path()).config_path())
            .await
            .unwrap();

        let outcome = init(
            temp.path(),
            InitOptions {
                name: Some("different".into()),
                adapter: Some("codex".into()),
                prompt_mode: None,
                interactive: false,
                project_root: Default::default(),
            },
        )
        .await;
        assert!(outcome.already_initialized);
        assert_eq!(
            tokio::fs::read_to_string(Paths::new(temp.path()).config_path())
                .await
                .unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn the_root_gitignore_gains_the_orchestry_entry_exactly_once() {
        let temp = tempfile::tempdir().unwrap();
        let gitignore = temp.path().join(".gitignore");
        tokio::fs::write(&gitignore, "target\n").await.unwrap();
        init(temp.path(), shell_args()).await;

        let content = tokio::fs::read_to_string(&gitignore).await.unwrap();
        assert_eq!(content, "target\n\n# Orchestry state\n.orchestry\n");

        // A second init is a no-op, so the entry is not duplicated.
        init(temp.path(), shell_args()).await;
        assert_eq!(content.matches(".orchestry").count(), 1);
    }

    #[tokio::test]
    async fn a_root_gitignore_without_a_trailing_newline_gets_one() {
        let temp = tempfile::tempdir().unwrap();
        let gitignore = temp.path().join(".gitignore");
        tokio::fs::write(&gitignore, "target").await.unwrap();
        clear_ensured_dirs();
        ensure_root_gitignore(temp.path()).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&gitignore).await.unwrap(),
            "target\n\n# Orchestry state\n.orchestry\n"
        );
    }

    #[tokio::test]
    async fn an_existing_orchestry_entry_is_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let gitignore = temp.path().join(".gitignore");
        tokio::fs::write(&gitignore, "a\n.orchestry\nb\n")
            .await
            .unwrap();
        clear_ensured_dirs();
        ensure_root_gitignore(temp.path()).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&gitignore).await.unwrap(),
            "a\n.orchestry\nb\n"
        );
    }

    #[tokio::test]
    async fn a_missing_root_gitignore_is_created() {
        let temp = tempfile::tempdir().unwrap();
        clear_ensured_dirs();
        ensure_root_gitignore(temp.path()).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join(".gitignore"))
                .await
                .unwrap(),
            "# Orchestry state\n.orchestry\n"
        );
    }
}
