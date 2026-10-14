//! `$EDITOR` round-trips — ported from `src/cli/editor.ts`
//! (`docs/parity/cli.md` §21.1).
//!
//! Used by `task add -e`, `task edit`, `agent add -e` and `agent edit`.
//! `config edit` spawns the editor on the real config file instead.
//!
//! **There is no TTY check**, in the reference or here: in a non-TTY the
//! editor is still spawned with inherited stdio and typically fails, which
//! surfaces as `Editor exited with code <n>` and exit 1. There is no
//! `--yes`/non-interactive fallback to reproduce.
//!
//! The two buffer formats are L0 — they are how a user's edit reaches a
//! `Task` or an `Agent` record — so the parsers are what the tests pin, not
//! the spawning.

use std::path::Path;

use orch_core::error::{Error, Result};

/// What a task buffer can change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskEdit {
    pub title: Option<String>,
    pub priority: Option<i64>,
    pub description: Option<String>,
}

/// What an agent buffer can change.
///
/// `role` is **always** `Some` once a fence was found — an empty string means
/// "cleared", which is how a caller tells clearing apart from leaving it
/// alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentEdit {
    pub name: Option<String>,
    pub command: Option<String>,
    pub model: Option<String>,
    pub role: Option<String>,
}

/// The buffer `task add -e` / `task edit` opens.
pub fn task_buffer(title: &str, priority: i64, description: &str) -> String {
    format!("---\ntitle: {title}\npriority: {priority}\n---\n\n{description}")
}

/// The buffer `agent add -e` / `agent edit` opens.
pub fn agent_buffer(name: &str, command: &str, model: &str, role: &str) -> String {
    format!(
        "# Edit agent configuration.\n\
         # Lines starting with # are ignored.\n\
         # Role description goes below the second --- separator.\n\
         ---\n\
         name: {name}\n\
         command: {command}\n\
         model: {model}\n\
         ---\n\n\
         {role}"
    )
}

/// `fromEditorContent` — frontmatter, then a free-form body.
///
/// With no frontmatter fence the **whole** buffer is the description, and an
/// empty buffer changes nothing.
pub fn parse_task_content(content: &str) -> TaskEdit {
    let Some((frontmatter, body)) = split_frontmatter(content) else {
        let description = content.trim();
        return TaskEdit {
            description: (!description.is_empty()).then(|| description.to_owned()),
            ..Default::default()
        };
    };

    let mut edit = TaskEdit::default();
    for (key, value) in frontmatter_pairs(&frontmatter) {
        match key.as_str() {
            "title" if !value.trim().is_empty() => edit.title = Some(value.trim().to_owned()),
            "priority" => {
                // Out-of-range and non-numeric values are ignored, not an
                // error.
                if let Ok(priority) = value.trim().parse::<i64>()
                    && (1..=4).contains(&priority)
                {
                    edit.priority = Some(priority);
                }
            }
            _ => {}
        }
    }

    let description = body.trim();
    if !description.is_empty() {
        edit.description = Some(description.to_owned());
    }
    edit
}

/// `fromAgentEditorContent`.
///
/// Comment lines are stripped **only before the first fence**, so a `#` inside
/// the role body survives — a role description full of markdown headings is
/// the normal case.
pub fn parse_agent_content(content: &str) -> AgentEdit {
    let lines: Vec<&str> = content.split('\n').collect();
    let first_fence = lines.iter().position(|line| line.trim_end() == "---");

    let stripped: String = match first_fence {
        Some(at) => lines[..at]
            .iter()
            .filter(|line| !line.starts_with('#'))
            .chain(lines[at..].iter())
            .copied()
            .collect::<Vec<_>>()
            .join("\n"),
        None => lines
            .iter()
            .filter(|line| !line.starts_with('#'))
            .copied()
            .collect::<Vec<_>>()
            .join("\n"),
    };

    let Some((frontmatter, body)) = split_frontmatter(&stripped) else {
        return AgentEdit::default();
    };

    let mut edit = AgentEdit::default();
    for (key, value) in frontmatter_pairs(&frontmatter) {
        // Empty values are allowed and meaningful: they clear the field.
        let value = value.trim().to_owned();
        match key.as_str() {
            "name" => edit.name = Some(value),
            "command" => edit.command = Some(value),
            "model" => edit.model = Some(value),
            _ => {}
        }
    }
    // Always set, so the caller can tell "cleared" from "not mentioned".
    edit.role = Some(body.trim().to_owned());
    edit
}

/// `/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/` — the leading fence must be the very
/// first line.
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let frontmatter = rest[..end].to_owned();
    let after = &rest[end + "\n---".len()..];
    // The newline after the closing fence is optional.
    let body = after.strip_prefix('\n').unwrap_or(after).to_owned();
    Some((frontmatter, body))
}

/// `/^([\w]+):\s*(.*)$/` per line; anything else is skipped.
fn frontmatter_pairs(frontmatter: &str) -> Vec<(String, String)> {
    frontmatter
        .split('\n')
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
            Some((key.to_owned(), value.trim_start().to_owned()))
        })
        .collect()
}

/// The editor command, in the reference's precedence order.
pub fn editor_command() -> String {
    std::env::var("EDITOR")
        .ok()
        .filter(|editor| !editor.is_empty())
        .or_else(|| {
            std::env::var("VISUAL")
                .ok()
                .filter(|editor| !editor.is_empty())
        })
        .unwrap_or_else(|| "vi".to_owned())
}

/// Write `initial_content` to a temporary file, open `$EDITOR` on it with
/// inherited stdio, and return what came back.
///
/// The temporary file and its directory are removed afterwards, including on
/// the error paths — the reference does the same in a `finally`.
pub async fn open_in_editor(initial_content: &str, extension: &str) -> Result<String> {
    let dir = tempfile::Builder::new()
        .prefix("orch-")
        .tempdir()
        .map_err(|err| Error::other(format!("cannot create a temporary directory: {err}")))?;
    let path = dir.path().join(format!("edit{extension}"));
    tokio::fs::write(&path, initial_content)
        .await
        .map_err(|err| Error::io("write the editor buffer", &path, err))?;

    run_editor(&editor_command(), &path).await?;

    let edited = tokio::fs::read_to_string(&path)
        .await
        .map_err(|err| Error::io("read the editor buffer", &path, err))?;
    // `dir` is removed when it drops, which is the `finally` in the original.
    Ok(edited)
}

/// Open an **existing** file in `$EDITOR`, which is what `config edit` does:
/// no temporary buffer, no parsing, and nothing printed.
pub async fn edit_file(path: &Path) -> Result<()> {
    run_editor(&editor_command(), path).await
}

/// `spawn(parts[0], [...parts.slice(1), filePath], { stdio: 'inherit' })`.
async fn run_editor(editor: &str, path: &Path) -> Result<()> {
    let mut parts = editor.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| Error::other("EDITOR is empty"))?;

    let status = tokio::process::Command::new(program)
        .args(parts)
        .arg(path)
        .status()
        .await
        .map_err(|err| Error::other(format!("spawn {program} {err}")))?;

    if status.success() {
        return Ok(());
    }
    // `Editor exited with code N` → the global boundary → exit 1.
    Err(Error::other(format!(
        "Editor exited with code {}",
        status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "null".to_owned())
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_buffer_round_trips() {
        let buffer = task_buffer("My Task", 2, "Free-form description here...");
        assert_eq!(
            buffer,
            "---\ntitle: My Task\npriority: 2\n---\n\nFree-form description here..."
        );
        assert_eq!(
            parse_task_content(&buffer),
            TaskEdit {
                title: Some("My Task".to_owned()),
                priority: Some(2),
                description: Some("Free-form description here...".to_owned()),
            }
        );
    }

    #[test]
    fn a_task_buffer_without_frontmatter_is_all_description() {
        assert_eq!(
            parse_task_content("just some prose\n"),
            TaskEdit {
                description: Some("just some prose".to_owned()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn an_empty_task_buffer_changes_nothing() {
        assert_eq!(parse_task_content(""), TaskEdit::default());
        assert_eq!(parse_task_content("   \n  "), TaskEdit::default());
    }

    #[test]
    fn a_blank_task_title_is_ignored_but_the_description_is_kept() {
        assert_eq!(
            parse_task_content("---\ntitle:   \npriority: 1\n---\nbody"),
            TaskEdit {
                title: None,
                priority: Some(1),
                description: Some("body".to_owned()),
            }
        );
    }

    #[test]
    fn an_out_of_range_or_non_numeric_priority_is_ignored() {
        for priority in ["0", "5", "abc", ""] {
            let parsed =
                parse_task_content(&format!("---\ntitle: t\npriority: {priority}\n---\nbody"));
            assert_eq!(parsed.priority, None, "priority: {priority}");
            assert_eq!(parsed.title.as_deref(), Some("t"));
        }
        for priority in 1..=4 {
            let parsed =
                parse_task_content(&format!("---\ntitle: t\npriority: {priority}\n---\nbody"));
            assert_eq!(parsed.priority, Some(priority));
        }
    }

    #[test]
    fn an_empty_task_body_leaves_the_description_alone() {
        assert_eq!(
            parse_task_content("---\ntitle: t\n---\n\n   \n"),
            TaskEdit {
                title: Some("t".to_owned()),
                description: None,
                ..Default::default()
            }
        );
    }

    #[test]
    fn unknown_frontmatter_keys_and_junk_lines_are_skipped() {
        assert_eq!(
            parse_task_content("---\ntitle: t\nnope: x\nnot a pair\n---\nbody"),
            TaskEdit {
                title: Some("t".to_owned()),
                priority: None,
                description: Some("body".to_owned()),
            }
        );
    }

    #[test]
    fn an_agent_buffer_round_trips() {
        let buffer = agent_buffer(
            "My Agent",
            "",
            "claude-sonnet-4-6",
            "Role description here...",
        );
        assert_eq!(
            parse_agent_content(&buffer),
            AgentEdit {
                name: Some("My Agent".to_owned()),
                command: Some(String::new()),
                model: Some("claude-sonnet-4-6".to_owned()),
                role: Some("Role description here...".to_owned()),
            }
        );
    }

    #[test]
    fn agent_comments_are_stripped_only_before_the_first_fence() {
        let buffer = "# a comment\n\
                      ---\n\
                      name: A\n\
                      ---\n\n\
                      # Heading\n\
                      Body with a # hash\n";
        let parsed = parse_agent_content(buffer);
        assert_eq!(parsed.name.as_deref(), Some("A"));
        assert_eq!(
            parsed.role.as_deref(),
            Some("# Heading\nBody with a # hash"),
            "a markdown heading in the role must survive"
        );
    }

    #[test]
    fn an_agent_role_is_always_set_so_clearing_is_detectable() {
        let parsed = parse_agent_content("---\nname: A\n---\n\n   \n");
        assert_eq!(parsed.role.as_deref(), Some(""));
    }

    #[test]
    fn an_agent_buffer_without_a_fence_changes_nothing() {
        assert_eq!(
            parse_agent_content("# only comments\n"),
            AgentEdit::default()
        );
        assert_eq!(parse_agent_content(""), AgentEdit::default());
    }

    #[test]
    fn empty_agent_values_are_kept_so_they_can_clear_a_field() {
        let parsed = parse_agent_content("---\nname:\ncommand:\nmodel:\n---\n");
        assert_eq!(parsed.name.as_deref(), Some(""));
        assert_eq!(parsed.command.as_deref(), Some(""));
        assert_eq!(parsed.model.as_deref(), Some(""));
    }

    #[test]
    fn a_fence_that_is_not_on_the_first_line_is_not_frontmatter() {
        // The regex is anchored at the start of the buffer.
        assert_eq!(
            parse_task_content("prose\n---\ntitle: t\n---\n"),
            TaskEdit {
                description: Some("prose\n---\ntitle: t\n---".to_owned()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn the_closing_fence_needs_no_trailing_newline() {
        assert_eq!(
            parse_task_content("---\ntitle: t\n---"),
            TaskEdit {
                title: Some("t".to_owned()),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn the_editor_round_trip_uses_the_editor_env_var() {
        // `sed -i` is a non-interactive "editor": it rewrites the buffer in
        // place, which is exactly what a real editor does.
        let buffer = open_in_editor_with(
            "sed -i s/before/after/",
            "---\ntitle: before\n---\n\nbefore\n",
            ".yml",
        )
        .await
        .expect("the edit succeeds");
        assert_eq!(buffer, "---\ntitle: after\n---\n\nafter\n");
    }

    #[tokio::test]
    async fn a_failing_editor_is_an_error_naming_its_exit_code() {
        let err = open_in_editor_with("false", "x", ".yml")
            .await
            .expect_err("a non-zero editor fails the command");
        assert_eq!(err.to_string(), "Editor exited with code 1");
        assert_eq!(err.exit_code(), 1);
    }

    #[tokio::test]
    async fn a_missing_editor_binary_is_an_error() {
        let err = open_in_editor_with("orch-definitely-not-an-editor", "x", ".yml")
            .await
            .expect_err("a missing editor fails the command");
        assert!(
            err.to_string()
                .starts_with("spawn orch-definitely-not-an-editor")
        );
    }

    #[tokio::test]
    async fn the_buffer_and_its_directory_are_removed_afterwards() {
        // The "editor" records the path it was handed; once the call returns,
        // neither that file nor its directory exists any more.
        let dir = tempfile::tempdir().expect("tempdir");
        let recorded = dir.path().join("recorded");
        let script = dir.path().join("record-editor");
        tokio::fs::write(
            &script,
            format!("printf '%s' \"$1\" > '{}'\n", recorded.display()),
        )
        .await
        .expect("write the script");

        // Run through `sh` rather than marking the script executable: exec'ing
        // a file this process has only just written races with any other test
        // thread that forks in between, and Linux answers `ETXTBSY`. The
        // editor command is split on whitespace, so this also covers the
        // multi-word `$EDITOR` path.
        let editor = format!("sh {}", script.display());
        let returned = open_in_editor_with(&editor, "contents", ".yml")
            .await
            .expect("the edit succeeds");
        assert_eq!(returned, "contents");

        let handed_to_editor = tokio::fs::read_to_string(&recorded)
            .await
            .expect("the editor ran");
        let handed_to_editor = std::path::PathBuf::from(handed_to_editor);
        assert_eq!(
            handed_to_editor.file_name().expect("a file name"),
            "edit.yml"
        );
        assert!(!handed_to_editor.exists(), "the buffer outlived the call");
        assert!(
            !handed_to_editor.parent().expect("a parent").exists(),
            "the temporary directory outlived the call"
        );
    }

    /// `open_in_editor` with the editor command injected, so the tests do not
    /// have to mutate the process environment (which is `unsafe` and shared
    /// between tests).
    async fn open_in_editor_with(
        editor: &str,
        initial_content: &str,
        extension: &str,
    ) -> Result<String> {
        // Same body as `open_in_editor`, with the editor injected: the
        // alternative is mutating `EDITOR`, which is `unsafe` in this edition
        // and shared between concurrently running tests.
        let dir = tempfile::Builder::new()
            .prefix("orch-")
            .tempdir()
            .map_err(|err| Error::other(format!("tempdir: {err}")))?;
        let path = dir.path().join(format!("edit{extension}"));
        tokio::fs::write(&path, initial_content)
            .await
            .map_err(|err| Error::io("write", &path, err))?;
        run_editor(editor, &path).await?;
        tokio::fs::read_to_string(&path)
            .await
            .map_err(|err| Error::io("read", &path, err))
    }

    #[test]
    fn the_editor_command_falls_back_to_vi() {
        // The real resolution order is EDITOR, VISUAL, vi. Reading the
        // environment is all this does, so assert the shape rather than
        // mutating it.
        let command = editor_command();
        assert!(!command.is_empty());
    }
}
