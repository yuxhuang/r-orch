//! `orch task`.
//!
//! Four details that are easy to lose:
//!
//! - `show` is a long cascade of optional sections, each printed only when
//!   its field is non-empty, and `--json` prints the task and nothing else;
//! - `edit` compares the edited buffer against the existing task and prints
//!   `  No changes.` when nothing differs, without touching the store;
//! - `cancel` builds the **full** container only when the task is
//!   `in_progress`, because only then is there a process to kill, and falls
//!   back to a store-only cancel when another process holds the lock;
//! - none of `cancel`/`approve`/`reject`/`retry` honours `--json` or
//!   `--quiet`: they always print their success line.

use orch_core::error::Result;
use orch_core::storage::TaskFilter;
use orch_domain::{CreateTaskInput, RemoteRunRef, Task, TaskStatus};
use orch_engine::Engine;

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
            attachments: args.attach.as_deref().map(attachment_list),
        })
        .await?;

    if let Some(format) = ctx.output {
        format.print(&task)?;
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

    let running = engine.state.read().await?.running;
    let mut rows = Vec::new();
    for task in &tasks {
        let remote = running
            .get(&task.id)
            .and_then(|entry| entry.remote.as_ref());
        let mut row = task_row(styles, task, remote);
        row.push(
            waiting_reason(engine, task, remote)
                .await?
                .unwrap_or_default(),
        );
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

/// Describe dispatch blockers from persisted peer snapshots; this is display
/// only and does not expand the directory's six permitted consumers.
async fn waiting_reason(
    engine: &Engine,
    task: &Task,
    remote: Option<&RemoteRunRef>,
) -> Result<Option<String>> {
    use orch_infra::storage::remote::FileRemoteStore;
    if !matches!(task.status, TaskStatus::Todo | TaskStatus::InProgress) {
        return Ok(None);
    }
    // The worker's own explanation beats anything inferred from a snapshot:
    // only it can say which `orch task approve` a mirror is waiting for.
    if let Some(hint) = remote.and_then(|remote| remote.hint()) {
        return Ok(Some(hint));
    }
    let store = FileRemoteStore::new(engine.paths.clone());
    if let Some((_, alias)) = task.assignee.as_deref().and_then(|id| id.split_once('@'))
        && !store.read_status(alias).await?.is_some_and(|s| s.online)
    {
        return Ok(Some(format!(
            "remote {alias} offline{}",
            if task.status == TaskStatus::InProgress {
                " (outcome pending)"
            } else {
                ""
            }
        )));
    }
    if task.status != TaskStatus::Todo {
        return Ok(None);
    }
    let caps: Vec<_> = task
        .labels
        .iter()
        .filter_map(|l| l.strip_prefix("cap:"))
        .collect();
    if caps.is_empty() {
        return Ok(None);
    }
    let mut candidates = engine.agents.list().await?;
    for remote in store.list().await? {
        if let Some(status) = store.read_status(&remote.alias).await?
            && status.online
        {
            for mut agent in status.agents {
                agent.id = format!("{}@{}", agent.id, remote.alias);
                candidates.push(agent);
            }
        }
    }
    let eligible = candidates.iter().any(|agent| {
        task.assignee.as_ref().is_none_or(|id| id == &agent.id)
            && !matches!(
                agent.status,
                orch_domain::AgentStatus::Disabled | orch_domain::AgentStatus::Offline
            )
            && caps
                .iter()
                .all(|cap| agent.capabilities.iter().any(|c| c == cap))
    });
    Ok((!eligible).then(|| format!("no online agent with cap {}", caps.join(", "))))
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
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    task_id: &str,
    agent_id: &str,
) -> Result<()> {
    let task = engine.task_service.assign(task_id, agent_id).await?;

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

pub async fn show(engine: &Engine, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    let task = engine.task_service.get(id).await?;

    if let Some(format) = ctx.output {
        return format.print(&task);
    }

    println!("\n  {}", task.title);
    println!("  {}", "═".repeat(42));
    println!();

    let remote = engine
        .state
        .read()
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
    if let Some(reason) = waiting_reason(engine, &task, remote.as_ref()).await? {
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
pub async fn edit(engine: &Engine, styles: &Styles, id: &str) -> Result<()> {
    let existing = engine.task_service.get(id).await?;

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

    let mut fields = orch_engine::task_service::UpdateTaskFields::default();
    if let Some(title) = parsed.title.filter(|title| *title != existing.title) {
        fields.title = Some(title);
    }
    if let Some(priority) = parsed.priority.filter(|p| *p != existing.priority) {
        fields.priority = Some(priority);
    }
    if let Some(description) = parsed
        .description
        .filter(|description| *description != existing.description)
    {
        fields.description = Some(description);
    }

    if fields == orch_engine::task_service::UpdateTaskFields::default() {
        println!("  No changes.");
        return Ok(());
    }

    let updated = engine.task_service.update(id, fields).await?;
    print_success(
        styles,
        &format!("Updated {} \"{}\"", updated.id, updated.title),
    );
    Ok(())
}

/// `task cancel` — the one command that upgrades to the full container
/// mid-flight, and only for an `in_progress` task: cancelling that one has to
/// kill the agent process, release the claim and finish the run, which lives
/// on the orchestrator.
///
/// When another process owns the project (`serve`, `run --watch`, the TUI),
/// the orchestrator cannot take the lock. The task is then cancelled in the
/// store only, and the owner's next reconcile sees it terminal and stops the
/// run itself.
pub async fn cancel(engine: &Engine, styles: &Styles, id: &str) -> Result<()> {
    let task = engine.task_service.get(id).await?;

    if task.status == TaskStatus::InProgress {
        let full = orch_engine::build_full(engine.paths.project_root().to_path_buf()).await?;
        match full.orchestrator.cancel_task(id).await {
            Err(orch_core::Error::Domain(orch_domain::DomainError::LockConflict(_))) => {
                engine.task_service.cancel(id).await?;
            }
            result => result?,
        }
    } else {
        engine.task_service.cancel(id).await?;
    }
    print_success(styles, &format!("Cancelled {id}"));
    Ok(())
}

pub async fn approve(engine: &Engine, styles: &Styles, id: &str) -> Result<()> {
    let task = engine.task_service.approve(id).await?;
    if task.status == TaskStatus::Done {
        print_success(styles, &format!("Approved {id}"));
    } else {
        print_success(styles, &format!("Approved {id} → released to run"));
    }
    Ok(())
}

pub async fn reject(
    engine: &Engine,
    styles: &Styles,
    id: &str,
    reason: Option<&str>,
) -> Result<()> {
    engine
        .task_service
        .reject(id, reason.map(str::to_owned))
        .await?;
    let suffix = match reason {
        Some(reason) => format!(" (reason: {reason})"),
        None => String::new(),
    };
    print_success(styles, &format!("Rejected {id} → todo{suffix}"));
    Ok(())
}

pub async fn retry(engine: &Engine, styles: &Styles, id: &str) -> Result<()> {
    engine.task_service.retry(id).await?;
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

const WRITABLE: &[&str] = &["title", "description", "priority", "labels"];
const READONLY: &[&str] = &[
    "id",
    "status",
    "assignee",
    "depends_on",
    "created_at",
    "updated_at",
    "attempts",
    "max_attempts",
    "workspace_mode",
    "workspace",
    "proof",
    "review_criteria",
    "review_results",
    "scope",
    "feedback",
    "goalId",
    "attachments",
];

fn definition_candidate(
    current: &serde_json::Value,
    input: &serde_json::Value,
    kind: crate::document_input::MutationKind,
) -> Result<serde_json::Value> {
    use crate::document_input::{MutationKind, apply_mutation, reject_nulls, writable_input};
    use orch_core::Error;
    let input = writable_input(input, current, WRITABLE, READONLY, kind)?;
    if kind == MutationKind::Update {
        reject_nulls(&input, "task")?;
    }
    let base = serde_json::Value::Object(
        current
            .as_object()
            .expect("serialized task")
            .iter()
            .filter(|(key, _)| WRITABLE.contains(&key.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    let mut candidate = apply_mutation(&base, &input, kind)?;
    for field in ["title", "description"] {
        if candidate.get(field).and_then(|v| v.as_str()).is_none() {
            return Err(Error::invalid_arguments(format!(
                "{field}: required string"
            )));
        }
    }
    if candidate["title"]
        .as_str()
        .expect("validated")
        .trim()
        .is_empty()
    {
        return Err(Error::invalid_arguments("title: cannot be blank"));
    }
    if !candidate
        .get("priority")
        .and_then(|v| v.as_i64())
        .is_some_and(|v| (1..=4).contains(&v))
    {
        return Err(Error::invalid_arguments(
            "priority: required integer between 1 and 4",
        ));
    }
    if candidate.get("labels").is_none() {
        candidate["labels"] = serde_json::json!([]);
    }
    if !candidate["labels"]
        .as_array()
        .is_some_and(|items| items.iter().all(|v| v.is_string()))
    {
        return Err(Error::invalid_arguments(
            "labels: expected an array of strings",
        ));
    }
    Ok(candidate)
}

pub async fn mutate(
    engine: &Engine,
    ctx: &CliContext,
    styles: &Styles,
    args: &crate::tree::EntityDefinitionInput,
    kind: crate::document_input::MutationKind,
) -> Result<()> {
    use orch_core::Error;
    let input =
        crate::document_input::read_document(&args.input.file, args.input.input_format).await?;
    let _lock =
        orch_infra::storage::fs_utils::mutation_lock(&engine.paths.root().join(".definition.lock"))
            .await
            .map_err(|err| crate::document_input::at_source(&args.input.file, err))?;
    let current = engine.task_service.get(&args.id).await?;
    let value = serde_json::to_value(&current).map_err(|_| Error::other("serialize task"))?;
    let candidate = definition_candidate(&value, &input, kind)
        .map_err(|err| crate::document_input::at_source(&args.input.file, err))?;
    let updated = engine
        .task_service
        .update_definition(
            &args.id,
            orch_engine::task_service::UpdateTaskFields {
                title: Some(candidate["title"].as_str().expect("validated").to_owned()),
                description: Some(
                    candidate["description"]
                        .as_str()
                        .expect("validated")
                        .to_owned(),
                ),
                priority: candidate["priority"].as_i64(),
                labels: Some(
                    serde_json::from_value(candidate["labels"].clone()).expect("validated"),
                ),
                attachments: None,
            },
        )
        .await
        .map_err(|err| crate::document_input::at_source(&args.input.file, err))?;
    if let Some(format) = ctx.output {
        format.print(&updated)?;
    } else if ctx.quiet {
        println!("{}", updated.id);
    } else {
        print_success(styles, &format!("Updated task {}", updated.id));
    }
    Ok(())
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

    #[tokio::test]
    async fn waiting_reasons_distinguish_missing_cap_and_pending_outcome() {
        let temp = tempfile::tempdir().unwrap();
        let paths = orch_infra::storage::Paths::new(temp.path());
        std::fs::create_dir_all(paths.root()).unwrap();
        let engine = orch_engine::build_light_with(
            temp.path(),
            orch_engine::EngineSeams {
                global_config_path: Some(paths.root().join("global.yml")),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut task = task(TaskStatus::Todo);
        task.labels = vec!["cap:freebsd".into()];
        assert_eq!(
            waiting_reason(&engine, &task, None)
                .await
                .unwrap()
                .as_deref(),
            Some("no online agent with cap freebsd")
        );
        task.assignee = Some("agt_worker@w1".into());
        assert_eq!(
            waiting_reason(&engine, &task, None)
                .await
                .unwrap()
                .as_deref(),
            Some("remote w1 offline")
        );
        task.status = TaskStatus::InProgress;
        assert_eq!(
            waiting_reason(&engine, &task, None)
                .await
                .unwrap()
                .as_deref(),
            Some("remote w1 offline (outcome pending)")
        );
        task.status = TaskStatus::Done;
        assert_eq!(waiting_reason(&engine, &task, None).await.unwrap(), None);
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
            waiting_reason(&engine, &task, Some(&remote))
                .await
                .unwrap()
                .as_deref(),
            Some("awaiting approval: orch task approve tsk_mirror on w1")
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

#[cfg(test)]
mod definition_tests {
    use super::*;
    use crate::document_input::MutationKind;
    use serde_json::json;
    #[test]
    fn definition_projection_requires_title_description_priority_and_replaces_labels() {
        let current = json!({"id":"tsk_1", "title":"old", "description":"body", "priority":2, "labels":["a","b"], "status":"todo"});
        let candidate =
            definition_candidate(&current, &json!({"labels":["c"]}), MutationKind::Patch).unwrap();
        assert_eq!(candidate["labels"], json!(["c"]));
        assert!(
            definition_candidate(&current, &json!({"priority":null}), MutationKind::Patch).is_err()
        );
        assert!(
            definition_candidate(&current, &json!({"status":"todo"}), MutationKind::Patch).is_err()
        );
        assert!(
            definition_candidate(
                &current,
                &json!({"title":"x", "description":"y"}),
                MutationKind::Update
            )
            .is_err()
        );
        let candidate = definition_candidate(
            &current,
            &json!({"title":"x", "description":"y", "priority":1}),
            MutationKind::Update,
        )
        .unwrap();
        assert_eq!(candidate["labels"], json!([]));
    }
}
