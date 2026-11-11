//! `orch goal`.
//!
//! Three things here are measured behaviour rather than taste:
//!
//! - `goal status` splits its failures across two exit codes — an unknown
//!   *status name* prints an error and exits **1**, while a known name that is
//!   not a legal transition comes out of the service as exit **2**;
//! - `goal delete --json` prints the **compact** `{"deleted":"…"}`;
//! - `goal show --json` is the goal object *spread* with `tasks` and
//!   `progress` appended, and `progress` is dropped entirely when there is no
//!   progress entry (`undefined`, not `null`).

use std::process::ExitCode;

use orch_core::error::Result;
use orch_domain::{CreateGoalInput, GOAL_STATUSES, Goal, GoalStatus};
use orch_engine::goal_service::UpdateGoalFields;
use orch_proto::ops::args;
use serde_json::{Value, json};

use crate::commands::rpc::Rpc;

use crate::context::CliContext;
use crate::output::{Styles, print_error, print_key_value, print_success, print_table};

/// Goal status icons. Hard-coded — unlike the task icons
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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    title: &str,
    description: Option<&str>,
    assignee: Option<&str>,
) -> Result<()> {
    let goal = engine
        .call::<args::GoalCreateArgs>(CreateGoalInput {
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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    status: Option<&str>,
) -> Result<()> {
    let goals = match status {
        // An empty `--status` is no filter at all rather than a filter
        // nothing matches.
        None | Some("") => engine.request(args::GoalListArgs { status: None }).await?,
        Some(raw) => match raw.parse::<GoalStatus>() {
            Ok(status) => {
                engine
                    .request(args::GoalListArgs {
                        status: Some(status),
                    })
                    .await?
            }
            // An unknown status is not rejected: it is compared by
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

pub async fn show(engine: &Rpc, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    let orch_proto::ops::results::GoalView {
        goal,
        tasks,
        progress,
    } = engine.call::<args::GoalGetArgs>(json!({"id": id})).await?;

    if let Some(format) = ctx.output {
        let mut merged = serde_json::to_value(&goal).expect("a goal serializes");
        let object = merged
            .as_object_mut()
            .expect("a goal serializes to an object");
        object.insert(
            "tasks".to_owned(),
            serde_json::to_value(&tasks).expect("tasks serialize"),
        );
        // The key is absent rather than null when there is no progress.
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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    raw_status: &str,
    force: bool,
) -> Result<ExitCode> {
    let Ok(status) = raw_status.parse::<GoalStatus>() else {
        print_error(
            styles,
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

    let goal = engine
        .call::<args::GoalStatusArgs>(json!({"id":id,"status":status,"force":force}))
        .await?;

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
    engine: &Rpc,
    ctx: &CliContext,
    styles: &Styles,
    id: &str,
    fields: UpdateGoalFields,
) -> Result<()> {
    let goal = engine
        .call::<args::GoalUpdateArgs>(json!({
            "id": id, "title": fields.title, "description": fields.description,
            "assignee": fields.assignee,
        }))
        .await?;

    if let Some(format) = ctx.output {
        format.print(&goal)?;
    } else if ctx.quiet {
        println!("{}", goal.id);
    } else {
        print_success(styles, &format!("Updated goal \"{}\"", goal.title));
    }
    Ok(())
}

pub async fn delete(engine: &Rpc, ctx: &CliContext, styles: &Styles, id: &str) -> Result<()> {
    engine
        .call::<args::GoalDeleteArgs>(json!({"id":id}))
        .await?;

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

#[cfg(test)]
mod rpc_tests {
    use super::*;
    use crate::commands::{context, msg, status, team};
    use crate::context::EnvFlags;
    use orch_client::{Client, Project};
    use orch_daemon::testing::DaemonHarness;
    use orch_proto::handshake::{Caller, CallerKind, SessionMode};

    async fn fixture() -> (DaemonHarness, Rpc, CliContext) {
        let harness = DaemonHarness::builder().start().await.unwrap();
        let client = Client::from_stream(
            Project::discover(harness.project_root(), None).unwrap(),
            harness.connect().await.unwrap().into_stream(),
            Caller {
                kind: CallerKind::Cli,
                agent_id: None,
                pid: None,
            },
            SessionMode::Normal,
        )
        .await
        .unwrap();
        let rpc = Rpc::from_client(harness.project_root().to_path_buf(), client);
        let ctx = CliContext::new(
            harness.project_root().to_path_buf(),
            None,
            true,
            true,
            false,
            EnvFlags::default(),
        );
        (harness, rpc, ctx)
    }

    fn exit_code(error: orch_core::Error) -> i32 {
        crate::commands::rpc::recover_client_error(&error)
            .map_or_else(|| error.exit_code(), orch_client::ClientError::exit_code)
    }

    async fn agent(rpc: &Rpc, name: &str) -> String {
        rpc.call::<args::AgentCreateArgs>(
            json!({"name":name,"adapter":"shell","command":"/usr/bin/true"}),
        )
        .await
        .unwrap()
        .agent
        .id
    }

    #[tokio::test]
    async fn goal_commands_round_trip_updates_transitions_and_coordinated_views() {
        let (harness, rpc, ctx) = fixture().await;
        let styles = Styles::plain();
        let owner = agent(&rpc, "owner").await;
        let title = "Ship 'quoted' Unicode 👩‍💻";
        add(
            &rpc,
            &ctx,
            &styles,
            title,
            Some("success criteria"),
            Some(&owner),
        )
        .await
        .unwrap();
        let goals = rpc
            .request(args::GoalListArgs { status: None })
            .await
            .unwrap();
        assert_eq!(goals.len(), 1);
        let id = &goals[0].id;
        assert_eq!(goals[0].title, title);
        update(
            &rpc,
            &ctx,
            &styles,
            id,
            UpdateGoalFields {
                title: Some("Changed".into()),
                assignee: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let task = rpc
            .call::<args::TaskCreateArgs>(json!({"title":"Linked task","goal_id":id}))
            .await
            .unwrap();
        context::set(
            &rpc,
            &ctx,
            &styles,
            &format!("{id}-progress"),
            "progress report",
            None,
        )
        .await
        .unwrap();
        let view = rpc
            .call::<args::GoalGetArgs>(json!({"id":id}))
            .await
            .unwrap();
        assert_eq!(view.goal.title, "Changed");
        assert_eq!(view.goal.assignee, None);
        assert_eq!(view.goal.description, "success criteria");
        assert_eq!(view.tasks[0].id, task.id);
        assert_eq!(view.progress.as_deref(), Some("progress report"));
        show(&rpc, &ctx, &styles, id).await.unwrap();
        assert_eq!(
            status(&rpc, &ctx, &styles, id, "unknown", false)
                .await
                .unwrap(),
            ExitCode::from(1)
        );
        assert_eq!(
            status(&rpc, &ctx, &styles, id, "paused", false)
                .await
                .unwrap(),
            ExitCode::SUCCESS
        );
        assert_eq!(
            rpc.call::<args::GoalGetArgs>(json!({"id":id}))
                .await
                .unwrap()
                .goal
                .status,
            GoalStatus::Paused
        );
        assert_eq!(
            exit_code(
                status(&rpc, &ctx, &styles, id, "paused", false)
                    .await
                    .unwrap_err()
            ),
            2
        );
        list(&rpc, &ctx, &styles, Some("unknown")).await.unwrap();
        list(&rpc, &ctx, &styles, Some("")).await.unwrap();
        delete(&rpc, &ctx, &styles, id).await.unwrap();
        assert!(
            rpc.request(args::GoalListArgs { status: None })
                .await
                .unwrap()
                .is_empty()
        );
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn team_commands_map_members_and_config_and_mutate_the_daemon_pool() {
        let (harness, rpc, ctx) = fixture().await;
        let styles = Styles::plain();
        let lead = agent(&rpc, "lead").await;
        let member = agent(&rpc, "member").await;
        let third = agent(&rpc, "third").await;
        team::create(
            &rpc,
            &ctx,
            &styles,
            team::TeamCreateArgs {
                name: "A team 👩‍💻",
                lead: &lead,
                members: Some(&format!(" {member} ")),
                description: Some("description"),
                no_auto_claim: true,
            },
        )
        .await
        .unwrap();
        let teams = rpc.request(args::TeamListArgs {}).await.unwrap();
        assert_eq!(teams.len(), 1);
        let id = &teams[0].id;
        assert_eq!(teams[0].members.len(), 2);
        assert!(!teams[0].config.auto_claim);
        assert_eq!(teams[0].description.as_deref(), Some("description"));
        team::join(&rpc, &styles, id, &third).await.unwrap();
        team::leave(&rpc, &styles, id, &third).await.unwrap();
        team::set_lead(&rpc, &styles, id, &member).await.unwrap();
        let task = rpc
            .call::<args::TaskCreateArgs>(json!({"title":"pool task"}))
            .await
            .unwrap();
        team::add_task(&rpc, &styles, id, &task.id).await.unwrap();
        let team = rpc
            .call::<args::TeamGetArgs>(json!({"id":id}))
            .await
            .unwrap();
        assert_eq!(team.lead_agent_id, member);
        assert_eq!(team.members.len(), 2);
        assert_eq!(team.task_pool, vec![task.id]);
        team::show(&rpc, &ctx, &styles, id).await.unwrap();
        team::list(&rpc, &ctx, &styles).await.unwrap();
        team::disband(&rpc, &styles, id).await.unwrap();
        assert_eq!(
            rpc.call::<args::TeamGetArgs>(json!({"id":id}))
                .await
                .unwrap()
                .status,
            orch_domain::TeamStatus::Disbanded
        );
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn messages_preserve_sender_defaults_replies_fanout_and_non_consuming_inbox() {
        let (harness, mut rpc, ctx) = fixture().await;
        let styles = Styles::plain();
        let caller = agent(&rpc, "caller").await;
        let recipient = agent(&rpc, "recipient").await;
        let disabled = agent(&rpc, "disabled").await;
        rpc.call::<args::AgentDisableArgs>(json!({"id":disabled}))
            .await
            .unwrap();
        rpc.agent_id = Some(caller.clone());
        msg::send(
            &rpc,
            &ctx,
            &styles,
            msg::MsgSendArgs {
                to_agent_id: &recipient,
                body: "body 👩‍💻",
                subject: Some("subject"),
                from: None,
                ttl_ms: Some(60000),
                reply_to: None,
            },
        )
        .await
        .unwrap();
        let inbox = rpc
            .call::<args::MessageInboxArgs>(json!({"agent_id":recipient}))
            .await
            .unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from_agent_id, caller);
        assert_eq!(inbox[0].body, "body 👩‍💻");
        assert!(inbox[0].expires_at.is_some());
        msg::inbox(&rpc, &ctx, &styles, &recipient).await.unwrap();
        let unchanged = rpc
            .call::<args::MessageInboxArgs>(json!({"agent_id":recipient}))
            .await
            .unwrap();
        assert_eq!(inbox, unchanged);
        msg::send(
            &rpc,
            &ctx,
            &styles,
            msg::MsgSendArgs {
                to_agent_id: &caller,
                body: "reply",
                subject: None,
                from: Some(&recipient),
                ttl_ms: None,
                reply_to: Some(&inbox[0].id),
            },
        )
        .await
        .unwrap();
        let reply = rpc
            .call::<args::MessageInboxArgs>(json!({"agent_id":caller}))
            .await
            .unwrap();
        assert_eq!(reply[0].from_agent_id, recipient);
        assert_eq!(reply[0].reply_to.as_deref(), Some(inbox[0].id.as_str()));
        msg::broadcast(
            &rpc,
            &ctx,
            &styles,
            msg::MsgBroadcastArgs {
                body: "broadcast",
                subject: None,
                from: None,
                team: None,
                ttl_ms: None,
            },
        )
        .await
        .unwrap();
        let all = rpc
            .request(args::MessageListArgs { agent_id: None })
            .await
            .unwrap();
        assert_eq!(all.len(), 3); // two direct messages and one enabled non-sender
        let broadcast = all
            .iter()
            .find(|message| message.body == "broadcast")
            .unwrap();
        assert_eq!(broadcast.to_agent_id.as_deref(), Some(recipient.as_str()));
        msg::list(&rpc, &ctx, &styles, Some(&recipient))
            .await
            .unwrap();
        msg::list(&rpc, &ctx, &styles, None).await.unwrap();
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn context_and_status_round_trip_without_local_state_access() {
        let (harness, rpc, ctx) = fixture().await;
        let styles = Styles::plain();
        context::get(&rpc, &ctx, &styles, "missing").await.unwrap();
        context::set(&rpc, &ctx, &styles, "key", "first", Some(60000))
            .await
            .unwrap();
        context::set(&rpc, &ctx, &styles, "key", "👩‍💻 updated", None)
            .await
            .unwrap();
        let entry = rpc
            .call::<args::ContextGetArgs>(json!({"key":"key"}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.value, "👩‍💻 updated");
        assert_eq!(entry.expires_at, None);
        assert_eq!(entry.ttl_ms, None);
        context::get(&rpc, &ctx, &styles, "key").await.unwrap();
        context::list(&rpc, &ctx, &styles).await.unwrap();
        assert_eq!(
            exit_code(
                context::set(&rpc, &ctx, &styles, "invalid-ttl", "v", Some(0))
                    .await
                    .unwrap_err()
            ),
            2
        );
        assert_eq!(
            rpc.request(args::ContextListArgs {}).await.unwrap().len(),
            1
        );
        status::run(&rpc, &ctx, &styles).await.unwrap();
        context::delete(&rpc, &ctx, &styles, "key").await.unwrap();
        assert!(
            rpc.request(args::ContextListArgs {})
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            add(&rpc, &ctx, &styles, &"x".repeat(8193), None, None)
                .await
                .unwrap_err()
                .exit_code(),
            2
        );
        assert!(
            rpc.request(args::GoalListArgs { status: None })
                .await
                .unwrap()
                .is_empty()
        );
        harness.shutdown().await.unwrap();
    }
}
