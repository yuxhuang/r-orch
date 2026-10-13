//! `orch task` — ported from `src/cli/commands/task.ts`.
//!
//! WP-0b covers `add` and `list`. The remaining subcommands (`show`, `edit`,
//! `assign`, `cancel`, `approve`, `reject`, `retry`, `delete`) are WP-5's.

use orch_core::error::Result;
use orch_core::storage::TaskFilter;
use orch_domain::{CreateTaskInput, Task, TaskStatus};
use orch_engine::Engine;

use crate::context::CliContext;
use crate::output::{Styles, print_json, print_success, print_table};

/// Comma-separated list option: split, trim, keep empties out of the way the
/// TS does **not** — `"a,,b".split(',').map(trim)` yields `["a", "", "b"]`, so
/// an empty element survives. Replicated.
fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|part| part.trim().to_owned())
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct TaskAddArgs {
    pub title: String,
    pub description: Option<String>,
    pub priority: i64,
    pub labels: Option<String>,
    pub depends_on: Option<String>,
    pub max_attempts: Option<u32>,
    pub workspace_mode: Option<orch_domain::WorkspaceMode>,
    pub assignee: Option<String>,
    pub review_criteria: Option<String>,
    pub scope: Option<String>,
    pub goal_id: Option<String>,
}

pub async fn add(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: TaskAddArgs,
) -> Result<()> {
    let review_criteria = match args.review_criteria.as_deref() {
        Some(raw) => Some(
            split_list(raw)
                .into_iter()
                .map(|value| {
                    value.parse::<orch_domain::ReviewCriterion>().map_err(|()| {
                        orch_core::Error::invalid_arguments(format!(
                            "Unknown review criterion: \"{value}\". Use test_pass, typecheck or lint."
                        ))
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        None => None,
    };

    let task = engine
        .task_service
        .create(CreateTaskInput {
            title: args.title,
            description: args.description,
            priority: Some(args.priority),
            assignee: args.assignee,
            labels: args.labels.as_deref().map(split_list),
            depends_on: args.depends_on.as_deref().map(split_list),
            max_attempts: args.max_attempts,
            workspace_mode: args.workspace_mode,
            review_criteria,
            scope: args.scope.as_deref().map(split_list),
            goal_id: args.goal_id,
            attachments: None,
        })
        .await?;

    if ctx.json {
        print_json(&task)?;
    } else if ctx.quiet {
        println!("{}", task.id);
    } else {
        print_success(styles, &format!("Created {} \"{}\"", task.id, task.title));
    }
    Ok(())
}

pub async fn list(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    status: Option<TaskStatus>,
) -> Result<()> {
    let filter = TaskFilter {
        status,
        goal_id: None,
    };
    let tasks = engine.task_service.list(&filter).await?;

    if ctx.json {
        return print_json(&tasks);
    }
    if ctx.quiet {
        for task in &tasks {
            println!("{}", task.id);
        }
        return Ok(());
    }
    if tasks.is_empty() {
        println!(
            "\n  No tasks. Create one: {}\n",
            styles.dim("orch task add \"Title\"")
        );
        return Ok(());
    }

    let rows: Vec<Vec<String>> = tasks.iter().map(|task| task_row(styles, task)).collect();
    println!();
    print_table(
        styles,
        &["STATUS", "PRI", "TASK", "AGENT", "TIME"],
        &rows,
        2,
    );
    println!("\n  {}\n", summary_line(&tasks));
    Ok(())
}

fn task_row(styles: &Styles, task: &Task) -> Vec<String> {
    // The TIME column shows an elapsed duration only for in_progress and done;
    // everything else is an em dash.
    let time = styles.dim("—");
    vec![
        format!(
            "{} {}",
            styles.status_icon(task.status.as_str()),
            task.status
        ),
        styles.priority_label(task.priority),
        // `.slice(0, 35)` counts UTF-16 code units, so a surrogate pair can be
        // split; `chars().take(35)` cannot. The difference is only reachable
        // with astral characters in a title past column 35.
        task.title.chars().take(35).collect(),
        task.assignee
            .as_deref()
            .map(|a| styles.agent_name(a))
            .unwrap_or_else(|| styles.dim("—")),
        time,
    ]
}

/// `<n> tasks` — **not pluralized**, and the running/review/done clauses only
/// appear when non-zero.
fn summary_line(tasks: &[Task]) -> String {
    let count = |status: TaskStatus| tasks.iter().filter(|t| t.status == status).count();
    let running = count(TaskStatus::InProgress);
    let review = count(TaskStatus::Review);
    let done = count(TaskStatus::Done);

    let mut line = format!("{} tasks", tasks.len());
    if running > 0 {
        line.push_str(&format!(" · {running} running"));
    }
    if review > 0 {
        line.push_str(&format!(" · {review} review"));
    }
    if done > 0 {
        line.push_str(&format!(" · {done} done"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::make_task;

    fn task(status: TaskStatus) -> Task {
        Task {
            status,
            ..make_task()
        }
    }

    #[test]
    fn split_list_trims_and_keeps_empty_elements() {
        assert_eq!(split_list("a,b"), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(split_list(" a , b "), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(split_list("a"), vec!["a".to_owned()]);
        // The TS `split(',').map(trim)` keeps the empty element; replicated.
        assert_eq!(
            split_list("a,,b"),
            vec!["a".to_owned(), String::new(), "b".to_owned()]
        );
        assert_eq!(split_list(""), vec![String::new()]);
    }

    #[test]
    fn the_summary_line_is_not_pluralized_and_omits_zero_clauses() {
        assert_eq!(summary_line(&[]), "0 tasks");
        assert_eq!(summary_line(&[task(TaskStatus::Todo)]), "1 tasks");
        assert_eq!(
            summary_line(&[task(TaskStatus::Todo), task(TaskStatus::Todo)]),
            "2 tasks"
        );
        assert_eq!(
            summary_line(&[task(TaskStatus::InProgress), task(TaskStatus::Todo)]),
            "2 tasks · 1 running"
        );
        assert_eq!(
            summary_line(&[
                task(TaskStatus::InProgress),
                task(TaskStatus::Review),
                task(TaskStatus::Done),
            ]),
            "3 tasks · 1 running · 1 review · 1 done"
        );
    }

    #[test]
    fn a_row_renders_the_status_glyph_priority_and_an_em_dash_for_no_agent() {
        let styles = Styles::plain();
        assert_eq!(
            task_row(&styles, &task(TaskStatus::Todo)),
            vec!["○ todo", "P3", "Test task", "—", "—"]
        );
        assert_eq!(
            task_row(
                &styles,
                &Task {
                    status: TaskStatus::InProgress,
                    ..make_task()
                }
            )[0],
            "● in_progress"
        );
    }

    #[test]
    fn a_row_shows_the_assignee_when_set() {
        let styles = Styles::plain();
        let task = Task {
            assignee: Some("agt_creator".into()),
            ..make_task()
        };
        assert_eq!(task_row(&styles, &task)[3], "agt_creator");
    }

    #[test]
    fn a_long_title_is_truncated_to_35_characters() {
        let styles = Styles::plain();
        let task = Task {
            title: "x".repeat(50),
            ..make_task()
        };
        assert_eq!(task_row(&styles, &task)[2].chars().count(), 35);

        let task = Task {
            title: "short".into(),
            ..make_task()
        };
        assert_eq!(task_row(&styles, &task)[2], "short");
    }

    #[test]
    fn the_rendered_table_matches_the_reference_padding() {
        let styles = Styles::plain();
        let tasks = [task(TaskStatus::Todo)];
        let rows: Vec<Vec<String>> = tasks.iter().map(|t| task_row(&styles, t)).collect();
        assert_eq!(
            crate::output::render_table(
                &styles,
                &["STATUS", "PRI", "TASK", "AGENT", "TIME"],
                &rows,
                2
            ),
            "  STATUS  PRI  TASK       AGENT  TIME  \n  ○ todo  P3   Test task  —      —     \n"
        );
    }
}
