//! The command-bar **command surface**: every verb and subcommand in
//! `COMMAND_REGISTRY`, their aliases, their confirmation/refusal strings, the
//! command-bar keys (`/`, Tab, history, Enter-on-highlighted-sub), and the
//! rendered command-bar frame.
//!
//! `src/command_bar.rs`'s own unit tests already cover `resolve_completion` /
//! `resolve_suggestions` / `CommandHistory` in isolation — this file does not
//! duplicate those pinned vectors. It covers the *execution* surface
//! (`App::execute_command`, `src/commands.rs`) and the command-mode key
//! handling in `src/keys.rs::handle_command_key`.
//!
//! **Numbering.** Cases are numbered `c001..` in one flat sequence, grouped by
//! comment banners in this order: legacy bare shortcuts, `/task`, `/agent`,
//! `/team`, `/goal`, the remaining top-level verbs, command-bar keys, rendered
//! frame, and the failure path.

mod common;

use common::Harness;
use orch_domain::{AgentStatus, GoalStatus, TaskStatus};
use orch_tui::backend::{
    Call, FakeBackend, LaunchContext, make_agent, make_goal, make_task, make_team,
};
use orch_tui::command_bar::resolve_suggestions;
use orch_tui::state::InputMode;
use orch_tui::wizard::WizardKind;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

// ── Fixtures ─────────────────────────────────────────────────────────────

fn with_task(status: TaskStatus) -> Harness {
    let backend = FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Fix the bug", status)]);
    Harness::with(backend)
}

fn with_agent(status: AgentStatus) -> Harness {
    let backend = FakeBackend::new().with_agents(vec![make_agent("agt_1", "alpha", status)]);
    Harness::with(backend)
}

fn with_goal(status: GoalStatus) -> Harness {
    let backend = FakeBackend::new().with_goals(vec![make_goal("goal_1", "Ship v2", status)]);
    Harness::with(backend)
}

fn shell_default_adapter() -> FakeBackend {
    FakeBackend::new().with_context(LaunchContext {
        default_adapter: "shell".into(),
        ..LaunchContext::default()
    })
}

// ── Legacy bare shortcuts ────────────────────────

#[test]
fn c001_bare_cancel_with_no_selection_refuses() {
    let mut harness = Harness::new();
    harness.run_command("cancel");
    harness.assert_message("No task selected");
}

#[test]
fn c002_bare_retry_with_no_selection_refuses() {
    let mut harness = Harness::new();
    harness.run_command("retry");
    harness.assert_message("No task selected");
}

#[test]
fn c003_bare_assign_with_no_selection_refuses_before_usage() {
    let mut harness = Harness::new();
    harness.run_command("assign someone");
    harness.assert_message("No task selected");
}

#[test]
fn c004_bare_assign_with_selection_but_no_agent_shows_usage() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("assign");
    harness.assert_message("Usage: assign <agent>");
    harness.refute_message("✓ Assigned");
    assert!(harness.calls().is_empty());
}

#[test]
fn c005_bare_assign_with_selection_and_agent_assigns() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("assign bob");
    harness.assert_called(|c| {
        matches!(c, Call::AssignTask { task_id, agent_id } if task_id == "tsk_1" && agent_id == "bob")
    });
    harness.assert_message("✓ Assigned \"Fix the bug\" to bob");
}

#[test]
fn c006_bare_disable_with_no_selection_refuses() {
    let mut harness = Harness::new();
    harness.run_command("disable");
    harness.assert_message("No agent selected");
}

#[test]
fn c007_bare_enable_with_no_selection_refuses() {
    let mut harness = Harness::new();
    harness.run_command("enable");
    harness.assert_message("No agent selected");
}

#[test]
fn c008_bare_cancel_with_selection_cancels() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("cancel");
    harness.assert_called(|c| matches!(c, Call::CancelTask(id) if id == "tsk_1"));
    harness.assert_message("✓ Cancelled \"Fix the bug\"");
}

#[test]
fn c009_bare_retry_with_selection_retries() {
    let mut harness = with_task(TaskStatus::Failed);
    harness.run_command("retry");
    harness.assert_called(|c| matches!(c, Call::RetryTask(id) if id == "tsk_1"));
    harness.assert_message("✓ Retried \"Fix the bug\"");
}

#[test]
fn c010_bare_disable_with_selection_disables() {
    let mut harness = with_agent(AgentStatus::Idle);
    harness.run_command("disable");
    harness.assert_called(|c| matches!(c, Call::DisableAgent(id) if id == "agt_1"));
    harness.assert_message("✓ Disabled alpha");
}

#[test]
fn c011_bare_enable_with_selection_enables() {
    let mut harness = with_agent(AgentStatus::Disabled);
    harness.run_command("enable");
    harness.assert_called(|c| matches!(c, Call::EnableAgent(id) if id == "agt_1"));
    harness.assert_message("✓ Enabled alpha");
}

// ── /task ────────────────────────────────────────────────────────────────

#[test]
fn c012_task_add_with_no_title_opens_the_wizard() {
    let mut harness = Harness::new();
    harness.run_command("/task add");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.kind, WizardKind::Task);
    assert_eq!(wizard.title, "NEW TASK");
}

#[test]
fn c013_task_add_with_a_title_creates_it() {
    let mut harness = Harness::new();
    harness.run_command("/task add Fix the bug");
    harness
        .assert_called(|c| matches!(c, Call::CreateTask { title, .. } if title == "Fix the bug"));
    harness.assert_message("✓ Created \"Fix the bug\" (");
}

#[test]
fn c014_task_add_refuses_when_create_is_unavailable() {
    let mut backend = FakeBackend::new();
    backend.can_create_task = false;
    let mut harness = Harness::with(backend);
    harness.run_command("/task add Fix the bug");
    harness.assert_message("Create not available");
}

#[test]
fn c015_task_list_empty_says_no_tasks() {
    let mut harness = Harness::new();
    harness.run_command("/task list");
    harness.assert_message("No tasks");
}

#[test]
fn c016_task_list_formats_each_row() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task list");
    harness.assert_message("  tsk_1  todo        Fix the bug");
}

#[test]
fn c017_task_show_with_no_id_and_no_selection_refuses() {
    let mut harness = Harness::new();
    harness.run_command("/task show");
    harness.assert_message("No task selected or id given");
}

#[test]
fn c018_task_show_formats_id_status_priority_and_title() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task show");
    harness.assert_message("tsk_1  todo  P3  \"Fix the bug\"");
}

#[test]
fn c019_task_show_includes_the_assignee_line_when_assigned() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.app.state.tasks[0].assignee = Some("alpha".into());
    harness.run_command("/task show");
    harness.assert_message("agent: alpha");
}

#[test]
fn c020_task_cancel_resolves_an_explicit_id() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task cancel tsk_1");
    harness.assert_called(|c| matches!(c, Call::CancelTask(id) if id == "tsk_1"));
}

#[test]
fn c021_task_cancel_falls_back_to_the_selection_for_an_unknown_id() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task cancel nonexistent");
    harness.assert_called(|c| matches!(c, Call::CancelTask(id) if id == "tsk_1"));
}

#[test]
fn c022_task_retry_resolves_an_explicit_id() {
    let mut harness = with_task(TaskStatus::Failed);
    harness.run_command("/task retry tsk_1");
    harness.assert_called(|c| matches!(c, Call::RetryTask(id) if id == "tsk_1"));
}

#[test]
fn c023_task_assign_with_a_known_id_takes_the_agent_from_the_next_word() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task assign tsk_1 bob");
    harness.assert_called(|c| {
        matches!(c, Call::AssignTask { task_id, agent_id } if task_id == "tsk_1" && agent_id == "bob")
    });
}

#[test]
fn c024_task_assign_without_a_known_id_treats_the_word_as_the_agent() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task assign bob");
    harness.assert_called(|c| {
        matches!(c, Call::AssignTask { task_id, agent_id } if task_id == "tsk_1" && agent_id == "bob")
    });
}

#[test]
fn c025_task_assign_with_no_agent_shows_usage() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task assign");
    harness.assert_message("Usage: /task assign [id] <agent>");
}

#[test]
fn c026_task_approve_requires_review_status() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task approve");
    harness.assert_message("Cannot approve — status is todo");
}

#[test]
fn c027_task_approve_on_a_review_task_approves() {
    let mut harness = with_task(TaskStatus::Review);
    harness.run_command("/task approve");
    harness.assert_called(|c| matches!(c, Call::ApproveTask(id) if id == "tsk_1"));
    harness.assert_message("✓ Approved \"Fix the bug\"");
}

#[test]
fn c028_task_reject_requires_review_status() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task reject");
    harness.assert_message("Cannot reject — status is todo");
}

#[test]
fn c029_task_reject_with_feedback_words() {
    let mut harness = with_task(TaskStatus::Review);
    harness.run_command("/task reject tsk_1 needs more tests");
    harness.assert_called(|c| {
        matches!(c, Call::RejectTask { id, feedback } if id == "tsk_1" && feedback.as_deref() == Some("needs more tests"))
    });
    harness.assert_message("✓ Rejected \"Fix the bug\" → todo");
}

#[test]
fn c030_task_reject_without_feedback_passes_none() {
    let mut harness = with_task(TaskStatus::Review);
    harness.run_command("/task reject");
    harness.assert_called(|c| matches!(c, Call::RejectTask { feedback, .. } if feedback.is_none()));
}

#[test]
fn c031_task_delete_refuses_a_running_task() {
    let mut harness = with_task(TaskStatus::InProgress);
    harness.run_command("/task delete");
    harness.assert_message("Cannot delete — task is running");
    harness.refute_called(|c| matches!(c, Call::DeleteTask(_)));
}

#[test]
fn c032_task_delete_schedules_a_soft_delete() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/task delete");
    harness.assert_message("will be deleted in 5s — press Z to undo");
}

#[test]
fn c033_task_unknown_sub_shows_usage_verbatim() {
    let mut harness = Harness::new();
    harness.run_command("/task bogus");
    harness.assert_message("Usage: /task add|list|show|cancel|retry|assign|approve|reject|delete");
}

// ── /agent ───────────────────────────────────────────────────────────────

#[test]
fn c034_agent_add_with_no_name_opens_the_wizard() {
    let mut harness = Harness::new();
    harness.run_command("/agent add");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.kind, WizardKind::Agent);
    assert_eq!(wizard.title, "NEW AGENT");
}

#[test]
fn c035_agent_add_with_a_non_shell_adapter_creates_directly() {
    let mut harness = Harness::new();
    harness.run_command("/agent add runner");
    harness.assert_called(|c| {
        matches!(c, Call::AddAgent { name, adapter, .. } if name == "runner" && adapter == "claude")
    });
    harness.assert_message("✓ Created agent \"runner\" (");
}

#[test]
fn c036_agent_add_shell_with_no_command_opens_a_prefilled_wizard_instead_of_creating() {
    let mut harness = Harness::new();
    harness.run_command("/agent add runner shell");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.kind, WizardKind::Agent);
    let name_step = wizard.all_steps().iter().find(|s| s.id == "name").unwrap();
    assert_eq!(name_step.default_value.resolve(wizard.values()), "runner");
    let adapter_step = wizard
        .all_steps()
        .iter()
        .find(|s| s.id == "adapter")
        .unwrap();
    assert_eq!(adapter_step.default_value.resolve(wizard.values()), "shell");
    harness.refute_called(|c| matches!(c, Call::AddAgent { .. }));
}

#[test]
fn c037_agent_add_runner_with_shell_default_adapter_also_opens_the_wizard() {
    // `/agent add runner` — no adapter word — with a shell default adapter
    // must open the prefilled wizard, not create a shell agent with no command.
    let mut harness = Harness::with(shell_default_adapter());
    harness.run_command("/agent add runner");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    harness.refute_called(|c| matches!(c, Call::AddAgent { .. }));
}

#[test]
fn c038_agent_add_shell_with_command_words_creates_with_the_joined_command() {
    let mut harness = Harness::new();
    harness.run_command("/agent add runner shell echo hi");
    harness.assert_called(|c| {
        matches!(
            c,
            Call::AddAgent { name, adapter, opts }
                if name == "runner"
                    && adapter == "shell"
                    && opts.command.as_deref() == Some("echo hi")
                    && opts.approval_policy.as_deref() == Some("auto")
        )
    });
    harness.assert_message("· echo hi");
}

#[test]
fn c039_agent_add_refuses_when_creation_is_unavailable() {
    let mut backend = FakeBackend::new();
    backend.can_add_agent = false;
    let mut harness = Harness::with(backend);
    harness.run_command("/agent add runner");
    harness.assert_message("Agent creation not available");
}

#[test]
fn c040_agent_list_empty_says_no_agents() {
    let mut harness = Harness::new();
    harness.run_command("/agent list");
    harness.assert_message("No agents");
}

#[test]
fn c041_agent_list_formats_each_row() {
    let mut harness = with_agent(AgentStatus::Idle);
    harness.run_command("/agent list");
    harness.assert_message("  agt_1  idle     alpha (claude)");
}

#[test]
fn c042_agent_disable_resolves_by_name() {
    let mut harness = with_agent(AgentStatus::Idle);
    harness.run_command("/agent disable alpha");
    harness.assert_called(|c| matches!(c, Call::DisableAgent(id) if id == "agt_1"));
}

#[test]
fn c043_agent_disable_resolves_by_id() {
    let mut harness = with_agent(AgentStatus::Idle);
    harness.run_command("/agent disable agt_1");
    harness.assert_called(|c| matches!(c, Call::DisableAgent(id) if id == "agt_1"));
}

#[test]
fn c044_agent_disable_with_no_selection_and_no_arg_refuses() {
    let mut harness = Harness::new();
    harness.run_command("/agent disable");
    harness.assert_message("No agent selected or id given");
}

#[test]
fn c045_agent_enable_resolves_by_name() {
    let mut harness = with_agent(AgentStatus::Disabled);
    harness.run_command("/agent enable alpha");
    harness.assert_called(|c| matches!(c, Call::EnableAgent(id) if id == "agt_1"));
}

#[test]
fn c046_agent_delete_refuses_a_running_agent() {
    let mut harness = with_agent(AgentStatus::Running);
    harness.run_command("/agent delete agt_1");
    harness.assert_message("Cannot delete — agent is running");
}

#[test]
fn c047_agent_delete_schedules_a_soft_delete() {
    let mut harness = with_agent(AgentStatus::Idle);
    harness.run_command("/agent delete agt_1");
    harness.assert_message("will be deleted in 5s — press Z to undo");
}

#[test]
fn c048_agent_remove_is_an_alias_for_delete() {
    let mut harness = with_agent(AgentStatus::Idle);
    harness.run_command("/agent remove agt_1");
    harness.assert_message("will be deleted in 5s — press Z to undo");
}

#[test]
fn c049_agent_autonomous_toggles_on() {
    let mut harness = with_agent(AgentStatus::Idle);
    harness.run_command("/agent autonomous agt_1");
    harness.assert_called(
        |c| matches!(c, Call::ToggleAutonomous { id, enabled } if id == "agt_1" && *enabled),
    );
    harness.assert_message("⟳ alpha autonomous ON");
}

#[test]
fn c050_agent_auto_alias_toggles_off() {
    let mut agent = make_agent("agt_1", "alpha", AgentStatus::Idle);
    agent.autonomous = Some(true);
    let mut harness = Harness::with(FakeBackend::new().with_agents(vec![agent]));
    harness.run_command("/agent auto agt_1");
    harness.assert_called(
        |c| matches!(c, Call::ToggleAutonomous { id, enabled } if id == "agt_1" && !*enabled),
    );
    harness.assert_message("⟳ alpha autonomous OFF");
}

#[test]
fn c051_agent_shop_opens_the_shop_wizard() {
    let mut harness = Harness::new();
    harness.run_command("/agent shop");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.kind, WizardKind::AgentShop);
    assert_eq!(wizard.title, "AGENT SHOP");
}

#[test]
fn c052_agent_shop_refuses_a_shell_default_adapter() {
    let mut harness = Harness::with(shell_default_adapter());
    harness.run_command("/agent shop");
    harness.assert_message("Agent Shop requires an AI provider. Press N to create a Shell agent.");
    assert_eq!(harness.app.state.input_mode, InputMode::None);
}

#[test]
fn c053_agent_unknown_sub_shows_usage_verbatim() {
    let mut harness = Harness::new();
    harness.run_command("/agent bogus");
    harness.assert_message("Usage: /agent add|list|disable|enable|delete|autonomous|shop");
}

// ── /team ────────────────────────────────────────────────────────────────

#[test]
fn c054_team_create_opens_the_team_wizard() {
    let mut harness = Harness::new();
    harness.run_command("/team create");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.kind, WizardKind::Team);
    assert_eq!(wizard.title, "NEW TEAM");
}

#[test]
fn c055_team_add_is_an_alias_for_create() {
    let mut harness = Harness::new();
    harness.run_command("/team add");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().kind,
        WizardKind::Team
    );
}

#[test]
fn c056_team_list_empty_says_no_teams() {
    let mut harness = Harness::new();
    harness.run_command("/team list");
    harness.assert_message("No teams");
}

#[test]
fn c057_team_list_formats_each_row() {
    let team = make_team("team_1", "Core", "agt_1", &["agt_1", "agt_2"]);
    let mut harness = Harness::with(FakeBackend::new().with_teams(vec![team]));
    harness.run_command("/team list");
    harness.assert_message("  team_1  active   Core (2 members)");
}

#[test]
fn c058_team_join_with_no_args_shows_usage() {
    let mut harness = Harness::new();
    harness.run_command("/team join");
    harness.assert_message("Usage: /team join <teamId> [agentId]");
}

#[test]
fn c059_team_join_defaults_the_agent_to_the_selection() {
    let team = make_team("team_1", "Core", "agt_1", &["agt_1"]);
    let agent = make_agent("agt_2", "beta", AgentStatus::Idle);
    let mut harness = Harness::with(
        FakeBackend::new()
            .with_teams(vec![team])
            .with_agents(vec![agent]),
    );
    harness.run_command("/team join team_1");
    harness.assert_called(|c| {
        matches!(c, Call::JoinTeam { team_id, agent_id } if team_id == "team_1" && agent_id == "agt_2")
    });
}

#[test]
fn c060_team_join_with_an_explicit_agent() {
    let team = make_team("team_1", "Core", "agt_1", &["agt_1"]);
    let mut harness = Harness::with(FakeBackend::new().with_teams(vec![team]));
    harness.run_command("/team join team_1 agt_9");
    harness.assert_called(|c| {
        matches!(c, Call::JoinTeam { team_id, agent_id } if team_id == "team_1" && agent_id == "agt_9")
    });
}

#[test]
fn c061_team_leave_with_no_args_shows_usage() {
    let mut harness = Harness::new();
    harness.run_command("/team leave");
    harness.assert_message("Usage: /team leave <teamId> [agentId]");
}

#[test]
fn c062_team_leave_with_explicit_ids() {
    let team = make_team("team_1", "Core", "agt_1", &["agt_1", "agt_2"]);
    let mut harness = Harness::with(FakeBackend::new().with_teams(vec![team]));
    harness.run_command("/team leave team_1 agt_2");
    harness.assert_called(|c| {
        matches!(c, Call::LeaveTeam { team_id, agent_id } if team_id == "team_1" && agent_id == "agt_2")
    });
}

#[test]
fn c063_team_disband_with_no_arg_shows_usage() {
    let mut harness = Harness::new();
    harness.run_command("/team disband");
    harness.assert_message("Usage: /team disband <teamId>");
}

#[test]
fn c064_team_disband_with_an_id() {
    let team = make_team("team_1", "Core", "agt_1", &["agt_1"]);
    let mut harness = Harness::with(FakeBackend::new().with_teams(vec![team]));
    harness.run_command("/team disband team_1");
    harness.assert_called(|c| matches!(c, Call::DisbandTeam(id) if id == "team_1"));
}

#[test]
fn c065_team_set_lead_with_no_args_shows_usage() {
    let mut harness = Harness::new();
    harness.run_command("/team set-lead");
    harness.assert_message("Usage: /team set-lead <teamId> <agentId>");
}

#[test]
fn c066_team_set_lead_with_both_ids() {
    let team = make_team("team_1", "Core", "agt_1", &["agt_1", "agt_2"]);
    let mut harness = Harness::with(FakeBackend::new().with_teams(vec![team]));
    harness.run_command("/team set-lead team_1 agt_2");
    harness.assert_called(|c| {
        matches!(c, Call::SetTeamLead { team_id, agent_id } if team_id == "team_1" && agent_id == "agt_2")
    });
}

#[test]
fn c067_team_unknown_sub_shows_usage_verbatim() {
    let mut harness = Harness::new();
    harness.run_command("/team bogus");
    harness.assert_message("Usage: /team create|list|join|leave|disband|set-lead");
}

// ── /goal ────────────────────────────────────────────────────────────────

#[test]
fn c068_goal_add_with_no_title_opens_the_wizard() {
    let mut harness = Harness::new();
    harness.run_command("/goal add");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.kind, WizardKind::Goal);
    assert_eq!(wizard.title, "New Goal");
}

#[test]
fn c069_goal_create_alias_with_a_title_creates_it() {
    let mut harness = Harness::new();
    harness.run_command("/goal create Ship v2 everywhere");
    harness.assert_called(
        |c| matches!(c, Call::CreateGoal(input) if input.title == "Ship v2 everywhere"),
    );
    harness.assert_message("✓ Created goal \"Ship v2 everywhere\" (");
}

#[test]
fn c070_goal_add_refuses_when_creation_is_unavailable() {
    let mut backend = FakeBackend::new();
    backend.can_create_goal = false;
    let mut harness = Harness::with(backend);
    harness.run_command("/goal add Ship v2");
    harness.assert_message("Goal creation not available");
}

#[test]
fn c071_goal_list_empty_says_no_goals() {
    let mut harness = Harness::new();
    harness.run_command("/goal list");
    harness.assert_message("No goals");
}

#[test]
fn c072_goal_list_formats_each_row() {
    let mut harness = with_goal(GoalStatus::Active);
    harness.run_command("/goal list");
    harness.assert_message("  goal_1  active   Ship v2");
}

#[test]
fn c073_goal_show_with_no_id_and_no_selection_refuses() {
    let mut harness = Harness::new();
    harness.run_command("/goal show");
    harness.assert_message("No goal selected or id given");
}

#[test]
fn c074_goal_show_formats_id_status_and_title() {
    let mut harness = with_goal(GoalStatus::Active);
    harness.run_command("/goal show");
    harness.assert_message("goal_1  active  \"Ship v2\"");
}

#[test]
fn c075_goal_status_with_an_explicit_id_updates() {
    let mut harness = with_goal(GoalStatus::Active);
    harness.run_command("/goal status goal_1 achieved");
    harness.assert_called(|c| {
        matches!(c, Call::UpdateGoalStatus { id, status, force } if id == "goal_1" && *status == GoalStatus::Achieved && !force)
    });
    harness.assert_message("Updating goal status to achieved...");
}

#[test]
fn c076_goal_status_invalid_value_shows_usage() {
    let mut harness = with_goal(GoalStatus::Active);
    harness.run_command("/goal status bogus");
    harness.assert_message("Usage: /goal status [id] <active|paused|achieved|abandoned>");
}

#[test]
fn c077_goal_delete_schedules_a_soft_delete() {
    let mut harness = with_goal(GoalStatus::Active);
    harness.run_command("/goal delete goal_1");
    harness.assert_message("will be deleted in 5s — press Z to undo");
}

#[test]
fn c078_goal_unknown_sub_shows_usage_verbatim() {
    let mut harness = Harness::new();
    harness.run_command("/goal bogus");
    harness.assert_message("Usage: /goal add|list|show|status|delete");
}

// ── run / run-all / watch / pause / status / config / help / quit ──────────

#[test]
fn c079_run_dispatches_the_selected_runnable_task() {
    let mut harness = with_task(TaskStatus::Todo);
    harness.run_command("/run");
    harness.assert_called(|c| matches!(c, Call::RunTask(id) if id == "tsk_1"));
    harness.assert_message("✓ Dispatched tsk_1");
}

#[test]
fn c080_run_refuses_a_non_runnable_explicit_id() {
    let mut harness = with_task(TaskStatus::Done);
    harness.run_command("/run tsk_1");
    harness.assert_message("Cannot run — status is done");
    harness.refute_called(|c| matches!(c, Call::RunTask(_)));
}

#[test]
fn c081_run_with_no_id_and_no_selection_refuses() {
    let mut harness = Harness::new();
    harness.run_command("/run");
    harness.assert_message("No task selected or id given");
    assert!(harness.calls().is_empty());
}

#[test]
fn c082_run_all_dispatches_every_todo_task() {
    let mut harness = Harness::new();
    harness.run_command("/run-all");
    harness.assert_called(|c| matches!(c, Call::RunAll));
    harness.assert_message("✓ Dispatched all todo tasks");
}

#[test]
fn c083_watch_starts_when_not_active() {
    let mut harness = Harness::new();
    harness.run_command("/watch");
    harness.assert_called(|c| matches!(c, Call::StartWatch));
    harness.assert_message("✓ Watch mode started");
    assert!(harness.app.state.watch_active);
}

#[test]
fn c084_watch_refuses_when_already_active() {
    let mut harness = Harness::new();
    harness.app.state.watch_active = true;
    harness.run_command("/watch");
    harness.assert_message("Watch mode already active");
    harness.refute_called(|c| matches!(c, Call::StartWatch));
}

#[test]
fn c085_pause_stops_when_active() {
    let mut harness = Harness::new();
    harness.app.state.watch_active = true;
    harness.run_command("/pause");
    harness.assert_called(|c| matches!(c, Call::StopWatch));
    harness.assert_message("✓ Watch mode paused");
    assert!(!harness.app.state.watch_active);
}

#[test]
fn c086_pause_refuses_when_not_active() {
    let mut harness = Harness::new();
    harness.run_command("/pause");
    harness.assert_message("Watch mode not active");
    harness.refute_called(|c| matches!(c, Call::StopWatch));
}

#[test]
fn c087_config_with_no_sub_lists_the_settings() {
    let mut harness = Harness::new();
    harness.run_command("/config");
    harness.assert_message(
        "Choose a setting: palette, activity-filter, max-concurrent, notifications-toast, notifications-bell",
    );
}

#[test]
fn c088_config_palette_applies_directly_with_no_wizard() {
    let mut harness = Harness::new();
    harness.run_command("/config palette ocean");
    harness.assert_called(|c| matches!(c, Call::SavePalette(orch_domain::TuiPaletteName::Ocean)));
    harness.assert_message("Palette: ocean");
    assert_eq!(harness.app.state.input_mode, InputMode::None);
}

#[test]
fn c089_config_palette_unknown_name_refuses() {
    let mut harness = Harness::new();
    harness.run_command("/config palette nope");
    harness.assert_message("Unknown palette: nope. Available: amber, ocean, forest, violet, light");
}

#[test]
fn c090_config_palette_with_no_value_opens_the_settings_wizard() {
    let mut harness = Harness::new();
    harness.run_command("/config palette");
    assert_eq!(harness.app.state.input_mode, InputMode::Wizard);
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.kind, WizardKind::Config);
    assert_eq!(wizard.title, "SETTINGS — PALETTE");
}

#[test]
fn c091_config_max_concurrent_with_no_value_titles_the_wizard_with_spaces() {
    let mut harness = Harness::new();
    harness.run_command("/config max-concurrent");
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.title, "SETTINGS — MAX CONCURRENT");
}

#[test]
fn c092_config_unknown_setting_refuses() {
    let mut harness = Harness::new();
    harness.run_command("/config bogus");
    harness.assert_message(
        "Unknown setting: bogus. Available: palette, activity-filter, max-concurrent, notifications-toast, notifications-bell",
    );
}

#[test]
fn c093_status_reports_mode_counts() {
    let mut harness = Harness::new();
    harness.run_command("/status");
    harness.assert_message("idle  0 running  0 tasks  0 agents");
}

#[test]
fn c094_help_lists_quit_watch_and_pause() {
    let mut harness = Harness::new();
    harness.run_command("/help");
    harness.assert_message("/quit  — Exit the TUI");
    harness.assert_message("/watch  — Start watch mode (auto-dispatch)");
    harness.assert_message("/pause  — Pause watch mode");
}

#[test]
fn c095_quit_sets_should_exit() {
    let mut harness = Harness::new();
    harness.run_command("/quit");
    assert!(harness.app.state.should_exit);
}

#[test]
fn c096_unknown_verb_refuses_with_a_help_pointer() {
    let mut harness = Harness::new();
    harness.run_command("/zzz");
    harness.assert_message("Unknown: zzz. Type /help for commands");
}

// ── Command-bar keys ───────────────────────

#[test]
fn c097_slash_seeds_the_buffer() {
    let mut harness = Harness::new();
    harness.key('/');
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(harness.app.state.command_bar.input.value(), "/");
}

#[test]
fn c098_typing_filters_the_suggestion_list() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.type_str("ta");
    let suggestions = resolve_suggestions(harness.app.state.command_bar.input.value());
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].cmd, "/task");
}

#[test]
fn c099_tab_completes_a_unique_prefix() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.type_str("ta");
    harness.press(KeyCode::Tab);
    assert_eq!(harness.app.state.command_bar.input.value(), "/task ");
}

#[test]
fn c100_ghost_completion_suffix_is_shown_for_help() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.type_str("he");
    // "he" + the resolved ghost suffix "lp" renders contiguously, immediately
    // followed by the block cursor — unique to the input line (the
    // suggestions panel never renders the block glyph).
    harness.assert_text("help█");
}

#[test]
fn c101_up_then_down_walks_history_then_clears() {
    let mut harness = Harness::new();
    // Two history entries via unknown verbs: a nonsense command still gets
    // pushed to history (only the "Unknown: ..." message differs). Unlike a
    // real verb, "alphacmd"/"betacmd" never resolve to a suggestion, so a
    // recalled entry does not itself divert the next ↑/↓ into suggestion
    // navigation.
    harness.key('/');
    harness.type_str("alphacmd");
    harness.press(KeyCode::Enter);
    harness.key('/');
    harness.type_str("betacmd");
    harness.press(KeyCode::Enter);

    harness.key('/');
    harness.type_str("zzz");
    harness.press(KeyCode::Up);
    assert_eq!(harness.app.state.command_bar.input.value(), "/betacmd");
    harness.press(KeyCode::Up);
    assert_eq!(harness.app.state.command_bar.input.value(), "/alphacmd");
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.command_bar.input.value(), "/betacmd");
    harness.press(KeyCode::Down);
    assert_eq!(harness.app.state.command_bar.input.value(), "");
}

#[test]
fn c102_enter_on_a_highlighted_verb_with_subcommands_fills_instead_of_executing() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.type_str("task");
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert_eq!(harness.app.state.command_bar.input.value(), "/task ");
    assert!(!harness.app.state.should_exit);
}

#[test]
fn c103_enter_with_an_empty_buffer_is_a_no_op() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.press(KeyCode::Backspace);
    assert_eq!(harness.app.state.command_bar.input.value(), "");
    harness.press(KeyCode::Enter);
    assert_eq!(harness.app.state.input_mode, InputMode::Command);
    assert!(harness.messages().is_empty());
}

#[test]
fn c104_esc_leaves_command_mode_and_clears_the_buffer() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.type_str("task");
    harness.press(KeyCode::Esc);
    assert_eq!(harness.app.state.input_mode, InputMode::None);
    assert_eq!(harness.app.state.command_bar.input.value(), "");
}

// ── Rendered command-bar frame ─────────────

#[test]
fn c105_suggestions_panel_shows_the_commands_label_and_cursor() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.assert_text("COMMANDS");
    harness.assert_text("▸");
}

#[test]
fn c106_hint_is_enter_exec_with_no_suggestions_visible() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.type_str("zzz");
    harness.assert_text("Enter exec");
    harness.refute_text("↑↓ select");
}

#[test]
fn c107_hint_is_arrow_select_with_suggestions_visible() {
    let mut harness = Harness::new();
    harness.key('/');
    harness.assert_text("↑↓ select");
    harness.refute_text("Enter exec");
}

#[test]
fn c108_key_release_events_are_ignored_in_command_mode_too() {
    // The release/repeat guard at the very top of `handle_key`
    // applies before the input-mode branch is even reached, so a
    // release event must not insert a character into the command buffer.
    let mut harness = Harness::new();
    harness.press_mod(KeyCode::Char('/'), KeyModifiers::NONE);
    assert_eq!(harness.app.state.command_bar.input.value(), "/");
    harness.release(KeyCode::Char('x'));
    assert_eq!(harness.app.state.command_bar.input.value(), "/");
}

// ── Failure path ────────────────────────────────────────────────────────

#[test]
fn c109_a_failing_backend_call_reports_failed_with_the_error() {
    let mut backend = FakeBackend::new();
    backend.fail_next = Some("boom".into());
    let mut harness = Harness::with(backend);
    harness.run_command("/run-all");
    harness.assert_message("Failed: boom");
}

#[test]
fn c110_only_one_leading_slash_is_stripped() {
    // `replace(/^\//, '')` — `//task` keeps its second slash and is unknown.
    let mut harness = Harness::new();
    harness.run_command("//task");
    harness.assert_message("Unknown: /task. Type /help for commands");
    harness.refute_message("Usage: /task add|list");
}

#[test]
fn c111_a_single_leading_slash_still_resolves_the_verb() {
    let mut harness = Harness::new();
    harness.run_command("/task");
    harness.assert_message("Usage: /task add|list|show|cancel|retry|assign|approve|reject|delete");
}

#[test]
fn c112_a_bare_verb_without_a_slash_still_resolves() {
    let mut harness = Harness::new();
    harness.run_command("status");
    harness.assert_message("0 running");
}
