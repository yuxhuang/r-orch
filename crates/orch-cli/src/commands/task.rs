//! `orch task`.
//!
//! Four details that are easy to lose:
//!
//! - `show` is a long cascade of optional sections, each printed only when
//!   its field is non-empty, and `--json` prints the task and nothing else;
//! - `edit` compares the edited buffer against the existing task and prints
//!   `  No changes.` when nothing differs, without touching the store;
//! - `cancel` reaches the daemon, which owns and stops any live process;
//! - none of `cancel`/`approve`/`reject`/`retry` honours `--json` or
//!   `--quiet`: they always print their success line.

use orch_core::error::Result;
use orch_domain::{RemoteRunRef, Task, TaskStatus};
use orch_proto::ops::args;
use serde_json::json;

use crate::commands::rpc::{Rpc, argument_error, arguments, client_error};

use crate::context::CliContext;
use crate::editor;
use crate::output::{Styles, print_key_value, print_success, print_table};

/// Comma-separated list option: split and trim, keeping empty elements:
/// `"a,,b"` yields `["a", "", "b"]`.
fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|part| part.trim().to_owned())
        .collect()
}

/// `--attach` paths. Unlike [`split_list`], empty elements are dropped: a
/// trailing comma names no file, and an empty path would only surface as a
/// confusing "Attachment file not found: " with nothing after it.
fn attachment_list(value: &str) -> Vec<String> {
    split_list(value)
        .into_iter()
        .filter(|path| !path.is_empty())
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
    /// `--attach`: comma-separated source paths, copied by the task service.
    pub attach: Option<String>,
}

pub async fn add(engine: &Rpc, ctx: &CliContext, styles: &Styles, args: TaskAddArgs) -> Result<()> {
    let task = engine.request(create_args(args)?).await?;

    if let Some(format) = ctx.output {
        format.print(&task)?;
    } else if ctx.quiet {
        println!("{}", task.id);
    } else {
        print_success(styles, &format!("Created {} \"{}\"", task.id, task.title));
    }
    Ok(())
}

fn create_args(args: TaskAddArgs) -> Result<args::TaskCreateArgs> {
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

    // Paths are interpreted by the caller, never against the daemon's cwd.
    let attachments = args
        .attach
        .as_deref()
        .map(absolute_attachments)
        .transpose()?;
    arguments::<args::TaskCreateArgs>(json!({
        "title": args.title,
        "description": args.description,
        "priority": args.priority,
        "assignee": args.assignee,
        "labels": args.labels.as_deref().map(split_list),
        "depends_on": args.depends_on.as_deref().map(split_list),
        "max_attempts": args.max_attempts,
        "workspace_mode": args.workspace_mode,
        "review_criteria": review_criteria,
        "scope": args.scope.as_deref().map(split_list),
        "goal_id": args.goal_id,
        "attachments": attachments,
    }))
}

pub async fn list(
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    status: Option<TaskStatus>,
) -> Result<()> {
    let tasks = engine
        .call::<args::TaskListArgs>(json!({"status": status}))
        .await?;

    if let Some(format) = ctx.output {
        return format.print(&tasks);
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

    let running = engine.request(args::StateGetArgs {}).await?.running;
    let remotes = engine.request(args::RemoteStatusArgs {}).await?.remotes;
    let agents = engine.request(args::AgentListArgs {}).await?;
    let mut rows = Vec::new();
    for task in &tasks {
        let remote = running
            .get(&task.id)
            .and_then(|entry| entry.remote.as_ref());
        let mut row = task_row(styles, task, remote);
        row.push(waiting_reason(task, remote, &remotes, &agents).unwrap_or_default());
        rows.push(row);
    }
    println!();
    print_table(
        styles,
        &["STATUS", "PRI", "TASK", "AGENT", "TIME", "WAITING"],
        &rows,
        2,
    );
    println!("\n  {}\n", summary_line(&tasks));
    Ok(())
}

/// Infer display-only blockers from daemon snapshots.
fn waiting_reason(
    task: &Task,
    remote: Option<&RemoteRunRef>,
    remotes: &[orch_domain::RemoteStatus],
    candidates: &[orch_domain::Agent],
) -> Option<String> {
    if !matches!(task.status, TaskStatus::Todo | TaskStatus::InProgress) {
        return None;
    }
    if let Some(hint) = remote.and_then(|remote| remote.hint()) {
        return Some(hint);
    }
    if let Some((_, alias)) = task.assignee.as_deref().and_then(|id| id.split_once('@'))
        && !remotes
            .iter()
            .any(|status| status.alias == alias && status.online)
    {
        return Some(format!(
            "remote {alias} offline{}",
            if task.status == TaskStatus::InProgress {
                " (outcome pending)"
            } else {
                ""
            }
        ));
    }
    if task.status != TaskStatus::Todo {
        return None;
    }
    let caps: Vec<_> = task
        .labels
        .iter()
        .filter_map(|l| l.strip_prefix("cap:"))
        .collect();
    if caps.is_empty() {
        return None;
    }
    let eligible = candidates.iter().any(|agent| {
        task.assignee.as_ref().is_none_or(|id| id == &agent.id)
            && agent.remote.as_ref().is_none_or(|remote| remote.online)
            && !matches!(
                agent.status,
                orch_domain::AgentStatus::Disabled | orch_domain::AgentStatus::Offline
            )
            && caps
                .iter()
                .all(|cap| agent.capabilities.iter().any(|c| c == cap))
    });
    (!eligible).then(|| format!("no online agent with cap {}", caps.join(", ")))
}

fn absolute_attachments(value: &str) -> Result<Vec<orch_proto::path::WirePath>> {
    attachment_list(value)
        .into_iter()
        .map(|path| {
            let absolute = std::path::absolute(path).map_err(|error| {
                orch_core::Error::other(format!("resolve attachment path: {error}"))
            })?;
            orch_proto::path::WirePath::new(absolute).map_err(argument_error)
        })
        .collect()
}

/// `remote` is the task's entry in `state.running`, when its run is on a peer.
/// Its phase replaces the local status in the STATUS column: `in_progress`
/// here can mean the worker has not even acknowledged the submit.
fn task_row(styles: &Styles, task: &Task, remote: Option<&RemoteRunRef>) -> Vec<String> {
    // The TIME column shows an elapsed duration only for in_progress and done;
    // everything else is an em dash.
    let time = styles.dim("—");
    vec![
        format!(
            "{} {}",
            styles.status_icon(task.status.as_str()),
            remote
                .and_then(|remote| remote.status_label())
                .unwrap_or_else(|| task.status.to_string())
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

/// `task assign <task-id> <agent-id>`.
///
/// Prints `Assigned <id> → <resolved assignee>`, falling back to the argument
/// when the task ended up with no assignee at all. **Not** `--quiet`-aware:
/// `task assign` is in the ignored half of the matrix.
pub async fn assign(
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    task_id: &str,
    agent_id: &str,
) -> Result<()> {
    let task = engine
        .call::<args::TaskAssignArgs>(json!({"id": task_id, "agent_id": agent_id}))
        .await?;

    if let Some(format) = ctx.output {
        return format.print(&task);
    }
    print_success(
        styles,
        &format!(
            "Assigned {} → {}",
            task.id,
            task.assignee.as_deref().unwrap_or(agent_id)
        ),
    );
    Ok(())
}

pub async fn show(engine: &Rpc, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    let task = engine.call::<args::TaskGetArgs>(json!({"id": id})).await?;

    if let Some(format) = ctx.output {
        return format.print(&task);
    }

    println!("\n  {}", task.title);
    println!("  {}", "═".repeat(42));
    println!();

    let remote = engine
        .request(args::StateGetArgs {})
        .await?
        .running
        .get(&task.id)
        .and_then(|entry| entry.remote.clone());
    let mut pairs = vec![
        (
            "Status".to_owned(),
            format!(
                "{} {} · attempt {}/{}",
                styles.status_icon(task.status.as_str()),
                remote
                    .as_ref()
                    .and_then(|remote| remote.status_label())
                    .unwrap_or_else(|| task.status.to_string()),
                task.attempts,
                task.max_attempts
            ),
        ),
        ("Priority".to_owned(), styles.priority_label(task.priority)),
    ];
    let remotes = engine.request(args::RemoteStatusArgs {}).await?.remotes;
    let agents = engine.request(args::AgentListArgs {}).await?;
    if let Some(reason) = waiting_reason(&task, remote.as_ref(), &remotes, &agents) {
        pairs.push(("Waiting".to_owned(), reason));
    }
    if let Some(assignee) = &task.assignee {
        pairs.push(("Agent".to_owned(), styles.agent_name(assignee)));
    }
    if !task.labels.is_empty() {
        pairs.push(("Labels".to_owned(), task.labels.join(", ")));
    }
    if let Some(scope) = task.scope.as_ref().filter(|scope| !scope.is_empty()) {
        pairs.push(("Scope".to_owned(), scope.join(", ")));
    }
    if let Some(mode) = &task.workspace_mode {
        pairs.push(("Workspace".to_owned(), mode.as_str().to_owned()));
    }
    if let Some(workspace) = &task.workspace {
        pairs.push(("Path".to_owned(), styles.file_path(workspace)));
    }
    if let Some(criteria) = task
        .review_criteria
        .as_ref()
        .filter(|criteria| !criteria.is_empty())
    {
        pairs.push((
            "Review".to_owned(),
            criteria
                .iter()
                .map(|criterion| criterion.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if let Some(feedback) = &task.feedback {
        pairs.push(("Feedback".to_owned(), feedback.clone()));
    }
    pairs.push(("Created".to_owned(), task.created_at.clone()));
    print_key_value(styles, &pairs);

    if let Some(attachments) = task
        .attachments
        .as_ref()
        .filter(|attachments| !attachments.is_empty())
    {
        println!(
            "\n  Attachments ({})\n  {}",
            attachments.len(),
            "─".repeat(42)
        );
        for attachment in attachments {
            println!("    {}", styles.file_path(attachment));
        }
    }

    if !task.description.is_empty() {
        println!("\n  Description\n  {}", "─".repeat(42));
        for line in task.description.split('\n') {
            println!("  {line}");
        }
    }

    if let Some(proof) = &task.proof {
        println!("\n  Result\n  {}", "─".repeat(42));
        if let Some(branch) = &proof.branch {
            println!("  Branch:        {branch}");
        }
        if let Some(pr_url) = &proof.pr_url {
            println!("  PR:            {pr_url}");
        }
        if !proof.files_changed.is_empty() {
            println!("  Files changed:");
            for file in &proof.files_changed {
                println!("    • {file}");
            }
        }
        if let Some(results) = &proof.test_results {
            println!("  Test results:");
            for line in results.split('\n') {
                println!("    {line}");
            }
        }
        if let Some(summary) = &proof.agent_summary {
            println!("  Agent summary:");
            for line in summary.split('\n') {
                println!("    {line}");
            }
        }
    }

    if let Some(results) = task
        .review_results
        .as_ref()
        .filter(|results| !results.is_empty())
    {
        println!("\n  Review Results\n  {}", "─".repeat(42));
        for result in results {
            // Hard-coded glyphs, unlike `statusIcon` — no ASCII fallback.
            let icon = if result.passed { "✓" } else { "✗" };
            let outcome = if result.passed { "passed" } else { "failed" };
            println!("  {icon} {}: {outcome}", result.criterion.as_str());
            if !result.output.is_empty() {
                for line in result.output.split('\n') {
                    println!("    {line}");
                }
            }
        }
    }

    println!();
    Ok(())
}

/// `task edit` — round-trip the task through `$EDITOR` and write back only
/// what changed.
pub async fn edit(engine: &Rpc, styles: &Styles, id: &str) -> Result<()> {
    let base = engine
        .call::<args::TaskDefinitionGetArgs>(json!({"id": id}))
        .await?;
    let authored: serde_json::Value =
        serde_json::from_str(&base.document).map_err(argument_error)?;
    let mut existing = engine.call::<args::TaskGetArgs>(json!({"id": id})).await?;
    existing.title = authored["title"]
        .as_str()
        .expect("task definition title")
        .to_owned();
    existing.description = authored["description"]
        .as_str()
        .expect("task definition description")
        .to_owned();
    existing.priority = authored["priority"]
        .as_i64()
        .expect("task definition priority");

    // Informational only: the note is a comment line, so it comes back as
    // part of the description unless the user deletes it — which is what the
    // original does too.
    let attachment_note = match existing
        .attachments
        .as_ref()
        .filter(|attachments| !attachments.is_empty())
    {
        Some(attachments) => format!("\n# Attachments: {}", attachments.join(", ")),
        None => String::new(),
    };
    let initial = format!(
        "{}{attachment_note}",
        editor::task_buffer(&existing.title, existing.priority, &existing.description)
    );

    let edited = editor::open_in_editor(&initial, ".yml").await?;
    let parsed = editor::parse_task_content(&edited);

    let mut patch = serde_json::Map::new();
    if let Some(title) = parsed.title.filter(|title| *title != existing.title) {
        patch.insert("title".into(), json!(title));
    }
    if let Some(priority) = parsed.priority.filter(|p| *p != existing.priority) {
        patch.insert("priority".into(), json!(priority));
    }
    if let Some(description) = parsed
        .description
        .filter(|description| *description != existing.description)
    {
        patch.insert("description".into(), json!(description));
    }
    if patch.is_empty() {
        println!("  No changes.");
        return Ok(());
    }

    let updated = definition_request(
        engine,
        id,
        &serde_json::Value::Object(patch),
        crate::document_input::MutationKind::Patch,
        Some(base.digest),
    )
    .await?;
    print_success(
        styles,
        &format!("Updated {} \"{}\"", updated.id, updated.title),
    );
    Ok(())
}

/// `task cancel` stops a live process through the daemon's coordinator.
pub async fn cancel(engine: &Rpc, styles: &Styles, id: &str) -> Result<()> {
    engine
        .call::<args::TaskCancelArgs>(json!({"id": id}))
        .await?;
    print_success(styles, &format!("Cancelled {id}"));
    Ok(())
}

pub async fn approve(engine: &Rpc, styles: &Styles, id: &str) -> Result<()> {
    let task = engine
        .call::<args::TaskApproveArgs>(json!({"id": id}))
        .await?;
    if task.status == TaskStatus::Done {
        print_success(styles, &format!("Approved {id}"));
    } else {
        print_success(styles, &format!("Approved {id} → released to run"));
    }
    Ok(())
}

pub async fn reject(engine: &Rpc, styles: &Styles, id: &str, reason: Option<&str>) -> Result<()> {
    engine
        .call::<args::TaskRejectArgs>(json!({"id": id, "feedback": reason}))
        .await?;
    let suffix = match reason {
        Some(reason) => format!(" (reason: {reason})"),
        None => String::new(),
    };
    print_success(styles, &format!("Rejected {id} → todo{suffix}"));
    Ok(())
}

pub async fn retry(engine: &Rpc, styles: &Styles, id: &str) -> Result<()> {
    engine
        .call::<args::TaskRetryArgs>(json!({"id": id}))
        .await?;
    print_success(styles, &format!("Reset {id} to todo"));
    Ok(())
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

pub async fn mutate(
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    args: &crate::tree::EntityDefinitionInput,
    kind: crate::document_input::MutationKind,
) -> Result<()> {
    let input =
        crate::document_input::read_document(&args.input.file, args.input.input_format).await?;
    let file = args.input.file.clone();
    let source = move |err| crate::document_input::at_source(&file, err);
    let updated = apply_definition(engine, &args.id, &input, kind, &source).await?;
    if let Some(format) = ctx.output {
        format.print(&updated)?;
    } else if ctx.quiet {
        println!("{}", updated.id);
    } else {
        print_success(styles, &format!("Updated task {}", updated.id));
    }
    Ok(())
}

/// The daemon applies and validates the bounded definition atomically.
/// Input-source attribution retains the typed transport error at the exit boundary.
pub(crate) async fn apply_definition(
    engine: &Rpc,
    id: &str,
    input: &serde_json::Value,
    kind: crate::document_input::MutationKind,
    source: &(dyn Fn(orch_core::Error) -> orch_core::Error + Sync),
) -> Result<Task> {
    definition_request(engine, id, input, kind, None)
        .await
        .map_err(source)
}

/// Keep large authored JSON out of the single-frame lane. Upload and apply
/// share one session lock because transfer tokens are connection-local.
async fn definition_request(
    engine: &Rpc,
    id: &str,
    input: &serde_json::Value,
    kind: crate::document_input::MutationKind,
    base_digest: Option<orch_proto::bounded::Id>,
) -> Result<Task> {
    let document = serde_json::to_string(input).map_err(argument_error)?;
    let large = document.len() > orch_proto::frame::MAX_FRAME_BYTES / 4;
    let mut request: args::TaskDefinitionApplyArgs = arguments(json!({
        "id": id,
        "definition": {
            "input": {"document": if large { None } else { Some(&document) }, "format": "json"},
            "kind": match kind { crate::document_input::MutationKind::Update => "update", crate::document_input::MutationKind::Patch => "patch" },
            "base_digest": base_digest,
        }
    }))?;
    if !large {
        return engine.request(request).await;
    }
    let mut lane = engine.lane().await;
    request.definition.input.input_transfer = Some(
        lane.upload(args::TransferPurpose::Document, document.as_bytes())
            .await
            .map_err(client_error)?,
    );
    lane.request(request).await.map_err(client_error)
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
    fn waiting_reasons_distinguish_missing_cap_and_pending_outcome() {
        let mut task = task(TaskStatus::Todo);
        task.labels = vec!["cap:freebsd".into()];
        assert_eq!(
            waiting_reason(&task, None, &[], &[]).as_deref(),
            Some("no online agent with cap freebsd")
        );
        task.assignee = Some("agt_worker@w1".into());
        assert_eq!(
            waiting_reason(&task, None, &[], &[]).as_deref(),
            Some("remote w1 offline")
        );
        task.status = TaskStatus::InProgress;
        assert_eq!(
            waiting_reason(&task, None, &[], &[]).as_deref(),
            Some("remote w1 offline (outcome pending)")
        );
        task.status = TaskStatus::Done;
        assert_eq!(waiting_reason(&task, None, &[], &[]), None);
        // A worker's own reason outranks anything inferred from a snapshot.
        task.status = TaskStatus::InProgress;
        let remote = RemoteRunRef {
            phase: orch_domain::RemotePhase::Queued,
            worker_task_id: Some("tsk_mirror".into()),
            reason: Some(format!(
                "{} run `orch task approve tsk_mirror` on this worker",
                orch_domain::AWAITING_APPROVAL_REASON
            )),
            ..RemoteRunRef::new("w1", "run_1")
        };
        assert_eq!(
            waiting_reason(&task, Some(&remote), &[], &[]).as_deref(),
            Some("awaiting approval: orch task approve tsk_mirror on w1")
        );
    }

    #[test]
    fn task_create_uses_protocol_goal_id_and_absolute_caller_paths() {
        let wire = create_args(TaskAddArgs {
            title: "a task".into(),
            priority: 2,
            goal_id: Some("goal_1".into()),
            attach: Some(" ./relative.md , /tmp/absolute.png,".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(wire.goal_id.as_deref(), Some("goal_1"));
        let paths = wire.attachments.unwrap().into_inner();
        assert_eq!(
            paths[0].as_path(),
            std::path::absolute("relative.md").unwrap()
        );
        assert_eq!(
            paths[1].as_path(),
            std::path::Path::new("/tmp/absolute.png")
        );
        assert!(paths.iter().all(|path| path.as_path().is_absolute()));
    }

    #[test]
    fn task_create_rejects_bad_criteria_and_unbounded_ids_before_sending() {
        let bad = create_args(TaskAddArgs {
            title: "task".into(),
            review_criteria: Some("unknown".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(bad.exit_code(), 2);
        assert!(
            create_args(TaskAddArgs {
                title: "task".into(),
                goal_id: Some("x".repeat(257)),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn attachment_list_trims_and_drops_empty_elements() {
        assert_eq!(
            attachment_list(" /a/b.png , c.md,"),
            vec!["/a/b.png".to_owned(), "c.md".to_owned()]
        );
        assert!(attachment_list(" , ").is_empty());
    }

    #[test]
    fn split_list_trims_and_keeps_empty_elements() {
        assert_eq!(split_list("a,b"), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(split_list(" a , b "), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(split_list("a"), vec!["a".to_owned()]);
        // The empty element is kept.
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
            task_row(&styles, &task(TaskStatus::Todo), None),
            vec!["○ todo", "P3", "Test task", "—", "—"]
        );
        assert_eq!(
            task_row(
                &styles,
                &Task {
                    status: TaskStatus::InProgress,
                    ..make_task()
                },
                None
            )[0],
            "● in_progress"
        );
    }

    #[test]
    fn a_remote_row_shows_the_worker_phase_instead_of_the_local_status() {
        let styles = Styles::plain();
        let task = Task {
            status: TaskStatus::InProgress,
            assignee: Some("agt_peer@2com".into()),
            ..make_task()
        };
        let mut remote = RemoteRunRef::new("2com", "run_1");
        let status = |remote: &RemoteRunRef| task_row(&styles, &task, Some(remote))[0].clone();
        assert_eq!(status(&remote), "● dispatched → 2com");
        remote.phase = orch_domain::RemotePhase::Queued;
        assert_eq!(status(&remote), "● queued @2com");
        remote.phase = orch_domain::RemotePhase::Running;
        assert_eq!(status(&remote), "● RUN @2com");
        remote.phase = orch_domain::RemotePhase::Unknown;
        assert_eq!(status(&remote), "● RUN? (2com offline)");
        // Once the outcome has arrived the task's own status is the truth.
        remote.phase = orch_domain::RemotePhase::Terminal;
        assert_eq!(status(&remote), "● in_progress");
        assert_eq!(
            task_row(&styles, &task, None)[0],
            "● in_progress",
            "a local task is unaffected"
        );
    }

    #[test]
    fn a_row_shows_the_assignee_when_set() {
        let styles = Styles::plain();
        let task = Task {
            assignee: Some("agt_creator".into()),
            ..make_task()
        };
        assert_eq!(task_row(&styles, &task, None)[3], "agt_creator");
    }

    #[test]
    fn a_long_title_is_truncated_to_35_characters() {
        let styles = Styles::plain();
        let task = Task {
            title: "x".repeat(50),
            ..make_task()
        };
        assert_eq!(task_row(&styles, &task, None)[2].chars().count(), 35);

        let task = Task {
            title: "short".into(),
            ..make_task()
        };
        assert_eq!(task_row(&styles, &task, None)[2], "short");
    }

    #[test]
    fn the_rendered_table_matches_the_reference_padding() {
        let styles = Styles::plain();
        let tasks = [task(TaskStatus::Todo)];
        let rows: Vec<Vec<String>> = tasks.iter().map(|t| task_row(&styles, t, None)).collect();
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
