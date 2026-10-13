//! `orch init` — ported from `src/cli/commands/init.ts`.
//!
//! Creates the `.orchestry/` scaffold. The set of files it writes is L0
//! (PARITY.md §6.11): `config.yml`, `agents/agt_creator.yml`,
//! `templates/default.md`, `.gitignore`, `workspace-exclude` — and
//! **nothing else**. `tasks/`, `runs/`, `logs/` and every `_index.json` are
//! created lazily on first write, which is why a first `task list` on a fresh
//! project rebuilds an index from an empty directory.
//!
//! The directories themselves *are* created (`tasks/`, `agents/`, `goals/`,
//! `runs/`, `templates/`, `logs/`), they are just empty.

use std::path::Path;
use std::time::Duration;

use orch_core::error::{Error, Result};
use orch_domain::{SUPPORTED_ADAPTERS, WorkspaceMode, default_agents, default_config};
use orch_infra::storage::Paths;
use orch_infra::storage::fs_utils::{atomic_write, ensure_dir, path_exists, write_yaml};

use crate::output::{Styles, print_success, print_warning};

/// The combined default prompt template, from WP-3's
/// `orch_infra::template`.
///
/// WP-0b carried its own byte-exact copy while WP-3 was building in parallel;
/// that copy is gone now the real module has landed, because two sources of
/// truth for a prompt input is exactly the kind of drift PARITY.md §9.1 makes
/// expensive. Verified byte-identical before the switch.
fn default_prompt_template() -> String {
    orch_infra::template::default_prompt_template()
}

/// `.orchestry/.gitignore` (PARITY.md §6.11).
const GITIGNORE: &str = "\
# Runtime state
state.json
*.lock

# Logs and runs
runs/
logs/

# Agent workspaces
workspaces/
";

/// `.orchestry/workspace-exclude` — the rsync exclude list for `isolated`
/// mode (PARITY.md §6.11).
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

#[derive(Debug, Clone, Default)]
pub struct InitArgs {
    pub name: Option<String>,
    pub adapter: Option<String>,
    /// Both stdout and stdin are a TTY, which is what gates the multi-adapter
    /// prompt (`docs/parity/cli.md` §15.4). The parity harness runs every
    /// scenario with stdin on `/dev/null`, so it never takes that branch.
    pub interactive: bool,
}

/// What `init` did, so the caller can print the `Next:` line without
/// re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    pub already_initialized: bool,
    pub adapter: String,
}

pub async fn run(styles: &Styles, project_root: &Path, args: &InitArgs) -> Result<InitOutcome> {
    let paths = Paths::new(project_root);

    if path_exists(&paths.root()).await {
        print_warning(styles, "Already initialized");
        return Ok(InitOutcome {
            already_initialized: true,
            adapter: args.adapter.clone().unwrap_or_else(|| "claude".to_owned()),
        });
    }

    let adapter = match &args.adapter {
        Some(adapter) => adapter.clone(),
        None => detect_adapter(styles, args.interactive).await,
    };

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
    config.project.name = args.name.clone().unwrap_or_else(|| {
        project_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "my-project".to_owned())
    });
    config.defaults.agent.adapter = adapter.clone();
    if !git_available {
        config.defaults.agent.workspace_mode = WorkspaceMode::Shared;
    }

    let agents = default_agents(&adapter);

    write_yaml(&paths.config_path(), &config).await?;
    atomic_write(&paths.gitignore_path(), GITIGNORE).await?;
    atomic_write(&paths.workspace_exclude_path(), WORKSPACE_EXCLUDE).await?;
    atomic_write(&paths.default_template_path(), &default_prompt_template()).await?;
    for agent in &agents {
        write_yaml(&paths.agent_path(&agent.id)?, agent).await?;
    }

    // Keep `.orchestry` out of the repo so worktrees do not nest recursively.
    ensure_root_gitignore(project_root).await?;

    if git_available {
        ensure_git_commit(project_root).await;
    }

    println!();
    print_success(styles, "initialized");
    println!();
    println!("  Created {}", styles.dim(".orchestry/"));
    println!("  {} config.yml", styles.dim("├──"));
    println!("  {} tasks/", styles.dim("├──"));
    println!("  {} agents/", styles.dim("├──"));
    for agent in &agents {
        println!(
            "  {} {}.yml {}",
            styles.dim("│   └──"),
            agent.id,
            styles.dim(&format!("({})", agent.name))
        );
    }
    println!("  {} templates/default.md", styles.dim("├──"));
    println!("  {} .gitignore", styles.dim("└──"));
    println!();

    Ok(InitOutcome {
        already_initialized: false,
        adapter,
    })
}

/// The `Next:` hint, keyed on the **`--adapter` flag** rather than the adapter
/// `init` settled on, and printed even when the project was already
/// initialized — both as the original does (`init.ts:264-268`, outside
/// `runInit`).
pub fn next_hint(adapter: Option<&str>) -> &'static str {
    if adapter == Some("shell") {
        "orch agent add \"Test Runner\" --adapter shell --command \"npm test\""
    } else {
        "orch task add \"Create backend agent\" --assignee agt_creator"
    }
}

/// Probe each adapter's `--version` and pick one.
///
/// Four outcomes: none detected defaults to claude with a note, one is taken
/// with a note, several are taken silently when not interactive, and several
/// on a TTY prompt.
async fn detect_adapter(styles: &Styles, interactive: bool) -> String {
    let mut available: Vec<(String, Option<String>)> = Vec::new();

    for adapter in SUPPORTED_ADAPTERS {
        if adapter.as_str() == "shell" {
            continue;
        }
        // Only `cursor-agent` is probed, never bare `agent` — too generic, and
        // it causes false positives.
        let candidates: &[&str] = match adapter.as_str() {
            "cursor" => &["cursor-agent"],
            "antigravity" => &["agy"],
            // Every other adapter is probed under its own name.
            _ => &[],
        };
        let own_name = [adapter.as_str()];
        let candidates = if candidates.is_empty() {
            &own_name[..]
        } else {
            candidates
        };
        for command in candidates {
            if let Some(version) = probe_version(command).await {
                available.push((adapter.as_str().to_owned(), version));
                break;
            }
        }
    }

    match available.len() {
        0 => {
            println!(
                "  {}",
                styles.dim("No AI adapters detected — defaulting to claude")
            );
            "claude".to_owned()
        }
        1 => {
            let (name, version) = &available[0];
            let suffix = version
                .as_ref()
                .map(|v| format!("({v})"))
                .unwrap_or_default();
            println!(
                "  {} {}",
                styles.dim(&format!("Detected: {name}")),
                styles.dim(&suffix)
            );
            name.clone()
        }
        _ if !interactive => available[0].0.clone(),
        _ => {
            print!("{}", render_adapter_menu(&available));
            use std::io::Write;
            let _ = std::io::stdout().flush();

            let mut answer = String::new();
            // A closed stdin reads nothing, which falls back to the first
            // adapter — the same place an out-of-range answer lands.
            let _ = std::io::stdin().read_line(&mut answer);
            let chosen = parse_adapter_choice(&answer, available.len());
            available[chosen].0.clone()
        }
    }
}

/// The menu and its prompt, as one block so the layout is assertable.
fn render_adapter_menu(available: &[(String, Option<String>)]) -> String {
    let mut out = String::from("\n  Available adapters:\n");
    for (index, (name, version)) in available.iter().enumerate() {
        match version {
            Some(version) => out.push_str(&format!("    {}) {name} {version}\n", index + 1)),
            None => out.push_str(&format!("    {}) {name} \n", index + 1)),
        }
    }
    out.push_str(&format!(
        "\n  Choose default adapter [1-{}]: ",
        available.len()
    ));
    out
}

/// A 1-based answer, where anything out of range or non-numeric falls back to
/// the **first** adapter.
fn parse_adapter_choice(answer: &str, len: usize) -> usize {
    answer
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|choice| *choice >= 1 && *choice <= len)
        .map_or(0, |choice| choice - 1)
}

/// `<command> --version`, 5 s timeout, first line of stdout trimmed.
async fn probe_version(command: &str) -> Option<Option<String>> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.arg("--version").stdin(std::process::Stdio::null());
    let output = tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(stdout.trim().lines().next().map(str::to_owned))
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
            // `fs.appendFile` in the original; a whole-file write is equivalent
            // and keeps the atomic-write discipline.
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

    #[test]
    fn the_adapter_menu_numbers_from_one_and_prompts_with_the_range() {
        let available = vec![
            (
                "claude".to_owned(),
                Some("2.1.296 (Claude Code)".to_owned()),
            ),
            ("codex".to_owned(), Some("codex-cli 0.162.1".to_owned())),
        ];
        assert_eq!(
            render_adapter_menu(&available),
            "\n  Available adapters:\n\
             \x20   1) claude 2.1.296 (Claude Code)\n\
             \x20   2) codex codex-cli 0.162.1\n\
             \n  Choose default adapter [1-2]: "
        );
    }

    #[test]
    fn an_adapter_without_a_version_still_gets_a_line() {
        let available = vec![("grok".to_owned(), None)];
        assert!(render_adapter_menu(&available).contains("    1) grok \n"));
    }

    #[test]
    fn an_out_of_range_or_junk_choice_falls_back_to_the_first_adapter() {
        assert_eq!(parse_adapter_choice("1\n", 2), 0);
        assert_eq!(parse_adapter_choice("2\n", 2), 1);
        assert_eq!(parse_adapter_choice(" 2 ", 2), 1);
        for junk in ["", "\n", "0", "3", "-1", "abc", "1.5", "two"] {
            assert_eq!(parse_adapter_choice(junk, 2), 0, "{junk:?}");
        }
    }

    async fn init(root: &Path, args: InitArgs) -> InitOutcome {
        clear_ensured_dirs();
        run(&Styles::plain(), root, &args).await.unwrap()
    }

    fn shell_args() -> InitArgs {
        InitArgs {
            name: Some("proj".into()),
            adapter: Some("shell".into()),
            interactive: false,
        }
    }

    #[tokio::test]
    async fn creates_exactly_the_documented_files() {
        let temp = tempfile::tempdir().unwrap();
        init(temp.path(), shell_args()).await;
        let paths = Paths::new(temp.path());

        // PARITY.md §6.11 — `init` creates only these.
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
            InitArgs {
                name: Some("p".into()),
                adapter: Some("claude".into()),
                interactive: false,
            },
        )
        .await;
        let paths = Paths::new(temp.path());
        assert!(path_exists(&paths.agent_path("agt_creator").unwrap()).await);
    }

    #[tokio::test]
    async fn the_bootstrap_agent_file_keeps_its_role_byte_exact() {
        let temp = tempfile::tempdir().unwrap();
        init(
            temp.path(),
            InitArgs {
                name: Some("p".into()),
                adapter: Some("claude".into()),
                interactive: false,
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
        // §6.11: 300000 here, not the 600000 project default.
        assert_eq!(agent.config.stall_timeout_ms, Some(300_000));
        assert_eq!(agent.config.model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[tokio::test]
    async fn the_config_records_the_chosen_adapter_and_project_name() {
        let temp = tempfile::tempdir().unwrap();
        init(
            temp.path(),
            InitArgs {
                name: Some("proj".into()),
                adapter: Some("codex".into()),
                interactive: false,
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
    async fn the_project_name_defaults_to_the_directory_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("my-dir");
        std::fs::create_dir_all(&root).unwrap();
        init(
            &root,
            InitArgs {
                name: None,
                adapter: Some("shell".into()),
                interactive: false,
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
            "# Runtime state\nstate.json\n*.lock\n\n# Logs and runs\nruns/\nlogs/\n\n# Agent workspaces\nworkspaces/\n"
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
        assert_eq!(default_prompt_template().len(), 4822);
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
            InitArgs {
                name: Some("different".into()),
                adapter: Some("codex".into()),
                interactive: false,
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

    #[test]
    fn the_next_hint_depends_on_the_adapter() {
        assert_eq!(
            next_hint(Some("shell")),
            "orch agent add \"Test Runner\" --adapter shell --command \"npm test\""
        );
        assert_eq!(
            next_hint(Some("claude")),
            "orch task add \"Create backend agent\" --assignee agt_creator"
        );
        // No `--adapter` at all takes the task hint, whatever `init`
        // auto-detected: the original reads the flag, not the outcome.
        assert_eq!(
            next_hint(None),
            "orch task add \"Create backend agent\" --assignee agt_creator"
        );
    }
}
