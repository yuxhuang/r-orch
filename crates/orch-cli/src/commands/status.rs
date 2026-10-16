//! `orch status` — ported from `src/cli/commands/status.ts`
//! (`docs/parity/cli.md` §6).
//!
//! The one place where r-orch deliberately differs from the oracle's `--json`:
//! **`state.claimed` is emitted as an array** of claimed task ids, the same
//! shape `state.json` holds on disk. The reference serializes its in-memory
//! JS `Set`, which `JSON.stringify` renders as `{}` for any contents. That is
//! PARITY.md §14.1, decided as a **fix** by the user; the harness masks the
//! field in `status --json` comparisons and asserts our value against
//! `state.json` instead.
//!
//! Everything else is the reference's: the un-indented header line, the status
//! counts in first-seen order over the task list, `RUNNING` counted from
//! `state.running` rather than from the tasks, and no `--quiet` handling.

use indexmap::IndexMap;
use orch_core::error::Result;
use orch_core::storage::TaskFilter;
use orch_domain::{Agent, OrchestratorState, Task, TaskStatus};
use orch_engine::Engine;
use serde::Serialize;

use crate::context::CliContext;
use crate::output::{Color, Styles, format_duration_since, format_tokens, print_json};

/// `{ tasks, agents, state }`, with `claimed` as an array.
///
/// A dedicated struct rather than an ad-hoc `json!`, so the field order is
/// the reference's and the `claimed` shape is visible in one place.
#[derive(Debug, Serialize)]
struct StatusJson<'a> {
    tasks: &'a [Task],
    agents: &'a [Agent],
    state: &'a OrchestratorState,
}

pub async fn run(engine: &Engine, ctx: &CliContext, styles: &Styles) -> Result<()> {
    let tasks = engine.task_service.list(&TaskFilter::default()).await?;
    let agents = engine.agent_service.list().await?;
    let state = engine.state.read().await?;

    if ctx.json {
        // `OrchestratorState` serializes `claimed` as the `IndexSet` it is,
        // i.e. a JSON array — §14.1's fix, and identical to `state.json`.
        return print_json(&StatusJson {
            tasks: &tasks,
            agents: &agents,
            state: &state,
        });
    }

    print!(
        "{}",
        render(styles, &engine.config.project.name, &tasks, &agents, &state)
    );
    Ok(())
}

/// The human block, as one string so the layout is assertable.
fn render(
    styles: &Styles,
    project: &str,
    tasks: &[Task],
    agents: &[Agent],
    state: &OrchestratorState,
) -> String {
    let running_count = state.running.len();
    let mode = if state.pid.is_some() {
        "watching"
    } else {
        "idle"
    };

    let mut out = String::from("\n");
    // No leading indent on this line, unlike every other line of output.
    out.push_str(&format!(
        "{} · {project} · {mode}\n",
        styles.paint(Color::Amber, "orch")
    ));
    out.push('\n');

    if running_count > 0 {
        // `''.padEnd(20)` is twenty spaces.
        out.push_str(&format!(
            "  {:<12}{running_count}{}AGENTS  {}\n",
            "RUNNING",
            " ".repeat(20),
            agents.len()
        ));
    }
    for (status, count) in status_counts(tasks) {
        if status != TaskStatus::InProgress {
            out.push_str(&format!(
                "  {}{count}\n",
                styles.dim(&format!("{:<12}", status.as_str()))
            ));
        }
    }

    let running: Vec<&Task> = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::InProgress)
        .collect();
    if !running.is_empty() {
        out.push('\n');
        for task in running {
            let assignee = task
                .assignee
                .as_deref()
                .map(|name| styles.agent_name(name))
                .unwrap_or_default();
            let title: String = task.title.chars().take(35).collect();
            out.push_str(&format!(
                "  {} {assignee}  {:<37}{}  {}\n",
                styles.status_icon("in_progress"),
                title,
                format_duration_since(&task.updated_at),
                styles.priority_label(task.priority),
            ));
        }
    }

    if let Some(footer) = footer(state) {
        out.push('\n');
        out.push_str(&format!("  {}\n", styles.dim(&footer)));
    }
    out.push('\n');
    out
}

/// Status → count, in **first-seen order over the task list**, which is what
/// `Object.entries` gives for a plain object built by iteration.
fn status_counts(tasks: &[Task]) -> IndexMap<TaskStatus, usize> {
    let mut counts = IndexMap::new();
    for task in tasks {
        *counts.entry(task.status).or_insert(0) += 1;
    }
    counts
}

/// `up <uptime> · ↑in ↓out 🧠reasoning Σtotal`, each half omitted when empty,
/// and the whole line omitted when both are.
fn footer(state: &OrchestratorState) -> Option<String> {
    let tokens = &state.stats.total_tokens;
    let mut parts: Vec<String> = Vec::new();
    if tokens.total > 0 {
        parts.push(format!("↑{}", format_tokens(tokens.input)));
        parts.push(format!("↓{}", format_tokens(tokens.output)));
        if tokens.reasoning > 0 {
            parts.push(format!("\u{1f9e0}{}", format_tokens(tokens.reasoning)));
        }
        parts.push(format!("Σ{}", format_tokens(tokens.total)));
    }

    let mut halves: Vec<String> = Vec::new();
    if let Some(started_at) = &state.started_at {
        halves.push(format!("up {}", format_duration_since(started_at)));
    }
    if !parts.is_empty() {
        halves.push(parts.join(" "));
    }
    (!halves.is_empty()).then(|| halves.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{make_agent, make_task};
    use orch_domain::{TokenUsage, default_state};

    fn task(status: TaskStatus, title: &str) -> Task {
        Task {
            status,
            title: title.to_owned(),
            ..make_task()
        }
    }

    #[test]
    fn claimed_is_emitted_as_an_array_not_an_empty_object() {
        // PARITY.md §14.1, the fix: `status --json` must carry the claimed
        // task ids, which the reference loses by stringifying a JS `Set`.
        let mut state = default_state();
        state.claimed.insert("tsk_AAAAAAA".to_owned());
        state.claimed.insert("tsk_BBBBBBB".to_owned());

        let json = serde_json::to_value(StatusJson {
            tasks: &[],
            agents: &[],
            state: &state,
        })
        .expect("serialize");

        assert_eq!(
            json["state"]["claimed"],
            serde_json::json!(["tsk_AAAAAAA", "tsk_BBBBBBB"]),
            "claimed must be the array of ids, in insertion order"
        );
        assert!(
            json["state"]["claimed"].is_array(),
            "not an object: {}",
            json["state"]["claimed"]
        );
    }

    #[test]
    fn an_empty_claimed_set_is_an_empty_array() {
        let json = serde_json::to_value(StatusJson {
            tasks: &[],
            agents: &[],
            state: &default_state(),
        })
        .expect("serialize");
        assert_eq!(json["state"]["claimed"], serde_json::json!([]));
    }

    #[test]
    fn the_claimed_array_matches_what_state_json_would_hold() {
        // The harness asserts exactly this equality in place of the masked
        // comparison, so it is worth pinning here too: both sides serialize
        // the same `OrchestratorState`.
        let mut state = default_state();
        state.claimed.insert("tsk_AAAAAAA".to_owned());

        let on_disk = serde_json::to_value(&state).expect("serialize state");
        let in_status = serde_json::to_value(StatusJson {
            tasks: &[],
            agents: &[],
            state: &state,
        })
        .expect("serialize status");

        assert_eq!(in_status["state"]["claimed"], on_disk["claimed"]);
    }

    #[test]
    fn the_json_has_exactly_three_keys_in_order() {
        let text = serde_json::to_string(&StatusJson {
            tasks: &[],
            agents: &[],
            state: &default_state(),
        })
        .expect("serialize");
        assert!(
            text.starts_with(r#"{"tasks":[],"agents":[],"state":{"#),
            "{text}"
        );
    }

    #[test]
    fn the_header_has_no_indent_and_reports_idle_without_a_pid() {
        let rendered = render(
            &Styles::plain(),
            "op2",
            &[task(TaskStatus::Todo, "a"), task(TaskStatus::Todo, "b")],
            &[],
            &default_state(),
        );
        assert_eq!(rendered, "\norch · op2 · idle\n\n  todo        2\n\n");
    }

    #[test]
    fn a_pid_switches_the_mode_to_watching() {
        let state = OrchestratorState {
            pid: Some(4242),
            ..default_state()
        };
        let rendered = render(&Styles::plain(), "op2", &[], &[], &state);
        assert!(rendered.starts_with("\norch · op2 · watching\n"));
    }

    #[test]
    fn status_counts_are_in_first_seen_order_and_skip_in_progress() {
        let tasks = [
            task(TaskStatus::Review, "r"),
            task(TaskStatus::Todo, "t"),
            task(TaskStatus::InProgress, "p"),
            task(TaskStatus::Todo, "t2"),
        ];
        let counts = status_counts(&tasks);
        assert_eq!(
            counts.keys().copied().collect::<Vec<_>>(),
            [TaskStatus::Review, TaskStatus::Todo, TaskStatus::InProgress]
        );

        let rendered = render(&Styles::plain(), "p", &tasks, &[], &default_state());
        let lines: Vec<&str> = rendered.lines().collect();
        // review first, then todo; in_progress is counted but not listed.
        assert_eq!(lines[3], "  review      1");
        assert_eq!(lines[4], "  todo        2");
        assert!(!rendered.contains("  in_progress"));
    }

    #[test]
    fn the_running_line_counts_state_not_tasks_and_pads_to_twenty() {
        let mut state = default_state();
        state.running.insert(
            "run_1".to_owned(),
            orch_domain::RunningEntry {
                remote: None,
                run_id: "run_1".to_owned(),
                agent_id: "agt_1".to_owned(),
                task_id: "tsk_1".to_owned(),
                pid: 1,
                started_at: "2026-10-13T16:00:00.000Z".to_owned(),
                last_event_at: "2026-10-13T16:00:00.000Z".to_owned(),
            },
        );

        let rendered = render(
            &Styles::plain(),
            "p",
            &[task(TaskStatus::Todo, "t")],
            &[make_agent(), make_agent()],
            &state,
        );
        let line = rendered
            .lines()
            .find(|line| line.contains("RUNNING"))
            .expect("the running line");
        assert_eq!(line, &format!("  RUNNING     1{}AGENTS  2", " ".repeat(20)));
    }

    #[test]
    fn an_in_progress_task_gets_its_own_line_with_the_title_padded_to_37() {
        let tasks = [Task {
            status: TaskStatus::InProgress,
            title: "Implement the thing".to_owned(),
            assignee: Some("worker".to_owned()),
            priority: 1,
            ..make_task()
        }];
        let rendered = render(&Styles::plain(), "p", &tasks, &[], &default_state());
        let line = rendered
            .lines()
            .find(|line| line.contains("Implement"))
            .expect("the task line");
        assert!(
            line.starts_with("  ● worker  Implement the thing  "),
            "{line}"
        );
        assert!(line.ends_with("P1"), "{line}");
    }

    #[test]
    fn a_long_running_title_is_truncated_to_35() {
        let tasks = [Task {
            status: TaskStatus::InProgress,
            title: "x".repeat(50),
            ..make_task()
        }];
        let rendered = render(&Styles::plain(), "p", &tasks, &[], &default_state());
        assert!(rendered.contains(&"x".repeat(35)));
        assert!(!rendered.contains(&"x".repeat(36)));
    }

    #[test]
    fn the_footer_appears_only_when_there_is_something_to_report() {
        assert_eq!(footer(&default_state()), None);

        let mut state = default_state();
        state.stats.total_tokens = TokenUsage {
            input: 1200,
            output: 300,
            reasoning: 0,
            total: 1500,
            cache_read: 0,
            cache_write: 0,
        };
        assert_eq!(footer(&state).as_deref(), Some("↑1.2k ↓300 Σ1.5k"));

        // Reasoning adds a third part only when non-zero.
        state.stats.total_tokens.reasoning = 50;
        state.stats.total_tokens.total = 1550;
        assert_eq!(
            footer(&state).as_deref(),
            Some("↑1.2k ↓300 \u{1f9e0}50 Σ1.6k")
        );
    }

    #[test]
    fn the_uptime_half_is_joined_with_a_middot() {
        let state = OrchestratorState {
            started_at: Some(jiff::Timestamp::now().to_string()),
            ..default_state()
        };
        assert_eq!(footer(&state).as_deref(), Some("up 0s"));
    }
}
