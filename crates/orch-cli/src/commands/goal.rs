//! `orch goal` — ported from `src/cli/commands/goal.ts`
//! (`docs/parity/cli.md` §10).
//!
//! Three things here are measured behaviour rather than taste:
//!
//! - `goal status` splits its failures across two exit codes — an unknown
//!   *status name* prints an error and exits **1**, while a known name that is
//!   not a legal transition comes out of the service as exit **2**;
//! - `goal delete --json` prints the **compact** `{"deleted":"…"}` (§22.4);
//! - `goal show --json` is the goal object *spread* with `tasks` and
//!   `progress` appended, and `progress` is dropped entirely when there is no
//!   progress entry (`undefined`, not `null`).

use std::process::ExitCode;

use orch_core::error::Result;
use orch_core::storage::GoalFilter;
use orch_domain::{CreateGoalInput, GOAL_STATUSES, Goal, GoalStatus};
use orch_engine::Engine;
use orch_engine::goal_service::UpdateGoalFields;
use serde_json::{Value, json};

use crate::context::CliContext;
use crate::output::{Styles, print_error_with_format, print_key_value, print_success, print_table};

/// Goal status icons. Hard-coded in the original — unlike the task icons
/// these have no ASCII fallback, so `--ascii` does not change them.
fn status_icon(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "\u{25CF}",
        GoalStatus::Paused => "\u{2016}",
        GoalStatus::Achieved => "\u{2713}",
        GoalStatus::Abandoned => "\u{2715}",
    }
}

pub async fn add(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    title: &str,
    description: Option<&str>,
    assignee: Option<&str>,
) -> Result<()> {
    let goal = engine
        .goal_service
        .create(CreateGoalInput {
            title: title.to_owned(),
            description: description.map(str::to_owned),
            assignee: assignee.map(str::to_owned),
        })
        .await?;

    if let Some(format) = ctx.output {
        return format.print(&goal);
    }
    if ctx.quiet {
        println!("{}", goal.id);
        return Ok(());
    }

    print_success(
        styles,
        &format!("Created goal \"{}\" ({})", goal.title, goal.id),
    );
    println!();
    println!("  {}", styles.dim("Tips for better results:"));
    println!(
        "  {} Be specific: {}",
        styles.dim("•"),
        styles.dim("\"Implement OAuth2 with Google\" > \"Add auth\"")
    );
    println!(
        "  {} Add {} with success criteria and constraints",
        styles.dim("•"),
        styles.dim("--description")
    );
    println!(
        "  {} Use {} to focus a specific agent on this goal",
        styles.dim("•"),
        styles.dim("--assignee")
    );
    Ok(())
}

pub async fn list(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    status: Option<&str>,
) -> Result<()> {
    let goals = match status {
        // `!filter?.status` in the original, so an empty `--status` is no
        // filter at all rather than a filter nothing matches.
        None | Some("") => engine.goal_service.list(&GoalFilter::default()).await?,
        Some(raw) => match raw.parse::<GoalStatus>() {
            Ok(status) => {
                engine
                    .goal_service
                    .list(&GoalFilter::with_status(status))
                    .await?
            }
            // An unknown status is not rejected: the original compares it by
            // equality, so nothing matches and the list comes back empty.
            Err(()) => Vec::new(),
        },
    };

    if let Some(format) = ctx.output {
        return format.print(&goals);
    }
    if goals.is_empty() {
        println!("{}", styles.dim("No goals found."));
        return Ok(());
    }

    let rows: Vec<Vec<String>> = goals.iter().map(|goal| row(styles, goal)).collect();
    print_table(styles, &["", "ID", "Title", "Status", "Assignee"], &rows, 2);
    Ok(())
}

pub async fn show(engine: &Engine, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    let goal = engine.goal_service.get(id).await?;
    let tasks = engine.goal_service.list_tasks_for_goal(id).await?;
    let progress = engine.goal_service.get_progress_report(id).await?;

    if let Some(format) = ctx.output {
        let mut merged = serde_json::to_value(&goal).expect("a goal serializes");
        let object = merged
            .as_object_mut()
            .expect("a goal serializes to an object");
        object.insert(
            "tasks".to_owned(),
            serde_json::to_value(&tasks).expect("tasks serialize"),
        );
        // `undefined` in the original, so the key is absent rather than null.
        if let Some(progress) = &progress {
            object.insert("progress".to_owned(), Value::String(progress.clone()));
        }
        return format.print(&merged);
    }

    print_key_value(
        styles,
        &[
            ("ID".to_owned(), goal.id.clone()),
            ("Title".to_owned(), goal.title.clone()),
            ("Status".to_owned(), goal.status.as_str().to_owned()),
            (
                "Assignee".to_owned(),
                goal.assignee.clone().unwrap_or_else(|| styles.dim("any")),
            ),
            (
                "Description".to_owned(),
                if goal.description.is_empty() {
                    styles.dim("none")
                } else {
                    goal.description.clone()
                },
            ),
            ("Created".to_owned(), goal.created_at.clone()),
            (
                "Updated".to_owned(),
                goal.updated_at
                    .clone()
                    .unwrap_or_else(|| styles.dim("never")),
            ),
        ],
    );

    if tasks.is_empty() {
        println!("\n  {}", styles.dim("No tasks linked to this goal yet."));
    } else {
        println!("\n  Tasks ({})\n  {}", tasks.len(), "─".repeat(42));
        let rows: Vec<Vec<String>> = tasks
            .iter()
            .map(|task| {
                vec![
                    format!(
                        "{} {}",
                        styles.status_icon(task.status.as_str()),
                        task.status
                    ),
                    task.id.clone(),
                    // `slice(0, 40)` over UTF-16 units.
                    slice_40(&task.title),
                    task.assignee
                        .as_deref()
                        .map(|assignee| styles.agent_name(assignee))
                        .unwrap_or_else(|| styles.dim("—")),
                ]
            })
            .collect();
        print_table(styles, &["STATUS", "ID", "TITLE", "AGENT"], &rows, 2);
    }

    if let Some(progress) = &progress {
        println!("\n  Progress Report\n  {}", "─".repeat(42));
        for line in progress.split('\n') {
            println!("  {line}");
        }
    }

    println!();
    Ok(())
}

/// An unknown status name is reported here and exits **1**; an illegal
/// transition is reported by the service and exits 2.
pub async fn status(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    raw_status: &str,
    force: bool,
) -> Result<ExitCode> {
    let Ok(status) = raw_status.parse::<GoalStatus>() else {
        print_error_with_format(
            styles,
            ctx.output,
            &format!(
                "Invalid status \"{raw_status}\". Valid: {}",
                GOAL_STATUSES
                    .iter()
                    .map(GoalStatus::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None,
        );
        return Ok(ExitCode::from(1));
    };

    let goal = engine.goal_service.update_status(id, status, force).await?;

    if let Some(format) = ctx.output {
        format.print(&goal)?;
    } else if ctx.quiet {
        println!("{}", goal.id);
    } else {
        print_success(
            styles,
            &format!("Goal \"{}\" → {}", goal.title, goal.status),
        );
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn update(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    fields: UpdateGoalFields,
) -> Result<()> {
    let goal = engine.goal_service.update(id, &fields).await?;

    if let Some(format) = ctx.output {
        format.print(&goal)?;
    } else if ctx.quiet {
        println!("{}", goal.id);
    } else {
        print_success(styles, &format!("Updated goal \"{}\"", goal.title));
    }
    Ok(())
}

pub async fn delete(engine: &Engine, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    engine.goal_service.delete(id).await?;

    if let Some(format) = ctx.output {
        // Compact, unlike every other goal `--json` site.
        return format.print_compact(&json!({ "deleted": id }));
    }
    if !ctx.quiet {
        print_success(styles, &format!("Deleted goal {id}"));
    }
    Ok(())
}

fn row(styles: &Styles, goal: &Goal) -> Vec<String> {
    vec![
        status_icon(goal.status).to_owned(),
        goal.id.clone(),
        goal.title.clone(),
        goal.status.as_str().to_owned(),
        goal.assignee.clone().unwrap_or_else(|| styles.dim("any")),
    ]
}

/// `title.slice(0, 40)` in UTF-16 units, dropping a character that would
/// straddle the cut rather than emitting half of it.
fn slice_40(title: &str) -> String {
    if title.encode_utf16().count() <= 40 {
        return title.to_owned();
    }
    let mut units = 0;
    let mut out = String::new();
    for ch in title.chars() {
        let width = ch.len_utf16();
        if units + width > 40 {
            break;
        }
        units += width;
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(status: GoalStatus, assignee: Option<&str>) -> Goal {
        Goal {
            id: "goal_1".to_owned(),
            title: "Ship v2".to_owned(),
            description: String::new(),
            status,
            assignee: assignee.map(str::to_owned),
            created_at: "2026-10-13T16:03:00.479Z".to_owned(),
            updated_at: None,
        }
    }

    #[test]
    fn each_status_has_its_own_icon() {
        assert_eq!(status_icon(GoalStatus::Active), "●");
        assert_eq!(status_icon(GoalStatus::Paused), "‖");
        assert_eq!(status_icon(GoalStatus::Achieved), "✓");
        assert_eq!(status_icon(GoalStatus::Abandoned), "✕");
    }

    #[test]
    fn an_unassigned_goal_lists_as_any() {
        let styles = Styles::plain();
        assert_eq!(row(&styles, &goal(GoalStatus::Active, None))[4], "any");
        assert_eq!(
            row(&styles, &goal(GoalStatus::Active, Some("agt_1")))[4],
            "agt_1"
        );
    }

    #[test]
    fn a_long_task_title_is_cut_at_forty_utf16_units() {
        assert_eq!(slice_40("short"), "short");
        let exactly_forty = "x".repeat(40);
        assert_eq!(slice_40(&exactly_forty), exactly_forty);
        assert_eq!(slice_40(&"x".repeat(41)).len(), 40);
        // 21 astral characters are 42 units, so 20 whole ones survive.
        assert_eq!(slice_40(&"\u{1f600}".repeat(21)).chars().count(), 20);
    }
}
