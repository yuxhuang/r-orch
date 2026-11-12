//! `orch init`.
//!
//! Creates the `.orchestry/` scaffold. The set of files it writes is contract:
//! `config.yml`, `agents/agt_creator.yml`,
//! `templates/default.md`, `.gitignore`, `workspace-exclude` — and
//! **nothing else**. `tasks/`, `runs/`, `logs/` and every `_index.json` are
//! created lazily on first write, which is why a first `task list` on a fresh
//! project rebuilds an index from an empty directory.
//!
//! The directories themselves *are* created (`tasks/`, `agents/`, `goals/`,
//! `runs/`, `templates/`, `logs/`), they are just empty.

use std::path::Path;
use std::time::Duration;

use crate::output::{Styles, print_success, print_warning};
use orch_core::error::{Error, Result};
use orch_domain::{SUPPORTED_ADAPTERS, default_agents};

#[derive(Debug, Clone, Default)]
pub struct InitArgs {
    pub name: Option<String>,
    pub adapter: Option<String>,
    /// `--prompt-mode`. Omitted writes `normal`; an already initialized
    /// project is left untouched, as with every other init flag.
    pub prompt_mode: Option<orch_domain::PromptMode>,
    /// Both stdout and stdin are a TTY, which is what gates the multi-adapter
    /// prompt. Scripted runs have stdin on `/dev/null`, so they never take that
    /// branch.
    pub interactive: bool,
}

pub use orch_daemon::InitOutcome;

pub async fn run(styles: &Styles, project_root: &Path, args: &InitArgs) -> Result<InitOutcome> {
    if orch_daemon::is_initialized(project_root).await {
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

    let outcome = orch_daemon::initialize(orch_daemon::InitOptions {
        project_root: project_root.to_path_buf(),
        name: args.name.clone(),
        adapter: Some(adapter.clone()),
        prompt_mode: args.prompt_mode,
        interactive: args.interactive,
    })
    .await
    .map_err(|error| match error {
        orch_daemon::DaemonError::Core(error) => error,
        error => Error::other(error.to_string()),
    })?;
    if outcome.already_initialized {
        print_warning(styles, "Already initialized");
        return Ok(outcome);
    }
    let agents = default_agents(&adapter);
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
    println!(
        "  Prompt mode: {} {}",
        args.prompt_mode.unwrap_or_default(),
        styles.dim("(change with `orch config set defaults.agent.prompt_mode eco|normal`)")
    );
    println!();

    Ok(InitOutcome {
        already_initialized: false,
        adapter,
    })
}

/// Initialization never starts the daemon or suggests a stateful command
/// before the owner starts one. Reinitialization prints the same hint.
pub fn next_hint(_adapter: Option<&str>) -> &'static str {
    "orch daemon start"
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn init_only_suggests_an_explicit_owner_start() {
        for adapter in [None, Some("shell"), Some("claude")] {
            assert_eq!(next_hint(adapter), "orch daemon start");
        }
    }
}
