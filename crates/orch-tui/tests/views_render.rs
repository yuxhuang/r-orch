//! Counted rendering cases: sort orders, glyphs, caps, badges, the detail
//! panels, the header, onboarding and the overlays, numbered `v001..`.

mod common;

use common::Harness;
use orch_domain::{
    AgentLastError, AgentStatus, GoalStatus, RemotePhase, RemoteRunRef, RunningEntry, TaskProof,
    TaskStatus,
};
use orch_tui::backend::{FakeBackend, make_agent, make_goal, make_task, make_team};
use orch_tui::state::{AgentSection, Focus, GoalSection, TaskSection, ToastKind, ViewId};
use ratatui::crossterm::event::KeyCode;

fn harness_for(view: ViewId, backend: FakeBackend) -> Harness {
    let mut harness = Harness::with(backend);
    harness.app.state.view = view;
    harness
}

// ── Sort orders ────────────────────────────────────────────────────────────

#[test]
fn v001_tasks_render_in_status_rank_order() {
    let backend = FakeBackend::new().with_tasks(vec![
        make_task("tsk_1", "Done one", TaskStatus::Done),
        make_task("tsk_2", "Todo one", TaskStatus::Todo),
        make_task("tsk_3", "Running one", TaskStatus::InProgress),
    ]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    let text = harness.text();
    let running = text.find("Running one").unwrap();
    let todo = text.find("Todo one").unwrap();
    let done = text.find("Done one").unwrap();
    assert!(running < todo && todo < done, "{text}");
}

#[test]
fn v002_agents_render_grouped_by_team_then_status() {
    let backend = FakeBackend::new()
        .with_agents(vec![
            make_agent("agt_1", "solo", AgentStatus::Idle),
            make_agent("agt_2", "teamed", AgentStatus::Idle),
        ])
        .with_teams(vec![make_team("team_1", "core", "agt_2", &["agt_2"])]);
    let mut harness = harness_for(ViewId::Agents, backend);
    let text = harness.text();
    assert!(
        text.find("teamed").unwrap() < text.find("solo").unwrap(),
        "{text}"
    );
}

#[test]
fn v003_goals_render_in_status_rank_order() {
    let backend = FakeBackend::new().with_goals(vec![
        make_goal("goal_1", "Abandoned one", GoalStatus::Abandoned),
        make_goal("goal_2", "Active one", GoalStatus::Active),
    ]);
    let mut harness = harness_for(ViewId::Goals, backend);
    let text = harness.text();
    assert!(
        text.find("Active one").unwrap() < text.find("Abandoned one").unwrap(),
        "{text}"
    );
}

// ── Status glyphs ──────────────────────────────────────────────────────────

#[test]
fn v004_the_running_chip_uses_the_first_braille_spinner_frame() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Busy", TaskStatus::InProgress)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("⠋ RUN");
}

#[test]
fn v005_the_todo_chip_uses_an_open_circle() {
    let backend = FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Todo)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("○ TODO");
}

#[test]
fn v006_the_done_chip_uses_a_check() {
    let backend = FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Done)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("✓ DONE");
}

#[test]
fn v007_the_failed_chip_uses_a_cross() {
    let backend = FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Failed)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("✕ FAIL");
}

#[test]
fn v008_the_review_chip_uses_a_lozenge() {
    let backend = FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Review)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("◈ REVIEW");
}

#[test]
fn v009_the_cancelled_chip_renders_off() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Cancelled)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("─ OFF");
}

#[test]
fn v010_a_non_running_task_shows_an_em_dash_in_the_time_column() {
    let backend = FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Todo)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("—");
}

#[test]
fn v011_priority_one_renders_three_bangs() {
    let mut task = make_task("tsk_1", "Critical", TaskStatus::Todo);
    task.priority = 1;
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(vec![task]));
    harness.assert_text("!!!");
}

#[test]
fn v012_priority_two_renders_two_bangs() {
    let mut task = make_task("tsk_1", "High", TaskStatus::Todo);
    task.priority = 2;
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(vec![task]));
    let text = harness.text();
    assert!(text.contains("!!") && !text.contains("!!!"), "{text}");
}

#[test]
fn v013_the_agent_chips_render_their_status_glyphs() {
    let backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_1", "idle-one", AgentStatus::Idle),
        make_agent("agt_2", "run-one", AgentStatus::Running),
        make_agent("agt_3", "err-one", AgentStatus::Error),
        make_agent("agt_4", "off-one", AgentStatus::Disabled),
    ]);
    let mut harness = harness_for(ViewId::Agents, backend);
    harness.assert_text("⠋ ACTIVE");
    harness.assert_text("○ IDLE");
    harness.assert_text("✕ ERROR");
    harness.assert_text("○ OFF");
}

#[test]
fn v014_goal_status_chips_render_their_glyphs() {
    let backend = FakeBackend::new().with_goals(vec![
        make_goal("goal_1", "A", GoalStatus::Active),
        make_goal("goal_2", "P", GoalStatus::Paused),
        make_goal("goal_3", "D", GoalStatus::Achieved),
        make_goal("goal_4", "X", GoalStatus::Abandoned),
    ]);
    let mut harness = harness_for(ViewId::Goals, backend);
    harness.assert_text("◉ ACTIVE");
    harness.assert_text("‖ PAUSED");
    harness.assert_text("✓ DONE");
    harness.assert_text("✕ DROP");
}

// ── Caps and pseudo-rows ───────────────────────────────────────────────────

#[test]
fn v015_eleven_tasks_show_the_show_all_hint() {
    let tasks: Vec<_> = (0..11)
        .map(|i| make_task(&format!("tsk_{i}"), &format!("Task {i}"), TaskStatus::Todo))
        .collect();
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(tasks));
    harness.assert_text("Show all (1 more) — press S");
}

#[test]
fn v016_ten_tasks_do_not_show_the_show_all_hint() {
    let tasks: Vec<_> = (0..10)
        .map(|i| make_task(&format!("tsk_{i}"), &format!("Task {i}"), TaskStatus::Todo))
        .collect();
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(tasks));
    harness.refute_text("Show all");
}

#[test]
fn v017_the_tasks_tab_badge_appears_only_when_tasks_are_hidden() {
    let tasks: Vec<_> = (0..15)
        .map(|i| make_task(&format!("tsk_{i}"), &format!("Task {i}"), TaskStatus::Todo))
        .collect();
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(tasks));
    harness.assert_text("(15)");

    let tasks: Vec<_> = (0..8)
        .map(|i| make_task(&format!("tsk_{i}"), &format!("Task {i}"), TaskStatus::Todo))
        .collect();
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(tasks));
    harness.refute_text("(8)");
}

#[test]
fn v018_the_add_pseudo_rows_render_per_view() {
    // With no tasks at all the welcome screen replaces the list, so seed one.
    let backend = FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Seed", TaskStatus::Todo)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("+ add task...");
    harness.app.state.view = ViewId::Agents;
    harness.assert_text("+ add agent...");
    harness.app.state.view = ViewId::Goals;
    harness.assert_text("+ add goal...");
}

#[test]
fn v019_the_add_row_is_absent_without_the_capability() {
    let mut backend = FakeBackend::new();
    backend.can_create_task = false;
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.refute_text("+ add task...");
}

#[test]
fn v020_an_attachment_count_renders_on_the_row() {
    let mut task = make_task("tsk_1", "With files", TaskStatus::Todo);
    task.attachments = Some(vec!["/tmp/a.png".into(), "/tmp/b.png".into()]);
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(vec![task]));
    // The paperclip is a wide glyph, so the flattened buffer pads it.
    harness.assert_text("📎 2");
}

#[test]
fn v021_an_empty_attachment_list_renders_no_paperclip() {
    let mut task = make_task("tsk_1", "No files", TaskStatus::Todo);
    task.attachments = Some(vec![]);
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(vec![task]));
    harness.refute_text("📎");
}

// ── Goal ↔ task linking ────────────────────────────────────────────────────

fn linked_backend() -> FakeBackend {
    let mut linked = make_task("tsk_1", "Linked task", TaskStatus::Todo);
    linked.goal_id = Some("goal_1".into());
    let mut done = make_task("tsk_2", "Done linked", TaskStatus::Done);
    done.goal_id = Some("goal_1".into());
    let mut orphan = make_task("tsk_3", "Orphan", TaskStatus::Todo);
    orphan.goal_id = Some("goal_gone".into());
    FakeBackend::new()
        .with_tasks(vec![linked, done, orphan])
        .with_goals(vec![make_goal(
            "goal_1",
            "A very long goal title here",
            GoalStatus::Active,
        )])
}

#[test]
fn v022_a_resolvable_goal_id_renders_the_plus_circle_badge() {
    let mut harness = harness_for(ViewId::Tasks, linked_backend());
    harness.assert_text("⊕");
}

#[test]
fn v023_the_goal_badge_title_is_capped_at_thirteen_characters() {
    let mut harness = harness_for(ViewId::Tasks, linked_backend());
    // `capLine` keeps n-1 characters and appends the ellipsis.
    harness.assert_text("A very long …");
}

#[test]
fn v024_an_orphaned_goal_id_renders_no_badge_for_that_row() {
    let mut harness = harness_for(ViewId::Tasks, linked_backend());
    let text = harness.text();
    let orphan_line = text
        .lines()
        .find(|line| line.contains("Orphan"))
        .unwrap_or("");
    assert!(!orphan_line.contains('⊕'), "{orphan_line}");
}

#[test]
fn v025_the_goal_progress_bar_renders_filled_and_empty_cells() {
    let mut harness = harness_for(ViewId::Goals, linked_backend());
    harness.assert_text("█");
    harness.assert_text("░");
    harness.assert_text("1/2");
}

#[test]
fn v026_a_goal_with_no_linked_tasks_hides_the_progress_bar() {
    let backend =
        FakeBackend::new().with_goals(vec![make_goal("goal_1", "Unlinked", GoalStatus::Active)]);
    let mut harness = harness_for(ViewId::Goals, backend);
    harness.refute_text("0/0");
}

#[test]
fn v027_grouping_renders_an_uppercased_goal_header_with_a_percentage() {
    let mut harness = harness_for(ViewId::Tasks, linked_backend());
    harness.key('g');
    harness.assert_text("⊕ A VERY LONG GOAL TITLE HERE · 2 tasks · 50% done");
}

#[test]
fn v028_grouping_renders_the_ungrouped_header() {
    let mut harness = harness_for(ViewId::Tasks, linked_backend());
    harness.key('g');
    harness.assert_text("⊕ UNGROUPED · 1 task ");
}

#[test]
fn v029_a_single_task_group_uses_the_singular_noun() {
    let mut linked = make_task("tsk_1", "Only one", TaskStatus::Todo);
    linked.goal_id = Some("goal_1".into());
    let backend = FakeBackend::new()
        .with_tasks(vec![linked])
        .with_goals(vec![make_goal("goal_1", "Solo", GoalStatus::Active)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.key('g');
    harness.assert_text("⊕ SOLO · 1 task · 0% done");
}

#[test]
fn v030_a_fully_done_group_renders_one_hundred_percent() {
    let mut done = make_task("tsk_1", "Finished", TaskStatus::Done);
    done.goal_id = Some("goal_1".into());
    let backend = FakeBackend::new()
        .with_tasks(vec![done])
        .with_goals(vec![make_goal("goal_1", "Solo", GoalStatus::Active)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.key('g');
    harness.assert_text("· 100% done");
}

// ── Team sections ──────────────────────────────────────────────────────────

#[test]
fn v031_a_team_header_names_the_team_its_size_and_its_lead() {
    let backend = FakeBackend::new()
        .with_agents(vec![
            make_agent("agt_1", "lead-one", AgentStatus::Idle),
            make_agent("agt_2", "member", AgentStatus::Idle),
        ])
        .with_teams(vec![make_team(
            "team_1",
            "core",
            "agt_1",
            &["agt_1", "agt_2"],
        )]);
    let mut harness = harness_for(ViewId::Agents, backend);
    harness.assert_text("◈ CORE · 2 agents · ★ lead-one");
}

#[test]
fn v032_the_unassigned_header_renders_on_the_transition() {
    let backend = FakeBackend::new()
        .with_agents(vec![
            make_agent("agt_1", "teamed", AgentStatus::Idle),
            make_agent("agt_2", "solo", AgentStatus::Idle),
        ])
        .with_teams(vec![make_team("team_1", "core", "agt_1", &["agt_1"])]);
    let mut harness = harness_for(ViewId::Agents, backend);
    harness.assert_text("◇ UNASSIGNED · 1 agent ");
}

#[test]
fn v033_an_autonomous_agent_shows_the_loop_glyph() {
    let mut agent = make_agent("agt_1", "auto-one", AgentStatus::Idle);
    agent.autonomous = Some(true);
    let mut harness = harness_for(ViewId::Agents, FakeBackend::new().with_agents(vec![agent]));
    harness.assert_text("⟳");
}

// ── Error hints ────────────────────────────────────────────────────────────

fn agent_with_error(kind: &str, message: &str) -> FakeBackend {
    let mut agent = make_agent("agt_1", "broken", AgentStatus::Error);
    agent.last_error = Some(AgentLastError {
        message: message.into(),
        kind: kind.into(),
        timestamp: "2026-10-13T11:59:00.000Z".into(),
    });
    FakeBackend::new().with_agents(vec![agent])
}

#[test]
fn v034_the_row_shows_the_mapped_hint_not_the_raw_message() {
    let mut harness = harness_for(
        ViewId::Agents,
        agent_with_error("adapter_not_found", "spawn claude ENOENT"),
    );
    harness.assert_text("CLI is not installed.");
    harness.refute_text("spawn claude ENOENT");
}

#[test]
fn v035_an_unrecognised_kind_falls_back_to_the_raw_message() {
    let mut harness = harness_for(
        ViewId::Agents,
        agent_with_error("not_a_kind", "weird failure"),
    );
    harness.assert_text("weird failure");
}

#[test]
fn v036_the_error_panel_shows_the_hint_the_fix_and_the_doctor_line() {
    let mut harness = harness_for(
        ViewId::Agents,
        agent_with_error("adapter_not_found", "spawn claude ENOENT"),
    );
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::AgentDetail(AgentSection::Error));
    harness.assert_text("⚠ Error");
    harness.assert_text("CLI is not installed.");
    harness.assert_text("Install it with: npm i -g @anthropic-ai/claude-code");
    harness.assert_text("Diagnostics: orch doctor");
}

#[test]
fn v037_a_kind_without_a_doctor_hint_omits_the_doctor_line() {
    let mut harness = harness_for(ViewId::Agents, agent_with_error("process_crash", "killed"));
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::AgentDetail(AgentSection::Error));
    harness.assert_text("Agent process crashed.");
    harness.refute_text("Diagnostics: orch doctor");
}

#[test]
fn v038_the_unknown_kind_also_shows_the_raw_message() {
    let mut harness = harness_for(ViewId::Agents, agent_with_error("unknown", "raw detail"));
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::AgentDetail(AgentSection::Error));
    harness.assert_text("Unknown error.");
    harness.assert_text("raw detail");
}

#[test]
fn v039_auth_failed_hides_the_raw_message() {
    let mut harness = harness_for(
        ViewId::Agents,
        agent_with_error("auth_failed", "401 unauthorized"),
    );
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::AgentDetail(AgentSection::Error));
    harness.assert_text("API key is invalid.");
    harness.refute_text("401 unauthorized");
}

#[test]
fn v040_the_error_panel_shows_a_relative_age() {
    let mut harness = harness_for(ViewId::Agents, agent_with_error("timeout", "slow"));
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::AgentDetail(AgentSection::Error));
    harness.assert_text("ago");
}

#[test]
fn v041_an_agent_without_an_error_renders_no_panel() {
    let mut harness = harness_for(
        ViewId::Agents,
        FakeBackend::new().with_agents(vec![make_agent("agt_1", "fine", AgentStatus::Idle)]),
    );
    harness.press(KeyCode::Enter);
    harness.refute_text("⚠ Error");
}

// ── Detail panels ──────────────────────────────────────────────────────────

#[test]
fn v042_the_task_detail_panel_shows_status_priority_and_attempts() {
    let mut task = make_task("tsk_1", "Detailed", TaskStatus::Todo);
    task.priority = 1;
    task.attempts = 2;
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(vec![task]));
    harness.press(KeyCode::Enter);
    harness.assert_text("status");
    harness.assert_text("P1");
    harness.assert_text("2/3");
}

#[test]
fn v043_a_task_without_a_description_or_summary_says_so() {
    let mut harness = harness_for(
        ViewId::Tasks,
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Bare", TaskStatus::Todo)]),
    );
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Description));
    harness.assert_text("No description.");
}

#[test]
fn v044_the_task_detail_panel_lists_attachments_by_name() {
    let mut task = make_task("tsk_1", "With files", TaskStatus::Todo);
    task.attachments = Some(vec!["/tmp/shot.png".into()]);
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(vec![task]));
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Attachments));
    harness.assert_text("Attachments");
    harness.assert_text("/tmp/shot.png");
}

#[test]
fn v045_a_task_without_attachments_omits_the_section() {
    let mut harness = harness_for(
        ViewId::Tasks,
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Bare", TaskStatus::Todo)]),
    );
    harness.press(KeyCode::Enter);
    harness.refute_text("attachments");
}

#[test]
fn v046_the_task_detail_panel_shows_the_agent_summary_as_the_result() {
    let mut task = make_task("tsk_1", "Finished", TaskStatus::Done);
    task.proof = Some(TaskProof {
        agent_summary: Some("Implemented the thing".into()),
        files_changed: vec!["src/lib.rs".into()],
        branch: Some("feat/x".into()),
        ..TaskProof::default()
    });
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(vec![task]));
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Result));
    harness.assert_text("Result");
    harness.assert_text("Implemented the thing");
    harness
        .app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Files));
    harness.assert_text("Files (1 changed · feat/x)");
}

#[test]
fn v047_the_task_detail_panel_resolves_dependency_ids_to_titles() {
    let mut blocked = make_task("tsk_2", "Blocked", TaskStatus::Todo);
    blocked.depends_on = vec!["tsk_1".into()];
    let backend = FakeBackend::new().with_tasks(vec![
        make_task("tsk_1", "Blocker title", TaskStatus::Done),
        blocked,
    ]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.app.state.task_selected = 1;
    harness.press(KeyCode::Enter);
    harness.assert_text("Blocker title");
}

#[test]
fn v048_the_agent_detail_panel_shows_the_skills_row_when_present() {
    let mut agent = make_agent("agt_1", "skilled", AgentStatus::Idle);
    agent.config.skills = Some(vec!["testing".into(), "docs".into()]);
    let mut harness = harness_for(ViewId::Agents, FakeBackend::new().with_agents(vec![agent]));
    harness.press(KeyCode::Enter);
    harness.assert_text("skills");
    harness.assert_text("testing, docs");
}

#[test]
fn v049_an_empty_skills_list_omits_the_row() {
    let mut agent = make_agent("agt_1", "plain", AgentStatus::Idle);
    agent.config.skills = Some(vec![]);
    let mut harness = harness_for(ViewId::Agents, FakeBackend::new().with_agents(vec![agent]));
    harness.press(KeyCode::Enter);
    harness.refute_text("skills");
}

#[test]
fn v050_a_shell_agent_shows_its_command_instead_of_a_model() {
    let mut agent = make_agent("agt_1", "shelly", AgentStatus::Idle);
    agent.adapter = "shell".into();
    agent.config.command = Some("npm test".into());
    let mut harness = harness_for(ViewId::Agents, FakeBackend::new().with_agents(vec![agent]));
    harness.press(KeyCode::Enter);
    harness.assert_text("command");
    harness.assert_text("npm test");
    harness.refute_text("model");
}

#[test]
fn v051_an_agent_without_a_role_says_so() {
    let mut harness = harness_for(
        ViewId::Agents,
        FakeBackend::new().with_agents(vec![make_agent("agt_1", "plain", AgentStatus::Idle)]),
    );
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::AgentDetail(AgentSection::Role));
    harness.assert_text("No role description.");
}

#[test]
fn v052_the_goal_detail_panel_shows_the_id_status_and_assignee_name() {
    let mut goal = make_goal("goal_1", "Ship v2", GoalStatus::Active);
    goal.assignee = Some("agt_1".into());
    let backend = FakeBackend::new()
        .with_goals(vec![goal])
        .with_agents(vec![make_agent("agt_1", "alpha", AgentStatus::Idle)]);
    let mut harness = harness_for(ViewId::Goals, backend);
    harness.press(KeyCode::Enter);
    harness.assert_text("goal_1");
    harness.assert_text("ACTIVE");
    harness.assert_text("alpha");
}

#[test]
fn v053_the_goal_detail_panel_shows_created_as_a_date_only() {
    let mut harness = harness_for(
        ViewId::Goals,
        FakeBackend::new().with_goals(vec![make_goal("goal_1", "G", GoalStatus::Active)]),
    );
    harness.press(KeyCode::Enter);
    harness.assert_text("2026-10-13");
    harness.refute_text("2026-10-13T12:00:00.000Z");
}

#[test]
fn v054_the_updated_row_appears_only_when_it_differs_from_created() {
    let mut harness = harness_for(
        ViewId::Goals,
        FakeBackend::new().with_goals(vec![make_goal("goal_1", "G", GoalStatus::Active)]),
    );
    harness.press(KeyCode::Enter);
    harness.refute_text("Updated");

    let mut goal = make_goal("goal_2", "Changed", GoalStatus::Active);
    goal.updated_at = Some("2026-10-14T09:00:00.000Z".into());
    let mut harness = harness_for(ViewId::Goals, FakeBackend::new().with_goals(vec![goal]));
    harness.press(KeyCode::Enter);
    harness.assert_text("Updated");
    harness.assert_text("2026-10-14");
}

#[test]
fn v055_the_goal_detail_panel_lists_only_its_linked_tasks() {
    let mut harness = harness_for(ViewId::Goals, linked_backend());
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Tasks));
    harness.assert_text("Tasks");
    harness.assert_text("Linked task");
    harness.refute_text("Orphan");
}

#[test]
fn v056_the_goal_detail_panel_shows_the_per_status_breakdown() {
    let mut harness = harness_for(ViewId::Goals, linked_backend());
    harness.press(KeyCode::Enter);
    harness.assert_text("1 todo");
    harness.assert_text("1 done");
}

#[test]
fn v057_the_goal_detail_panel_renders_the_full_progress_report() {
    let mut backend = linked_backend();
    backend.goal_progress_report = "line one\nline two\nline three\nline four".into();
    let mut harness = harness_for(ViewId::Goals, backend);
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Progress));
    harness.assert_text("Progress");
    harness.assert_text("line four");
}

#[test]
fn v058_a_goal_without_a_description_says_so() {
    let mut harness = harness_for(
        ViewId::Goals,
        FakeBackend::new().with_goals(vec![make_goal("goal_1", "G", GoalStatus::Active)]),
    );
    harness.press(KeyCode::Enter);
    harness
        .app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Description));
    harness.assert_text("No description.");
}

#[test]
fn v059_the_goal_detail_header_is_prefixed_with_goal() {
    let mut harness = harness_for(
        ViewId::Goals,
        FakeBackend::new().with_goals(vec![make_goal("goal_1", "Ship v2", GoalStatus::Active)]),
    );
    harness.press(KeyCode::Enter);
    harness.assert_text("GOAL: Ship v2");
}

// ── Resize hints ───────────────────────────────────────────────────────────

#[test]
fn v060_the_detail_panel_advertises_the_resize_and_max_hints() {
    let mut harness = harness_for(
        ViewId::Tasks,
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Todo)]),
    );
    harness.press(KeyCode::Enter);
    harness.assert_text("Tab/Esc to list to act");
}

#[test]
fn v061_a_maximized_panel_advertises_exit_max() {
    let mut harness = harness_for(
        ViewId::Tasks,
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Todo)]),
    );
    harness.press(KeyCode::Enter);
    harness.key('M');
    harness.assert_text("M restores to act; Esc closes detail");
}

#[test]
fn v062_the_logs_view_shows_neither_resize_hint() {
    let mut harness = harness_for(ViewId::Logs, FakeBackend::new());
    let text = harness.text();
    assert!(!text.contains("+/- resize"), "{text}");
    assert!(!text.contains("+/- exit max"), "{text}");
}

// ── Header ─────────────────────────────────────────────────────────────────

#[test]
fn v063_the_header_shows_the_brand_version_and_project() {
    let mut backend = FakeBackend::new();
    backend.context.project_name = "demo-project".into();
    backend.context.version = "1.2.3".into();
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("ORCH 1.2.3");
    harness.assert_text("demo-project");
}

#[test]
fn v064_the_idle_mode_chip_renders_when_nothing_runs() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.assert_text("○ IDLE");
}

#[test]
fn v065_the_watching_chip_renders_in_watch_mode() {
    let mut backend = FakeBackend::new();
    backend.context.initial_watch_active = true;
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("● WATCHING");
}

#[test]
fn v066_the_observing_chip_renders_in_observer_mode() {
    let mut backend = FakeBackend::new();
    backend.context.observer_mode = true;
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("● OBSERVING");
}

#[test]
fn v067_an_empty_project_renders_the_no_tasks_chip() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.assert_text("NO TASKS");
}

#[test]
fn v068_status_chips_render_only_for_non_zero_counts() {
    let backend = FakeBackend::new().with_tasks(vec![
        make_task("tsk_1", "A", TaskStatus::Todo),
        make_task("tsk_2", "B", TaskStatus::Done),
    ]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("TODO 1");
    harness.assert_text("DONE 1");
    harness.refute_text("FAIL");
    harness.refute_text("NO TASKS");
}

#[test]
fn v069_the_token_chip_renders_once_tokens_are_counted() {
    let mut backend = FakeBackend::new();
    backend.state.stats.total_tokens.input = 1_500;
    backend.state.stats.total_tokens.output = 700;
    backend.state.stats.total_tokens.total = 2_200;
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("↑1.5k ↓700");
    harness.assert_text("Σ2.2k");
}

#[test]
fn v070_the_update_chip_renders_when_a_newer_version_is_known() {
    let mut backend = FakeBackend::new();
    backend.pending_update = Some("9.9.9".into());
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.app.tick();
    harness.assert_text("v9.9.9 INSTALLED — RESTART TO APPLY");
}

#[test]
fn v071_the_active_count_chip_renders_while_an_agent_runs() {
    let mut backend = FakeBackend::new();
    backend.state.running.insert(
        "run_1".into(),
        RunningEntry {
            remote: None,
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            task_id: "tsk_1".into(),
            pid: 42,
            started_at: "2026-10-13T11:59:00.000Z".into(),
            last_event_at: "2026-10-13T11:59:30.000Z".into(),
        },
    );
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("1 active");
}

#[test]
fn v072_the_tab_pills_render_the_active_tab_and_the_inactive_labels() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.assert_text(" T TASKS ");
    harness.assert_text("A agents");
    harness.assert_text("G goals");
    harness.assert_text("L actions");
}

// ── Onboarding ─────────────────────────────────────────────────────────────

#[test]
fn v073_the_welcome_screen_replaces_the_task_list_on_first_run() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.assert_text("◆ Welcome to Orch");
    harness.assert_text("Press N to create your first task");
}

#[test]
fn v074_the_task_created_nudge_renders_under_the_list() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "First", TaskStatus::Todo)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("◆ Press R to run task");
}

#[test]
fn v075_the_run_started_nudge_renders_without_a_hint() {
    let mut backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "First", TaskStatus::InProgress)]);
    backend.state.running.insert(
        "run_1".into(),
        RunningEntry {
            remote: None,
            run_id: "run_1".into(),
            agent_id: "agt_1".into(),
            task_id: "tsk_1".into(),
            pid: 1,
            started_at: "2026-10-13T11:59:00.000Z".into(),
            last_event_at: "2026-10-13T11:59:00.000Z".into(),
        },
    );
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.assert_text("◆ Agent is running your task...");
}

#[test]
fn v076_the_per_tab_onboarding_box_renders_for_an_empty_view() {
    let mut backend = FakeBackend::new();
    backend.state.onboarding_completed = Some(true);
    let mut harness = harness_for(ViewId::Agents, backend);
    harness.assert_text("Agents");
    harness.assert_text("AI workers that execute your tasks.");
    harness.assert_text("N new agent");
}

#[test]
fn v077_the_onboarding_box_collapses_to_a_nudge_at_one_or_two_items() {
    let mut backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_1", "one", AgentStatus::Idle),
        make_agent("agt_2", "two", AgentStatus::Idle),
    ]);
    backend.state.onboarding_completed = Some(true);
    let mut harness = harness_for(ViewId::Agents, backend);
    harness.assert_text("Add more agents to increase parallelism.");
    harness.refute_text("AI workers that execute your tasks.");
}

#[test]
fn v078_the_onboarding_box_disappears_at_three_items() {
    let mut backend = FakeBackend::new().with_agents(vec![
        make_agent("agt_1", "one", AgentStatus::Idle),
        make_agent("agt_2", "two", AgentStatus::Idle),
        make_agent("agt_3", "three", AgentStatus::Idle),
    ]);
    backend.state.onboarding_completed = Some(true);
    let mut harness = harness_for(ViewId::Agents, backend);
    harness.refute_text("Add more agents");
}

#[test]
fn v079_the_dropped_w_hint_is_never_advertised() {
    let mut backend = FakeBackend::new();
    backend.state.onboarding_completed = Some(true);
    let mut harness = harness_for(ViewId::Agents, backend);
    harness.refute_text("W start orchestrator");
}

// ── Toasts and banners ─────────────────────────────────────────────────────

#[test]
fn v080_a_done_toast_renders_its_icon_title_and_message() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    let now = harness.now();
    harness
        .app
        .state
        .push_toast(ToastKind::Done, "Ship it", "Task completed by alpha", now);
    harness.assert_text("✓ Ship it");
    harness.assert_text("Task completed by alpha");
}

#[test]
fn v081_only_the_first_two_queued_toasts_render() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    let now = harness.now();
    for i in 0..5 {
        harness
            .app
            .state
            .push_toast(ToastKind::Done, &format!("toast{i}"), "done", now);
    }
    harness.assert_text("toast0");
    harness.assert_text("toast1");
    harness.refute_text("toast2");
}

#[test]
fn v082_a_long_toast_title_is_capped_at_forty_characters() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    let now = harness.now();
    let title = "x".repeat(60);
    harness
        .app
        .state
        .push_toast(ToastKind::Failed, &title, "Task failed", now);
    let text = harness.text();
    assert!(text.contains(&format!("{}…", "x".repeat(39))), "{text}");
}

#[test]
fn v083_the_undo_banner_shows_the_entity_label_name_and_countdown() {
    let backend =
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "Doomed", TaskStatus::Todo)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.key('d');
    harness.assert_text("✗ Task \"Doomed\" — 5s │ Z undo");
}

#[test]
fn v084_the_undo_banner_shows_one_row_per_pending_deletion() {
    let backend = FakeBackend::new().with_tasks(vec![
        make_task("tsk_1", "First", TaskStatus::Todo),
        make_task("tsk_2", "Second", TaskStatus::Todo),
    ]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.key('d');
    harness.key('j');
    harness.key('d');
    harness.assert_text("First");
    harness.assert_text("Second");
    assert_eq!(harness.app.state.pending_deletions.len(), 2);
}

// ── Help overlay ───────────────────────────────────────────────────────────

#[test]
fn v085_the_help_overlay_renders_its_three_column_dividers() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.key('?');
    harness.assert_text("NAVIGATION");
    harness.assert_text("ACTIONS");
    harness.assert_text("COMMANDS");
}

#[test]
fn v086_the_help_overlay_lists_the_navigation_shortcuts() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.key('?');
    harness.assert_text("Navigate");
    harness.assert_text("Switch tabs");
    harness.assert_text("Detail view");
}

#[test]
fn v087_the_help_overlay_lists_the_action_shortcuts() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.key('?');
    harness.assert_text("New item");
    harness.assert_text("Undo delete");
    harness.assert_text("Autonomous");
}

#[test]
fn v088_the_help_overlay_lists_the_command_shortcuts() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.key('?');
    harness.assert_text("Command mode");
    harness.assert_text("Auto-dispatch");
    harness.assert_text("/quit");
}

#[test]
fn v089_the_help_overlay_shows_the_dismiss_footer() {
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new());
    harness.key('?');
    harness.assert_text("Press any key to dismiss");
}

// ── Layout guard ───────────────────────────────────────────────────────────

#[test]
fn v090_a_terminal_below_the_minimum_renders_the_guard() {
    let mut harness = Harness::sized(FakeBackend::new(), 30, 10);
    harness.assert_text("terminal too small");
}

#[test]
fn v091_a_terminal_at_the_minimum_renders_the_dashboard() {
    let mut harness = Harness::sized(FakeBackend::new(), 40, 14);
    harness.refute_text("terminal too small");
    harness.assert_text("ORCH");
}

#[test]
fn v092_the_logs_task_badge_renders_only_above_eighty_columns() {
    let backend = FakeBackend::new()
        .with_tasks(vec![make_task("tsk_1", "Badged task", TaskStatus::Todo)])
        .with_agents(vec![make_agent("agt_1", "alpha", AgentStatus::Idle)]);
    let mut wide = harness_for(ViewId::Logs, backend);
    wide.app
        .handle_event(&orch_domain::OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
        });
    wide.app.state.flush_messages(wide.now());
    wide.assert_text("#Badged task");

    let backend = FakeBackend::new()
        .with_tasks(vec![make_task("tsk_1", "Badged task", TaskStatus::Todo)])
        .with_agents(vec![make_agent("agt_1", "alpha", AgentStatus::Idle)]);
    let mut narrow = Harness::sized(backend, 60, 24);
    narrow.app.state.view = ViewId::Logs;
    narrow
        .app
        .handle_event(&orch_domain::OrchestratorEvent::TaskStatusChanged {
            task_id: "tsk_1".into(),
            from: TaskStatus::Todo,
            to: TaskStatus::InProgress,
        });
    narrow.app.state.flush_messages(narrow.now());
    narrow.refute_text("#Badged task");
}

#[test]
fn v093_the_activity_feed_renders_its_filter_suffix() {
    let backend = FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Todo)]);
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.app.execute_command("status");
    harness.app.state.flush_messages(harness.now());
    harness.assert_text("ACTIVITY");
    harness.assert_text("F:ALL");
}

#[test]
fn v094_an_empty_progress_report_renders_no_progress_section() {
    // The engine returns no report until one is recorded; that is not an error
    // and must not leave an empty `progress` divider behind (BACKEND.md).
    let mut backend = linked_backend();
    backend.goal_progress_report = String::new();
    let mut harness = harness_for(ViewId::Goals, backend);
    harness.press(KeyCode::Enter);
    harness.refute_text("progress");
}

#[test]
fn remote_agents_show_capabilities_location_and_offline_state() {
    let mut agent = make_agent("agt_1@w1", "worker@w1", AgentStatus::Offline);
    agent.capabilities = vec!["linux".into()];
    agent.remote = Some(orch_domain::RemoteRef {
        alias: "w1".into(),
        online: false,
    });
    let mut harness = harness_for(ViewId::Agents, FakeBackend::new().with_agents(vec![agent]));
    harness.assert_text("OFFLINE");
    harness.assert_text("linux");
    harness.assert_text("w1 offline");
}

/// The detail panel shows the phase, in the phase's own colour, and the one
/// thing the operator can do about it: the `orch task approve` that has to be
/// run on the worker.
#[test]
fn the_task_detail_shows_the_remote_phase_and_its_approval_hint() {
    let mut backend = FakeBackend::new().with_tasks(vec![make_task(
        "tsk_1",
        "Delegated",
        TaskStatus::InProgress,
    )]);
    backend.state.running.insert(
        "tsk_1".into(),
        RunningEntry {
            remote: Some(RemoteRunRef {
                phase: RemotePhase::Queued,
                worker_task_id: Some("tsk_mirror".into()),
                reason: Some(format!(
                    "{} run `orch task approve tsk_mirror` on this worker",
                    orch_domain::AWAITING_APPROVAL_REASON
                )),
                ..RemoteRunRef::new("2com", "run_1")
            }),
            run_id: "run_1".into(),
            agent_id: "agt_1@2com".into(),
            task_id: "tsk_1".into(),
            pid: 0,
            started_at: "2026-10-13T11:59:00.000Z".into(),
            last_event_at: "2026-10-13T11:59:30.000Z".into(),
        },
    );
    let mut harness = harness_for(ViewId::Tasks, backend);
    harness.press(KeyCode::Enter);
    harness.assert_text("queued @2com");
    harness.assert_text("awaiting approval: orch task approve tsk_mirror on 2com");
    harness.refute_text("in_progress");
}

/// A task dispatched to a peer is `in_progress` here from the moment it is
/// reserved, so the chip must come from the worker's phase and never claim
/// `RUN` before the worker says so.
#[test]
fn remote_task_chips_come_from_the_worker_phase() {
    let chip = |phase| {
        let mut backend = FakeBackend::new().with_tasks(vec![make_task(
            "tsk_1",
            "Delegated",
            TaskStatus::InProgress,
        )]);
        backend.state.running.insert(
            "tsk_1".into(),
            RunningEntry {
                remote: Some(RemoteRunRef {
                    phase,
                    ..RemoteRunRef::new("2com", "run_1")
                }),
                run_id: "run_1".into(),
                agent_id: "agt_1@2com".into(),
                task_id: "tsk_1".into(),
                pid: 0,
                started_at: "2026-10-13T11:59:00.000Z".into(),
                last_event_at: "2026-10-13T11:59:30.000Z".into(),
            },
        );
        let mut harness = harness_for(ViewId::Tasks, backend);
        harness
            .text()
            .lines()
            .find(|line| line.contains("Delegated"))
            .expect("the task row")
            .to_owned()
    };
    assert!(chip(RemotePhase::Dispatched).contains("→ SENT"));
    assert!(
        !chip(RemotePhase::Dispatched).contains("RUN"),
        "a dispatched task must never read as running"
    );
    assert!(chip(RemotePhase::Queued).contains("○ QUEUE"));
    assert!(chip(RemotePhase::Running).contains("⠋ RUN"));
    assert!(chip(RemotePhase::Unknown).contains("◈ RUN?"));
    // An outcome has arrived: the task's own status is the truth again.
    assert!(chip(RemotePhase::Terminal).contains("⠋ RUN"));
}

#[test]
fn v095_wizard_reserves_two_rows_before_banners_at_minimum_size() {
    use orch_tui::state::{Focus, InputMode};
    use orch_tui::wizard::{StepKind, Wizard, WizardKind, WizardStep};
    for (toasts, expected_height, expected_y) in [(0, 3, 11), (1, 3, 10), (2, 2, 9)] {
        let backend = FakeBackend::new().with_tasks(
            (0..8)
                .map(|i| make_task(&format!("t{i}"), "T", TaskStatus::Todo))
                .collect(),
        );
        let mut harness = Harness::sized(backend, 40, 14);
        harness.app.state.input_mode = InputMode::Wizard;
        harness.app.state.wizard = Some(Wizard::new(
            WizardKind::Task,
            "New task",
            vec![WizardStep::new(
                "description",
                "Description",
                StepKind::Textarea,
            )],
        ));
        for _ in 0..toasts {
            harness
                .app
                .state
                .push_toast(ToastKind::Done, "Notice", "done", harness.now());
        }
        harness.draw();
        let key = orch_tui::state::PaneKey {
            view: ViewId::Tasks,
            entity: None,
            section: Focus::MainList,
        };
        let metrics = harness
            .app
            .state
            .mouse
            .hits
            .pane_metrics(&key)
            .expect("main geometry published");
        assert_eq!(
            metrics.body,
            ratatui::layout::Rect::new(0, 7, 40, expected_height)
        );
        let label: String = (0..40)
            .map(|x| harness.terminal.backend().buffer()[(x, 6)].symbol())
            .collect();
        assert!(label.contains("Description"), "{label}");
        assert!(expected_y + 2 <= 13 - toasts);
    }
}

#[test]
fn v096_details_render_one_section_with_size_dependent_overview_pin() {
    for (width, height) in [(40, 14), (80, 24), (120, 40)] {
        let mut goal = make_goal("goal", "Pinned identity", GoalStatus::Active);
        goal.description = (0..40)
            .map(|n| format!("description-row-{n:02}\n"))
            .collect();
        let mut harness = Harness::sized(FakeBackend::new().with_goals(vec![goal]), width, height);
        harness.app.state.view = ViewId::Goals;
        harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
        harness
            .app
            .state
            .set_focus(Focus::GoalDetail(GoalSection::Description));
        harness.draw();
        let key = harness
            .app
            .state
            .pane_key(Focus::GoalDetail(GoalSection::Description));
        let metrics = harness
            .hits()
            .pane_metrics(&key)
            .expect("visible section geometry");
        assert!(metrics.body.height > 0);
        assert_eq!(
            metrics.items, 0,
            "read-only description has no selectable rows"
        );
        assert_eq!(metrics.display_rows, 41, "complete source rows retained");
        assert!(harness.text().contains("> Description"));
        let pin = harness.hits().zones().iter().any(|zone| {
            zone.rect.height == 2
                && zone.kind
                    == orch_tui::hit::ZoneKind::DetailSection(Focus::GoalDetail(
                        GoalSection::Overview,
                    ))
        });
        assert_eq!(pin, height > 14, "overview pin floor at {width}x{height}");
    }
}

#[test]
fn v097_contextual_help_preserves_exact_step_and_scrolls_at_minimum_size() {
    use orch_tui::state::{CopyHelpContext, InputMode};
    use orch_tui::wizard::{DraftKey, StepKind, Wizard, WizardKind, WizardStep};
    let mut harness = Harness::sized(FakeBackend::new(), 40, 14);
    let description = format!(
        "Start of full help. {} End of full help.",
        "wide description ".repeat(30)
    );
    let wizard = Wizard::new(
        WizardKind::Task,
        "Task",
        vec![
            WizardStep::new("description", "Description", StepKind::Textarea)
                .description(&description),
        ],
    );
    let draft = DraftKey::from_wizard(&wizard);
    harness.app.state.wizard = Some(wizard);
    harness.app.state.input_mode = InputMode::Wizard;
    let origin = harness
        .app
        .state
        .register_editor_origin(draft, "description".into())
        .expect("live origin");
    harness.app.state.copy_help = Some(CopyHelpContext {
        origin: origin.clone(),
        scroll_row: 0,
    });
    harness.app.state.show_help = true;
    let text = harness.text();
    assert!(text.contains("COPY HELP — Description"), "{text}");
    assert!(text.contains("Start of full help."), "{text}");
    assert!(
        text.contains("Esc/F1 closes; outside click closes"),
        "{text}"
    );
    harness.app.state.copy_help.as_mut().unwrap().scroll_row = usize::MAX;
    let text = harness.text();
    assert!(text.contains("maximized detail to the list."), "{text}");
    assert_eq!(harness.app.state.copy_help.as_ref().unwrap().origin, origin);
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().step_id(),
        "description"
    );
}

#[test]
fn v098_wrapped_detail_rows_publish_source_byte_anchors_beyond_u16() {
    let mut task = make_task("task", "Long document", TaskStatus::Todo);
    task.description = (0..8000)
        .map(|n| format!("line {n:05} and more text\n"))
        .collect();
    let mut harness = Harness::sized(FakeBackend::new().with_tasks(vec![task]), 80, 24);
    harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
    let focus = Focus::TaskDetail(TaskSection::Description);
    harness.app.state.set_focus(focus);
    let key = harness.app.state.pane_key(focus);
    harness.app.state.pane_states.push((
        key.clone(),
        orch_tui::navigation::ScrollState {
            selected: None,
            offset: 7000,
        },
    ));
    harness.draw();
    assert!(harness.text().contains("line 07000 and more text"));
    assert!(
        harness
            .hits()
            .source_anchors
            .iter()
            .any(|(_, _, byte)| *byte > u16::MAX as usize)
    );
    assert_eq!(
        harness.hits().pane_metrics(&key).unwrap().display_rows,
        8001
    );
}

#[test]
fn v099_wizard_main_metrics_follow_exact_banner_budget_at_ordinary_sizes() {
    use orch_tui::state::{InputMode, PaneKey};
    use orch_tui::wizard::{StepKind, Wizard, WizardKind, WizardStep};
    for (width, height) in [(80, 24), (120, 40)] {
        for toast_count in 0..=2 {
            let backend = FakeBackend::new().with_tasks(
                (0..20)
                    .map(|i| make_task(&format!("t{i}"), "T", TaskStatus::Todo))
                    .collect(),
            );
            let mut harness = Harness::sized(backend, width, height);
            harness.app.state.input_mode = InputMode::Wizard;
            harness.app.state.wizard = Some(Wizard::new(
                WizardKind::Task,
                "Create",
                vec![WizardStep::new(
                    "description",
                    "Description",
                    StepKind::Textarea,
                )],
            ));
            for _ in 0..toast_count {
                harness
                    .app
                    .state
                    .push_toast(ToastKind::Done, "Notice", "done", harness.now());
            }
            harness.draw();
            let key = PaneKey {
                view: ViewId::Tasks,
                entity: None,
                section: Focus::MainList,
            };
            let actual = harness.hits().pane_metrics(&key).unwrap().body;
            let baseline = orch_tui::layout::regions(
                width,
                height,
                ViewId::Tasks,
                harness.app.state.list_item_count(true),
                0,
                false,
            );
            let work = height - 8;
            let expected_main = baseline.main_h.min(work - toast_count - 2);
            assert_eq!(
                actual,
                ratatui::layout::Rect::new(0, 7, width, expected_main)
            );
        }
    }
}

#[test]
fn v100_compact_pickers_reveal_the_last_item_and_keep_confirm_cancel_clickable() {
    use orch_tui::hit::{VerbId, ZoneKind};
    for agent_picker in [false, true] {
        let agents = (0..30)
            .map(|i| make_agent(&format!("a{i}"), &format!("agent {i}"), AgentStatus::Idle))
            .collect();
        let mut harness = Harness::sized(FakeBackend::new().with_agents(agents), 40, 14);
        harness.app.state.view = ViewId::Logs;
        harness.app.state.show_agent_picker = agent_picker;
        harness.app.state.show_type_picker = !agent_picker;
        harness.app.state.picker_cursor = if agent_picker { 29 } else { 7 };
        harness.draw();
        let zones = harness.hits().zones();
        assert!(
            zones.iter().any(|zone| if agent_picker {
                matches!(zone.kind, ZoneKind::PickerRow { index: 29, .. })
            } else {
                matches!(zone.kind, ZoneKind::TypePickerRow { index: 7, .. })
            }),
            "last item must be visible"
        );
        for verb in [VerbId::PickerConfirm, VerbId::PickerCancel] {
            assert!(
                zones.iter().any(|zone| zone.kind == ZoneKind::Verb(verb)),
                "{verb:?} must remain clickable"
            );
        }
    }
}

#[test]
fn v101_detail_soft_wrap_anchors_preserve_whole_graphemes() {
    use unicode_segmentation::UnicodeSegmentation;
    let mut goal = make_goal("g", "Unicode", GoalStatus::Active);
    let body = format!("{}👩‍👩‍👧‍👦 {}", "a".repeat(38), "e\u{301}".repeat(50));
    goal.description = body.clone();
    let mut harness = Harness::sized(FakeBackend::new().with_goals(vec![goal]), 40, 14);
    harness.app.state.view = ViewId::Goals;
    harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
    harness.app.state.detail_maximized = true;
    harness
        .app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Description));
    harness.draw();
    let boundaries: Vec<_> = body
        .grapheme_indices(true)
        .map(|(byte, _)| byte)
        .chain(std::iter::once(body.len()))
        .collect();
    assert!(harness.hits().source_anchors.len() > 1);
    for (_, _, byte) in &harness.hits().source_anchors {
        assert!(boundaries.contains(byte), "anchor {byte} splits a grapheme");
    }
}

#[test]
fn v102_direct_field_feedback_uses_reserved_footer_without_opening_viewer() {
    use orch_tui::backend::{CopyOutcome, CopyResult, CopyTarget, CopyTicket};
    for (width, height) in [(40, 14), (80, 24)] {
        let mut task = make_task("t", "Copy source", TaskStatus::Todo);
        task.description = "Copy these exact bytes".into();
        let mut harness = Harness::sized(FakeBackend::new().with_tasks(vec![task]), width, height);
        harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
        harness
            .app
            .state
            .set_focus(Focus::TaskDetail(TaskSection::Description));
        let origin = harness
            .app
            .state
            .active_copy_origin()
            .expect("field origin");
        harness.app.state.register_copy_request(
            origin.clone(),
            CopyTarget::Auto,
            Ok(CopyTicket(7)),
            harness.now(),
        );
        harness.assert_text("Copy pending");
        harness.app.state.apply_copy_result(
            CopyResult {
                ticket: CopyTicket(7),
                outcome: CopyOutcome::NativeWritten,
                guidance: None,
            },
            harness.now(),
        );
        harness.assert_text("Copied on this host");
        assert!(harness.app.state.viewer.is_none());
        harness.app.state.dismiss_copy_feedback(&origin);
        harness.refute_text("Copied on this host");
    }
}

#[test]
fn v103_activity_reports_cumulative_retention_losses_at_minimum_width() {
    let mut harness = Harness::sized(FakeBackend::new(), 40, 14);
    harness.app.execute_command("status");
    harness.app.state.flush_messages(harness.now());
    harness.app.state.retained_evicted = 2;
    harness.app.state.retained_rejected = 3;
    harness.assert_text("ev2 rej3");
}

#[test]
fn v104_viewer_help_retains_the_full_capture_date_and_source_provenance() {
    use orch_tui::state::CopyHelpContext;
    use orch_tui::widgets::text_viewer::{
        Completeness, Representation, TextViewer, ViewerDocument, ViewerSource,
    };
    let mut harness = Harness::sized(FakeBackend::new(), 80, 24);
    let source = ViewerSource::Message(7);
    harness.app.state.viewer = Some(TextViewer::new(ViewerDocument {
        source: source.clone(),
        title: "Full source name".into(),
        body: "full bytes".into(),
        representation: Representation::Raw,
        raw_body: None,
        completeness: Completeness::Complete,
        original_bytes: 10,
        captured_at_ms: 0,
        initial_anchor: 0,
    }));
    let origin = harness
        .app
        .state
        .register_viewer_origin(source)
        .expect("viewer origin");
    harness.app.state.copy_help = Some(CopyHelpContext {
        origin,
        scroll_row: 0,
    });
    harness.app.state.show_help = true;
    harness.assert_text("1970-01-01T00:00:00.000Z");
    harness.assert_text("Full source name");
    harness.assert_text("10 original bytes");
}

#[test]
fn activity_header_drops_filter_to_preserve_losses_and_follow_label() {
    for (width, following, label) in [(54, true, "following"), (64, false, "paused · End follows")]
    {
        let mut h = Harness::sized(FakeBackend::new(), width, 24);
        h.app.execute_command("status");
        h.app.state.flush_messages(h.now());
        h.app.state.activity_follow_tail = following;
        h.app.state.retained_evicted = 2;
        h.app.state.retained_rejected = 3;
        h.draw();
        let (_, y) = h.find_text("ACTIVITY").unwrap();
        let header = h.line(y);
        assert!(header.contains(label), "{header}");
        assert!(header.contains("evicted2 rejected3"), "{header}");
        assert!(!header.contains("F:"), "{header}");
        assert!(!h.hits().zones().iter().any(|z| matches!(
            z.kind,
            orch_tui::hit::ZoneKind::Verb(orch_tui::hit::VerbId::CycleActivityFilter)
        ) && z.rect.width > 0));
    }
}

#[test]
fn activity_header_keeps_recovery_hint_with_three_digit_retention_counts() {
    for width in [54, 64] {
        for focused in [false, true] {
            for following in [false, true] {
                for (evicted, rejected) in [(184, 0), (236, 999)] {
                    let mut h = Harness::sized(FakeBackend::new(), width, 24);
                    h.app.execute_command("status");
                    h.app.state.flush_messages(h.now());
                    h.app.state.focus = if focused {
                        Focus::Activity
                    } else {
                        Focus::MainList
                    };
                    h.app.state.activity_follow_tail = following;
                    h.app.state.retained_evicted = evicted;
                    h.app.state.retained_rejected = rejected;
                    h.draw();
                    let (_, y) = h.find_text("ACTIVITY").unwrap();
                    let header = h.line(y);
                    let label = if following {
                        "following"
                    } else {
                        "paused · End follows"
                    };
                    assert!(
                        header.contains(label),
                        "width={width}, focused={focused}: {header}"
                    );
                    assert!(
                        header.contains(&format!("evicted{evicted} rejected{rejected}"))
                            || header.contains(&format!("ev{evicted} rej{rejected}")),
                        "{header}"
                    );
                }
            }
        }
    }
}

/// The detail rail's sections in screen order, read from the hit map.
fn rail_order(harness: &mut Harness) -> Vec<Focus> {
    harness.draw();
    let mut rail: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter_map(|zone| match zone.kind {
            orch_tui::hit::ZoneKind::DetailSection(section) if zone.rect.height == 1 => {
                Some((zone.rect.x, section))
            }
            _ => None,
        })
        .collect();
    rail.sort_by_key(|(x, _)| *x);
    rail.into_iter().map(|(_, section)| section).collect()
}

/// R2: moving focus between sections restyles the focused entry but never
/// moves it; the rail stays in `focus_ring()` order.
#[test]
fn detail_rail_order_is_stable_across_focus() {
    let mut harness = harness_for(ViewId::Goals, linked_backend());
    harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
    let ring: Vec<Focus> = harness
        .app
        .state
        .focus_ring()
        .into_iter()
        .filter(|f| matches!(f, Focus::GoalDetail(_)))
        .collect();
    assert_eq!(
        ring,
        [
            Focus::GoalDetail(GoalSection::Overview),
            Focus::GoalDetail(GoalSection::Description),
            Focus::GoalDetail(GoalSection::Tasks),
            Focus::GoalDetail(GoalSection::Activity),
        ]
    );
    for focus in ring.iter().copied() {
        harness.app.state.set_focus(focus);
        assert_eq!(rail_order(&mut harness), ring, "focused {focus:?}");
        let text = harness.text();
        let line = text
            .lines()
            .find(|l| l.contains("Overview │"))
            .expect("rail row");
        let marked = format!("> {}", orch_tui::views::detail::section_name(focus));
        assert_eq!(line.matches("> ").count(), 1, "{line}");
        assert!(line.contains(&marked), "{line}");
    }
    // At the minimum width the rail drops entries but never reorders them,
    // and the focused entry is always on it.
    let mut narrow = Harness::sized(linked_backend(), 40, 14);
    narrow.app.state.view = ViewId::Goals;
    narrow.app.state.detail = orch_tui::state::DetailPanel::Entity;
    for focus in ring.iter().copied() {
        narrow.app.state.set_focus(focus);
        let shown = rail_order(&mut narrow);
        assert!(shown.contains(&focus), "focused {focus:?}: {shown:?}");
        let mut expected = ring.iter().filter(|f| shown.contains(f));
        assert!(
            shown.iter().all(|f| expected.next() == Some(f)),
            "{shown:?} is not in ring order"
        );
    }
}

/// Messages for `tsk_1`: two from alpha, an error from beta, one system row.
fn activity_backend_and_messages() -> (FakeBackend, Vec<orch_tui::messages::Message>) {
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    let backend = FakeBackend::new()
        .with_tasks(vec![make_task("tsk_1", "Ship it", TaskStatus::InProgress)])
        .with_agents(vec![
            make_agent("alpha", "alpha", AgentStatus::Running),
            make_agent("beta", "beta", AgentStatus::Idle),
        ]);
    let task = |m: Message| m.with_task(Some("tsk_1".into()));
    let messages = vec![
        task(Message::new(
            1,
            880_000,
            "plan ok",
            Token::White,
            MsgType::Output,
        ))
        .with_agent(Some("alpha".into())),
        task(Message::new(
            2,
            900_000,
            "edit main",
            Token::Cyan,
            MsgType::Tool,
        ))
        .with_agent(Some("alpha".into())),
        task(Message::new(3, 940_000, "boom", Token::Red, MsgType::Error))
            .with_agent(Some("beta".into())),
        task(Message::new(
            4,
            1_000_000,
            "settled",
            Token::Green,
            MsgType::Lifecycle,
        )),
    ];
    (backend, messages)
}

/// The screen cells of the row whose zone matches `pick`, left to right.
fn row_cells(
    harness: &mut Harness,
    pick: impl Fn(&orch_tui::hit::Zone) -> bool,
) -> Vec<(String, ratatui::style::Style)> {
    harness.draw();
    let rect = harness
        .hits()
        .zones()
        .iter()
        .find(|zone| pick(zone))
        .map(|zone| zone.rect)
        .expect("row zone");
    let buffer = harness.terminal.backend().buffer();
    (rect.x..rect.right())
        .map(|x| {
            let cell = &buffer[(x, rect.y)];
            (cell.symbol().to_owned(), cell.style())
        })
        .collect()
}

/// R1: the task detail's Activity rows have the bottom feed's columns and
/// styling: marker, relative time, agent badge, icon, capped text, error and
/// group backgrounds.
#[test]
fn task_detail_activity_rows_match_feed() {
    use orch_tui::hit::{ZoneId, ZoneKind};
    use orch_tui::widgets::text_viewer::ViewerSource;
    let (backend, messages) = activity_backend_and_messages();
    let mut harness = Harness::sized(backend, 120, 40);
    harness.app.state.view = ViewId::Tasks;
    harness.app.state.messages = messages;
    harness.draw();

    let feed: Vec<_> = (1..=4)
        .map(|id| {
            row_cells(&mut harness, |zone| {
                zone.kind == ZoneKind::ActivityRow { message_id: id }
            })
        })
        .collect();

    harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
    harness
        .app
        .state
        .set_focus(Focus::TaskDetail(TaskSection::Activity));
    let detail: Vec<_> = (1..=4)
        .map(|id| {
            row_cells(&mut harness, |zone| {
                zone.id == ZoneId::ExpandText(ViewerSource::Message(id))
                    && zone.kind == ZoneKind::ExpandText
            })
        })
        .collect();

    for (id, (feed, detail)) in feed.iter().zip(&detail).enumerate() {
        // Time, badge, icon and the (short, uncapped) text: 17 + 10 cells.
        let width = 27;
        let feed_row: String = feed.iter().map(|(s, _)| s.as_str()).collect();
        let detail_row: String = detail.iter().map(|(s, _)| s.as_str()).collect();
        // The first cell is the accent bar, or `>` on the selected row.
        let from = usize::from(feed[0].0 == ">" || detail[0].0 == ">");
        assert_eq!(
            feed[from..width],
            detail[from..width],
            "message {}: feed {feed_row:?} detail {detail_row:?}",
            id + 1
        );
    }
    let first: String = detail[0].iter().map(|(s, _)| s.as_str()).collect();
    assert!(first.contains("2m") && first.contains("alpha"), "{first:?}");
    let second: String = detail[1].iter().map(|(s, _)| s.as_str()).collect();
    assert!(
        !second.contains("alpha"),
        "a group shows its badge once: {second:?}"
    );
}

/// `activity_backend_and_messages` plus a second task outside any goal, an
/// alpha message on it, a beta message with no task whose text names the goal
/// and task ids, an empty goal and an agent with no messages. Feed order:
/// 1 alpha, 2 alpha, 3 beta, 5 alpha, 6 beta, 4 system.
fn goal_agent_activity_harness() -> Harness {
    use orch_tui::messages::{Message, MsgType};
    use orch_tui::theme::Token;
    let mut member = make_task("tsk_1", "Ship it", TaskStatus::InProgress);
    member.goal_id = Some("g1".into());
    let backend = FakeBackend::new()
        .with_tasks(vec![
            member,
            make_task("tsk_2", "Elsewhere", TaskStatus::Todo),
        ])
        .with_goals(vec![
            make_goal("g1", "Launch", GoalStatus::Active),
            make_goal("g2", "Empty goal", GoalStatus::Active),
        ])
        .with_agents(vec![
            make_agent("alpha", "alpha", AgentStatus::Running),
            make_agent("beta", "beta", AgentStatus::Idle),
            make_agent("gamma", "gamma", AgentStatus::Idle),
        ]);
    let (_, mut messages) = activity_backend_and_messages();
    let settled = messages.pop().expect("message 4");
    messages.push(
        Message::new(5, 960_000, "other edit", Token::Cyan, MsgType::Tool)
            .with_task(Some("tsk_2".into()))
            .with_agent(Some("alpha".into())),
    );
    messages.push(
        Message::new(6, 970_000, "see g1 tsk_1", Token::White, MsgType::Info)
            .with_agent(Some("beta".into())),
    );
    messages.push(settled);
    let mut harness = Harness::sized(backend, 120, 40);
    harness.app.state.messages = messages;
    harness
}

fn select_goal(harness: &mut Harness, id: &str) {
    harness.app.state.view = ViewId::Goals;
    harness.app.state.goal_selected = (0..harness.app.state.goals.len())
        .find(|i| {
            harness.app.state.goal_selected = *i;
            harness
                .app
                .state
                .selected_goal()
                .is_some_and(|g| g.id == id)
        })
        .expect("goal row");
}

fn select_agent(harness: &mut Harness, id: &str) {
    harness.app.state.view = ViewId::Agents;
    harness.app.state.agent_selected = (0..harness.app.state.agents.len())
        .find(|i| {
            harness.app.state.agent_selected = *i;
            harness
                .app
                .state
                .selected_agent()
                .is_some_and(|a| a.id == id)
        })
        .expect("agent row");
}

/// Bottom-feed row cells for each message id, captured before a detail opens.
fn feed_rows(
    harness: &mut Harness,
    ids: &[u64],
) -> Vec<(u64, Vec<(String, ratatui::style::Style)>)> {
    use orch_tui::hit::ZoneKind;
    ids.iter()
        .map(|&id| {
            (
                id,
                row_cells(harness, |zone| {
                    zone.kind == ZoneKind::ActivityRow { message_id: id }
                }),
            )
        })
        .collect()
}

/// The detail Activity rows on screen: message id and cells, top to bottom.
fn detail_activity_screen(
    harness: &mut Harness,
) -> Vec<(u64, Vec<(String, ratatui::style::Style)>)> {
    use orch_tui::hit::{ZoneId, ZoneKind};
    use orch_tui::widgets::text_viewer::ViewerSource;
    harness.draw();
    let mut rows: Vec<_> = harness
        .hits()
        .zones()
        .iter()
        .filter(|zone| zone.kind == ZoneKind::ExpandText)
        .filter_map(|zone| match &zone.id {
            ZoneId::ExpandText(ViewerSource::Message(id)) => Some((*id, zone.rect)),
            _ => None,
        })
        .collect();
    rows.sort_by_key(|(_, rect)| rect.y);
    let buffer = harness.terminal.backend().buffer();
    rows.into_iter()
        .map(|(id, rect)| {
            let cells = (rect.x..rect.right())
                .map(|x| {
                    let cell = &buffer[(x, rect.y)];
                    (cell.symbol().to_owned(), cell.style())
                })
                .collect();
            (id, cells)
        })
        .collect()
}

fn joined(cells: &[(String, ratatui::style::Style)]) -> String {
    cells.iter().map(|(s, _)| s.as_str()).collect()
}

/// Time, badge, icon and short text match the feed cell for cell; the first
/// cell is the accent bar, or `>` on the selected row.
fn assert_matches_feed(
    feed: &[(String, ratatui::style::Style)],
    detail: &[(String, ratatui::style::Style)],
    id: u64,
) {
    let width = 27;
    let from = usize::from(feed[0].0 == ">" || detail[0].0 == ">");
    assert_eq!(
        feed[from..width],
        detail[from..width],
        "message {id}: feed {:?} detail {:?}",
        joined(feed),
        joined(detail)
    );
}

/// R1/R12: a goal's Activity lists the messages of its tasks (structured
/// task_id → goal_id only, never a text match), newest first, in the bottom
/// feed's row format; an empty goal says "No activity."; pausing names Home.
#[test]
fn goal_detail_activity_rows() {
    let mut harness = goal_agent_activity_harness();
    harness.app.state.view = ViewId::Tasks;
    let feed = feed_rows(&mut harness, &[3, 4]);

    select_goal(&mut harness, "g1");
    harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
    harness
        .app
        .state
        .set_focus(Focus::GoalDetail(GoalSection::Activity));
    let rows = detail_activity_screen(&mut harness);
    let ids: Vec<u64> = rows.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, vec![4, 3, 2, 1], "newest first, members only");
    for (id, feed) in &feed {
        let (_, detail) = rows.iter().find(|(r, _)| r == id).unwrap();
        assert_matches_feed(feed, detail, *id);
    }
    let first = joined(&rows[0].1);
    assert!(first.starts_with('>'), "following selects row 0: {first:?}");
    assert!(
        first.contains("now"),
        "the top system row shows its time: {first:?}"
    );
    let alpha_top = joined(&rows[2].1);
    let alpha_next = joined(&rows[3].1);
    assert!(alpha_top.contains("alpha"), "{alpha_top:?}");
    assert!(
        !alpha_next.contains("alpha"),
        "a group shows its badge once: {alpha_next:?}"
    );
    let text = harness.text();
    assert!(text.contains("Activity · following"), "{text}");
    assert!(!text.contains("see g1 tsk_1"), "no text matching: {text}");

    harness.press(KeyCode::Down);
    let text = harness.text();
    assert!(text.contains("paused · Home follows"), "{text}");
    harness.press(KeyCode::Home);
    assert!(harness.text().contains("Activity · following"));

    select_goal(&mut harness, "g2");
    let rows = detail_activity_screen(&mut harness);
    assert!(rows.is_empty(), "{rows:?}");
    harness.assert_text("No activity.");
}

/// R1/R12: an agent's Activity lists exactly its own messages across tasks,
/// newest first, in the bottom feed's row format; clicking a row selects it;
/// an agent with no messages says "No activity.".
#[test]
fn agent_detail_activity_rows() {
    let mut harness = goal_agent_activity_harness();
    harness.app.state.view = ViewId::Tasks;
    let feed = feed_rows(&mut harness, &[5]);

    select_agent(&mut harness, "alpha");
    harness.app.state.detail = orch_tui::state::DetailPanel::Entity;
    harness
        .app
        .state
        .set_focus(Focus::AgentDetail(AgentSection::Activity));
    let rows = detail_activity_screen(&mut harness);
    let ids: Vec<u64> = rows.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, vec![5, 2, 1], "newest first, agent_id only");
    assert_matches_feed(&feed[0].1, &rows[0].1, 5);
    // One agent, one group: the badge shows once, on the newest row.
    for (_, cells) in &rows[1..] {
        let row = joined(cells);
        assert!(!row.contains("alpha"), "{row:?}");
    }

    // Rows are selectable items: a click pauses on the clicked message.
    let (x, y) = harness.find_text("plan ok").expect("row on screen");
    harness.click(x, y);
    let key = harness
        .app
        .state
        .pane_key(Focus::AgentDetail(AgentSection::Activity));
    assert_eq!(
        harness.app.state.detail_activity_scroll(&key, 10).selected,
        Some(2)
    );
    assert!(harness.text().contains("paused · Home follows"));

    select_agent(&mut harness, "gamma");
    let rows = detail_activity_screen(&mut harness);
    assert!(rows.is_empty(), "{rows:?}");
    harness.assert_text("No activity.");
}
