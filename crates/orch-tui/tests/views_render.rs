//! Counted rendering cases: sort orders, glyphs, caps, badges, the detail
//! panels, the header, onboarding and the overlays
//! (docs/parity/tui.md §1, §3.2, §6, §8.3, §8.4), numbered `v001..`.

mod common;

use common::Harness;
use orch_domain::{AgentLastError, AgentStatus, GoalStatus, RunningEntry, TaskProof, TaskStatus};
use orch_tui::backend::{FakeBackend, make_agent, make_goal, make_task, make_team};
use orch_tui::state::{ToastKind, ViewId};
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
    harness.assert_text("⚠ Error");
    harness.assert_text("CLI is not installed.");
    harness.assert_text("Install it with: npm i -g @anthropic-ai/claude-code");
    harness.assert_text("Diagnostics: orch doctor");
}

#[test]
fn v037_a_kind_without_a_doctor_hint_omits_the_doctor_line() {
    let mut harness = harness_for(ViewId::Agents, agent_with_error("process_crash", "killed"));
    harness.press(KeyCode::Enter);
    harness.assert_text("Agent process crashed.");
    harness.refute_text("Diagnostics: orch doctor");
}

#[test]
fn v038_the_unknown_kind_also_shows_the_raw_message() {
    let mut harness = harness_for(ViewId::Agents, agent_with_error("unknown", "raw detail"));
    harness.press(KeyCode::Enter);
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
    harness.assert_text("API key is invalid.");
    harness.refute_text("401 unauthorized");
}

#[test]
fn v040_the_error_panel_shows_a_relative_age() {
    let mut harness = harness_for(ViewId::Agents, agent_with_error("timeout", "slow"));
    harness.press(KeyCode::Enter);
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
    harness.assert_text("No description.");
}

#[test]
fn v044_the_task_detail_panel_lists_attachments_by_name() {
    let mut task = make_task("tsk_1", "With files", TaskStatus::Todo);
    task.attachments = Some(vec!["/tmp/shot.png".into()]);
    let mut harness = harness_for(ViewId::Tasks, FakeBackend::new().with_tasks(vec![task]));
    harness.press(KeyCode::Enter);
    harness.assert_text("attachments (1)");
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
    harness.assert_text("result");
    harness.assert_text("Implemented the thing");
    harness.assert_text("files 1 changed · feat/x");
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
    harness.assert_text("tasks (2)");
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
    harness.assert_text("progress");
    harness.assert_text("line four");
}

#[test]
fn v058_a_goal_without_a_description_says_so() {
    let mut harness = harness_for(
        ViewId::Goals,
        FakeBackend::new().with_goals(vec![make_goal("goal_1", "G", GoalStatus::Active)]),
    );
    harness.press(KeyCode::Enter);
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
    harness.assert_text("+/- resize │ M max");
}

#[test]
fn v061_a_maximized_panel_advertises_exit_max() {
    let mut harness = harness_for(
        ViewId::Tasks,
        FakeBackend::new().with_tasks(vec![make_task("tsk_1", "T", TaskStatus::Todo)]),
    );
    harness.press(KeyCode::Enter);
    harness.key('M');
    harness.assert_text("+/- exit max");
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
    harness.assert_text("CAPS:linux");
    harness.assert_text("WHERE:w1 offline");
}
