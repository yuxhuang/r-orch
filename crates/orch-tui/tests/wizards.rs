//! Counted wizard-catalogue cases — one case per spec item, numbered
//! `w001..`, grouped to match the spec's own section order:
//!
//! 1. NEW AGENT (steps, skips, options, mappers)
//! 2. AGENT SHOP (templates)
//! 3. NEW AGENT — {template} / EDIT AGENT
//! 4. NEW TASK / EDIT TASK
//! 5. NEW TEAM
//! 6. New Goal / Edit Goal
//! 7. SETTINGS
//! 8. Validation catalogue + debounce/sync semantics
//! 9. Engine keys, wired through real catalogue steps
//! 10. End-to-end via the app harness
//!
//! plus a trailing section for §5.4 (model options), which the spec calls out
//! as its own normative subsection.
//!
//! Mix of direct unit-style assertions on the builders/mappers (fast, exact)
//! and harness-driven cases that push keys and read the drawn frame or the
//! recorded backend calls. Never sleeps; uses `wizard.poll(now_ms)` /
//! `harness.advance(ms)` with explicit timestamps.

mod common;

use common::Harness;
use orch_domain::{
    AGENT_SHOP_TEMPLATES, AgentStatus, TaskStatus, get_shop_template_by_key, resolve_model,
};
use orch_tui::backend::{
    Call, ModelCatalog, ModelOption, make_agent, make_goal, make_task, make_team,
};
use orch_tui::state::ViewId;
use orch_tui::wizard::configs::{
    self, AGENT_NAME_TAKEN, COMMAND_REQUIRED_FOR_SHELL, CUSTOM_ROLE, EFFORT_ADAPTERS,
    NAME_REQUIRED, PRIORITY_RANGE, TEAM_NAME_TAKEN, TITLE_REQUIRED,
};
use orch_tui::wizard::engine::{ENTER_BLOCK_HINT_MS, FIX_THE_ERROR_ABOVE, VALIDATION_DEBOUNCE_MS};
use orch_tui::wizard::{Choice, StepKind, Values, Wizard, WizardKind, WizardOutcome, WizardStep};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// ── Shared helpers ──────────────────────────────────────────────────────────

fn vals(pairs: &[(&str, &str)]) -> Values {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn empty_catalog() -> ModelCatalog {
    ModelCatalog::new()
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn step_ids(steps: &[WizardStep]) -> Vec<&str> {
    steps.iter().map(|s| s.id.as_str()).collect()
}

fn find<'a>(steps: &'a [WizardStep], id: &str) -> &'a WizardStep {
    steps
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("no step {id:?}"))
}

fn type_into(wizard: &mut Wizard, text: &str, now: i64) {
    for ch in text.chars() {
        wizard.handle_key(key(KeyCode::Char(ch)), now);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. NEW AGENT — getAgentWizardSteps
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn w001_new_agent_has_ten_steps_in_order() {
    let steps = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude");
    assert_eq!(
        step_ids(&steps),
        vec![
            "name",
            "adapter",
            "command",
            "model",
            "effort",
            "role",
            "role_custom",
            "skills",
            "approval_policy",
            "team",
        ]
    );
}

#[test]
fn w002_command_step_is_skipped_for_every_non_shell_adapter() {
    let steps = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude");
    let command = find(&steps, "command");
    for adapter in [
        "claude",
        "opencode",
        "codex",
        "cursor",
        "pi",
        "grok",
        "antigravity",
    ] {
        assert!(
            command.is_skipped(&vals(&[("adapter", adapter)])),
            "command must skip for {adapter}"
        );
    }
    assert!(!command.is_skipped(&vals(&[("adapter", "shell")])));
}

#[test]
fn w003_model_role_skills_team_skip_only_for_shell() {
    let teams = vec![make_team("team_1", "A", "agt_1", &["agt_1"])];
    let steps = configs::agent_wizard_steps(&[], &teams, &empty_catalog(), "claude");
    for id in ["model", "role", "skills"] {
        let step = find(&steps, id);
        assert!(
            step.is_skipped(&vals(&[("adapter", "shell")])),
            "{id} shell"
        );
        assert!(
            !step.is_skipped(&vals(&[("adapter", "claude")])),
            "{id} claude"
        );
    }
}

#[test]
fn w004_effort_skips_for_every_adapter_except_claude_pi_grok() {
    let steps = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude");
    let effort = find(&steps, "effort");
    for adapter in EFFORT_ADAPTERS {
        assert!(
            !effort.is_skipped(&vals(&[("adapter", adapter)])),
            "effort must show for {adapter}"
        );
    }
    for adapter in ["codex", "cursor", "opencode", "antigravity", "shell"] {
        assert!(
            effort.is_skipped(&vals(&[("adapter", adapter)])),
            "effort must skip for {adapter}"
        );
    }
}

#[test]
fn w005_approval_policy_is_always_skipped() {
    let steps = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude");
    let approval = find(&steps, "approval_policy");
    assert!(approval.is_skipped(&Values::new()));
    assert!(approval.is_skipped(&vals(&[("adapter", "claude")])));
    assert!(approval.is_skipped(&vals(&[("adapter", "shell")])));
}

#[test]
fn w006_team_step_skips_when_fewer_than_two_team_options() {
    let no_teams = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude");
    assert!(find(&no_teams, "team").is_skipped(&vals(&[("adapter", "claude")])));

    let one_team = vec![make_team("team_1", "Alpha", "agt_1", &["agt_1"])];
    let with_team = configs::agent_wizard_steps(&[], &one_team, &empty_catalog(), "claude");
    assert!(!find(&with_team, "team").is_skipped(&vals(&[("adapter", "claude")])));
    assert!(find(&with_team, "team").is_skipped(&vals(&[("adapter", "shell")])));
}

#[test]
fn w007_effort_sits_immediately_after_model() {
    let steps = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude");
    let ids = step_ids(&steps);
    let model_idx = ids.iter().position(|&id| id == "model").unwrap();
    assert_eq!(ids[model_idx + 1], "effort");
}

#[test]
fn w008_the_shell_path_collapses_to_exactly_three_active_steps() {
    let teams = vec![make_team("team_1", "A", "agt_1", &["agt_1"])];
    let steps = configs::agent_wizard_steps(&[], &teams, &empty_catalog(), "claude");
    let shell_values = vals(&[("adapter", "shell")]);
    let active: Vec<&str> = steps
        .iter()
        .filter(|s| !s.is_skipped(&shell_values))
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(active, vec!["name", "adapter", "command"]);
}

#[test]
fn w009_adapter_options_are_in_order_with_shell_eighth() {
    let options = configs::adapter_options();
    assert_eq!(
        options,
        vec![
            Choice::new("claude", "Claude", "Claude Code CLI"),
            Choice::new("opencode", "OpenCode", "OpenCode — multi-provider"),
            Choice::new("codex", "Codex", "OpenAI Codex CLI"),
            Choice::new("cursor", "Cursor", "Cursor Agent CLI"),
            Choice::new("pi", "Pi", "Pi coding agent RPC"),
            Choice::new("grok", "Grok", "Grok CLI"),
            Choice::new("antigravity", "Antigravity", "Google Antigravity CLI (agy)"),
            Choice::new("shell", "Shell", "custom shell command"),
        ]
    );
    assert_eq!(options.iter().position(|o| o.value == "shell"), Some(7));
}

#[test]
fn w010_seven_downs_on_an_isolated_adapter_step_reach_shell() {
    let adapter_step = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude")
        .into_iter()
        .find(|s| s.id == "adapter")
        .unwrap();
    let mut wizard = Wizard::new(WizardKind::Agent, "T", vec![adapter_step]);
    for _ in 0..7 {
        wizard.handle_key(key(KeyCode::Char('j')), 0);
    }
    assert_eq!(wizard.select_index(), 7);
    match wizard.handle_key(key(KeyCode::Enter), 0) {
        WizardOutcome::Complete(values) => assert_eq!(values["adapter"], "shell"),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn w011_every_effort_option_value_label_and_hint() {
    assert_eq!(
        configs::effort_options(),
        vec![
            Choice::new("", "Default", "no override — use model default"),
            Choice::new("high", "High", "deepest reasoning, best quality, slowest"),
            Choice::new("medium", "Medium", "balanced speed and quality"),
            Choice::new("low", "Low", "fastest responses, minimal reasoning"),
        ]
    );
}

#[test]
fn w012_every_role_preset_value_label_and_hint() {
    assert_eq!(
        configs::role_presets(),
        vec![
            Choice::new("", "Skip", "no role description"),
            Choice::new(
                "Full-stack developer",
                "Full-stack developer",
                "general purpose"
            ),
            Choice::new("Frontend developer", "Frontend developer", "React, CSS, UI"),
            Choice::new(
                "Backend developer",
                "Backend developer",
                "APIs, databases, services"
            ),
            Choice::new(
                "DevOps engineer",
                "DevOps engineer",
                "CI/CD, infra, deploys"
            ),
            Choice::new(
                "QA / Test engineer",
                "QA / Test engineer",
                "testing, quality"
            ),
            Choice::new("Code reviewer", "Code reviewer", "review PRs, find bugs"),
            Choice::new("Technical writer", "Technical writer", "docs, READMEs"),
            Choice::new(CUSTOM_ROLE, "Custom...", "type your own"),
        ]
    );
}

#[test]
fn w013_agent_wizard_to_input_maps_an_ai_adapter() {
    let values = vals(&[
        ("name", "alpha"),
        ("adapter", "claude"),
        ("model", "opus"),
        ("effort", "high"),
        ("role", CUSTOM_ROLE),
        ("role_custom", "Reviews PRs"),
        ("skills", "review, careful ,, testing"),
        ("team", "team_1"),
    ]);
    let input = configs::agent_wizard_to_input(&values, "claude");
    assert_eq!(input.name, "alpha");
    assert_eq!(input.adapter, "claude");
    assert_eq!(input.opts.command, None);
    assert_eq!(input.opts.model, Some("opus".into()));
    assert_eq!(input.opts.effort, Some("high".into()));
    assert_eq!(input.opts.role, Some("Reviews PRs".into()));
    assert_eq!(
        input.opts.skills,
        Some(vec![
            "review".to_string(),
            "careful".to_string(),
            "testing".to_string()
        ])
    );
    assert_eq!(input.opts.approval_policy, Some("auto".into()));
    assert_eq!(input.team_id, Some("team_1".into()));
}

#[test]
fn w014_agent_wizard_to_input_uses_the_plain_role_when_not_custom() {
    let values = vals(&[
        ("name", "a"),
        ("adapter", "claude"),
        ("role", "Backend developer"),
    ]);
    let input = configs::agent_wizard_to_input(&values, "claude");
    assert_eq!(input.opts.role, Some("Backend developer".into()));
}

#[test]
fn w015_agent_wizard_to_input_for_shell_forces_auto_and_clears_ai_fields() {
    let values = vals(&[
        ("name", "runner"),
        ("adapter", "shell"),
        ("command", "  npm test  "),
        ("model", "sonnet"),
        ("effort", "high"),
        ("role", "Backend developer"),
        ("skills", "a,b"),
        ("team", "team_1"),
    ]);
    let input = configs::agent_wizard_to_input(&values, "claude");
    assert_eq!(input.opts.command, Some("npm test".into()));
    assert_eq!(input.opts.model, None);
    assert_eq!(input.opts.effort, None);
    assert_eq!(input.opts.role, None);
    assert_eq!(input.opts.skills, None);
    assert_eq!(input.opts.approval_policy, Some("auto".into()));
    assert_eq!(input.team_id, None);
}

#[test]
fn w016_agent_wizard_to_input_falls_back_to_the_default_adapter() {
    let values = vals(&[("name", "a")]);
    let input = configs::agent_wizard_to_input(&values, "codex");
    assert_eq!(input.adapter, "codex");
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. AGENT SHOP
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn w017_shop_wizard_is_one_select_step_over_all_fifteen_templates() {
    let steps = configs::shop_wizard_steps();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].id, "shop_template");
    assert_eq!(steps[0].label, "Agent Shop — choose a template");
    assert_eq!(steps[0].kind, StepKind::Select);

    let keys: Vec<&str> = AGENT_SHOP_TEMPLATES.iter().map(|t| t.key).collect();
    assert_eq!(
        keys,
        vec![
            "backend-dev",
            "frontend-dev",
            "qa-engineer",
            "code-reviewer",
            "architect",
            "devops-engineer",
            "bug-hunter",
            "tech-writer",
            "marketer",
            "content-creator",
            "growth-hacker",
            "security-auditor",
            "performance-engineer",
            "data-engineer",
            "fullstack-dev",
        ]
    );
}

#[test]
fn w018_shop_option_hints_are_the_template_descriptions() {
    let options = steps_options(&configs::shop_wizard_steps()[0]);
    let backend = options.iter().find(|o| o.value == "backend-dev").unwrap();
    assert_eq!(backend.label, "Backend Developer");
    assert_eq!(backend.hint, "APIs, databases, backend services");
}

fn steps_options(step: &WizardStep) -> Vec<Choice> {
    step.options.resolve(&Values::new())
}

#[test]
fn w019_apply_shop_template_injects_name_adapter_role_and_approval_policy() {
    let base = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude");
    let template = get_shop_template_by_key("backend-dev").unwrap();
    let steps = configs::apply_shop_template(base, template, "claude");
    let empty = Values::new();
    assert_eq!(
        find(&steps, "name").default_value.resolve(&empty),
        "Backend Developer"
    );
    assert_eq!(
        find(&steps, "adapter").default_value.resolve(&empty),
        "claude"
    );
    assert_eq!(
        find(&steps, "role").default_value.resolve(&empty),
        CUSTOM_ROLE
    );
    assert_eq!(
        find(&steps, "role_custom").default_value.resolve(&empty),
        template.role
    );
    assert_eq!(
        find(&steps, "approval_policy")
            .default_value
            .resolve(&empty),
        "auto"
    );
}

#[test]
fn w020_apply_shop_template_keeps_mcp_skills_for_claude_only() {
    let for_claude = configs::apply_shop_template(
        configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude"),
        get_shop_template_by_key("backend-dev").unwrap(),
        "claude",
    );
    let empty = Values::new();
    let claude_skills = find(&for_claude, "skills").default_value.resolve(&empty);
    assert!(claude_skills.contains("feature-dev:feature-dev"));

    let for_codex = configs::apply_shop_template(
        configs::agent_wizard_steps(&[], &[], &empty_catalog(), "codex"),
        get_shop_template_by_key("backend-dev").unwrap(),
        "codex",
    );
    let codex_skills = find(&for_codex, "skills").default_value.resolve(&empty);
    assert_eq!(codex_skills, "review, careful");
}

#[test]
fn w021_apply_shop_template_model_falls_back_to_the_empty_option() {
    // resolve_model("claude", Balanced) = "claude-sonnet-4-6", which is not
    // among the claude fallback options — must fall back to "".
    assert_eq!(
        resolve_model("claude", orch_domain::ModelTier::Balanced),
        "claude-sonnet-4-6"
    );
    let steps = configs::apply_shop_template(
        configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude"),
        get_shop_template_by_key("backend-dev").unwrap(),
        "claude",
    );
    assert_eq!(
        find(&steps, "model").default_value.resolve(&Values::new()),
        ""
    );
}

#[test]
fn w022_apply_shop_template_model_uses_the_tier_model_when_offered() {
    // resolve_model("cursor", Balanced) = "auto", which *is* a cursor
    // fallback option — must be used as-is.
    let steps = configs::apply_shop_template(
        configs::agent_wizard_steps(&[], &[], &empty_catalog(), "cursor"),
        get_shop_template_by_key("backend-dev").unwrap(),
        "cursor",
    );
    assert_eq!(
        find(&steps, "model").default_value.resolve(&Values::new()),
        "auto"
    );
}

#[test]
fn w023_is_mcp_skill_detects_the_pack_colon_skill_form() {
    assert!(configs::is_mcp_skill("feature-dev:feature-dev"));
    assert!(!configs::is_mcp_skill("careful"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. NEW AGENT — {template} / EDIT AGENT
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn w024_agent_from_shop_wizard_prefills_the_name_step() {
    let template = get_shop_template_by_key("qa-engineer").unwrap();
    let wizard = configs::agent_from_shop_wizard(template, &[], &[], &empty_catalog(), "claude");
    assert_eq!(wizard.title, "NEW AGENT — QA Engineer");
    assert_eq!(wizard.kind, WizardKind::AgentFromShop);
    assert_eq!(wizard.template_key, Some("qa-engineer".to_string()));
    assert_eq!(wizard.step_id(), "name");
    assert_eq!(wizard.text_value(), "QA Engineer");
}

/// EDIT AGENT has 8 steps: the 10-step NEW AGENT catalogue minus skills and
/// approval_policy.
#[test]
fn w025_edit_agent_has_eight_steps_minus_skills_and_approval_policy() {
    let agent = make_agent("agt_1", "alpha", AgentStatus::Idle);
    let steps = configs::edit_agent_wizard_steps(&agent, &[], &[], &empty_catalog());
    assert_eq!(
        step_ids(&steps),
        vec![
            "name",
            "adapter",
            "command",
            "model",
            "effort",
            "role",
            "role_custom",
            "team"
        ]
    );
}

#[test]
fn w026_edit_agent_defaults_come_from_the_agent() {
    let mut agent = make_agent("agt_1", "Alpha", AgentStatus::Idle);
    agent.adapter = "codex".into();
    agent.config.command = Some("echo hi".into());
    let steps = configs::edit_agent_wizard_steps(&agent, &[], &[], &empty_catalog());
    let empty = Values::new();
    assert_eq!(find(&steps, "name").default_value.resolve(&empty), "Alpha");
    assert_eq!(
        find(&steps, "adapter").default_value.resolve(&empty),
        "codex"
    );
    assert_eq!(
        find(&steps, "command").default_value.resolve(&empty),
        "echo hi"
    );
}

#[test]
fn w027_edit_agent_command_skip_falls_back_to_agent_adapter() {
    let mut agent = make_agent("agt_1", "Alpha", AgentStatus::Idle);
    agent.adapter = "shell".into();
    let steps = configs::edit_agent_wizard_steps(&agent, &[], &[], &empty_catalog());
    let command = find(&steps, "command");
    assert!(
        !command.is_skipped(&Values::new()),
        "falls back to agent.adapter == shell"
    );
    assert!(
        command.is_skipped(&vals(&[("adapter", "claude")])),
        "explicit choice wins"
    );
}

#[test]
fn w028_edit_agent_model_default_clears_only_when_adapter_changes() {
    let mut agent = make_agent("agt_1", "Alpha", AgentStatus::Idle);
    agent.adapter = "claude".into();
    agent.config.model = Some("sonnet".into());
    let steps = configs::edit_agent_wizard_steps(&agent, &[], &[], &empty_catalog());
    let model = find(&steps, "model");
    assert_eq!(
        model.default_value.resolve(&Values::new()),
        "sonnet",
        "unchanged adapter"
    );
    assert_eq!(
        model.default_value.resolve(&vals(&[("adapter", "claude")])),
        "sonnet",
        "re-chosen same adapter"
    );
    assert_eq!(
        model.default_value.resolve(&vals(&[("adapter", "codex")])),
        "",
        "changed adapter clears the model"
    );
}

#[test]
fn w029_edit_agent_name_validation_excludes_self() {
    let agents = vec![
        make_agent("agt_1", "Alpha", AgentStatus::Idle),
        make_agent("agt_2", "Beta", AgentStatus::Idle),
    ];
    let steps = configs::edit_agent_wizard_steps(&agents[0], &agents, &[], &empty_catalog());
    let validate = find(&steps, "name").validate.as_ref().unwrap();
    assert_eq!(
        validate("Alpha"),
        None,
        "renaming to its own current name is fine"
    );
    assert_eq!(validate("Beta"), Some(AGENT_NAME_TAKEN.to_string()));
    assert_eq!(validate(""), Some(NAME_REQUIRED.to_string()));
}

#[test]
fn w030_edit_agent_role_default_handles_preset_custom_and_none() {
    let mut preset = make_agent("agt_1", "A", AgentStatus::Idle);
    preset.role = Some("Frontend developer".into());
    let preset_steps = configs::edit_agent_wizard_steps(&preset, &[], &[], &empty_catalog());
    assert_eq!(
        find(&preset_steps, "role")
            .default_value
            .resolve(&Values::new()),
        "Frontend developer"
    );
    assert_eq!(
        find(&preset_steps, "role_custom")
            .default_value
            .resolve(&Values::new()),
        ""
    );

    let mut custom = make_agent("agt_2", "B", AgentStatus::Idle);
    custom.role = Some("Specializes in Rust".into());
    let custom_steps = configs::edit_agent_wizard_steps(&custom, &[], &[], &empty_catalog());
    assert_eq!(
        find(&custom_steps, "role")
            .default_value
            .resolve(&Values::new()),
        CUSTOM_ROLE
    );
    assert_eq!(
        find(&custom_steps, "role_custom")
            .default_value
            .resolve(&Values::new()),
        "Specializes in Rust"
    );

    let none_agent = make_agent("agt_3", "C", AgentStatus::Idle);
    let none_steps = configs::edit_agent_wizard_steps(&none_agent, &[], &[], &empty_catalog());
    assert_eq!(
        find(&none_steps, "role")
            .default_value
            .resolve(&Values::new()),
        ""
    );
    assert_eq!(
        find(&none_steps, "role_custom")
            .default_value
            .resolve(&Values::new()),
        ""
    );
}

#[test]
fn w031_edit_agent_team_default_is_the_current_team() {
    let agent = make_agent("agt_1", "A", AgentStatus::Idle);
    let teams = vec![make_team("team_1", "Alpha Team", "agt_1", &["agt_1"])];
    let steps = configs::edit_agent_wizard_steps(&agent, &[], &teams, &empty_catalog());
    assert_eq!(
        find(&steps, "team").default_value.resolve(&Values::new()),
        "team_1"
    );
}

#[test]
fn w032_edit_agent_to_fields_shell_trims_the_command_and_clears_ai_fields() {
    let values = vals(&[("adapter", "shell"), ("command", "  npm test  ")]);
    let (fields, team_id) = configs::edit_agent_wizard_to_fields(&values);
    assert_eq!(fields.command, Some("npm test".into()));
    assert_eq!(fields.role, None);
    assert_eq!(fields.model, None);
    assert_eq!(fields.effort, None);
    assert_eq!(team_id, None);
}

#[test]
fn w033_edit_agent_to_fields_clears_the_command_when_switching_away_from_shell() {
    let values = vals(&[("adapter", "codex")]);
    let (fields, _) = configs::edit_agent_wizard_to_fields(&values);
    assert_eq!(fields.command, Some(String::new()));
}

#[test]
fn w034_edit_agent_to_fields_leaves_the_command_untouched_with_no_adapter_key() {
    let values = vals(&[("name", "x")]);
    let (fields, _) = configs::edit_agent_wizard_to_fields(&values);
    assert_eq!(fields.command, None);
}

#[test]
fn w035_edit_agent_to_fields_preserves_effort_as_empty_string_to_allow_clearing() {
    let cleared = vals(&[("adapter", "claude"), ("effort", "")]);
    let (fields, _) = configs::edit_agent_wizard_to_fields(&cleared);
    assert_eq!(
        fields.effort,
        Some(String::new()),
        "an explicit empty value clears it"
    );

    let absent = vals(&[("adapter", "claude")]);
    let (fields, _) = configs::edit_agent_wizard_to_fields(&absent);
    assert_eq!(fields.effort, None, "an absent key leaves it untouched");
}

#[test]
fn w036_edit_agent_to_fields_maps_the_team_id() {
    let none = vals(&[("adapter", "claude"), ("team", "")]);
    assert_eq!(configs::edit_agent_wizard_to_fields(&none).1, None);
    let some = vals(&[("adapter", "claude"), ("team", "team_2")]);
    assert_eq!(
        configs::edit_agent_wizard_to_fields(&some).1,
        Some("team_2".into())
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. NEW TASK / EDIT TASK
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn w037_new_task_has_four_steps_in_order() {
    let steps = configs::task_wizard_steps(&[]);
    assert_eq!(
        step_ids(&steps),
        vec!["title", "priority", "assignee", "description"]
    );
}

#[test]
fn w038_edit_task_defaults_come_from_the_task() {
    let mut task = make_task("tsk_1", "Ship it", TaskStatus::Todo);
    task.priority = 2;
    task.assignee = Some("agt_1".into());
    task.description = "do the thing".into();
    let steps = configs::edit_task_wizard_steps(&task, &[]);
    let empty = Values::new();
    assert_eq!(
        find(&steps, "title").default_value.resolve(&empty),
        "Ship it"
    );
    assert_eq!(find(&steps, "priority").default_value.resolve(&empty), "2");
    assert_eq!(
        find(&steps, "assignee").default_value.resolve(&empty),
        "agt_1"
    );
    assert_eq!(
        find(&steps, "description").default_value.resolve(&empty),
        "do the thing"
    );
}

#[test]
fn w039_new_task_priority_default_is_three() {
    let steps = configs::task_wizard_steps(&[]);
    assert_eq!(
        find(&steps, "priority")
            .default_value
            .resolve(&Values::new()),
        "3"
    );
}

#[test]
fn w040_new_task_assignee_sentinel_is_auto_assign() {
    let agents = vec![make_agent("agt_1", "a", AgentStatus::Idle)];
    let steps = configs::task_wizard_steps(&agents);
    let options = steps_options(find(&steps, "assignee"));
    assert_eq!(
        options[0],
        Choice::new("", "Auto-assign", "orchestrator picks the best agent")
    );
}

#[test]
fn w041_edit_task_assignee_sentinel_keeps_the_current_assignee() {
    let agents = vec![make_agent("agt_1", "a", AgentStatus::Idle)];
    let task = make_task("tsk_1", "T", TaskStatus::Todo);
    let steps = configs::edit_task_wizard_steps(&task, &agents);
    let options = steps_options(find(&steps, "assignee"));
    assert_eq!(
        options[0],
        Choice::new("", "Keep current", "leave the assignee unchanged")
    );
}

#[test]
fn w042_assignee_hint_is_adapter_bracket_and_the_first_role_line() {
    let mut agent = make_agent("agt_1", "Alpha", AgentStatus::Idle);
    agent.adapter = "codex".into();
    agent.role = Some("  Backend specialist  \nsecond line".into());
    let options = configs::agent_choices(&[agent]);
    assert_eq!(options[0].hint, "[codex] Backend specialist");
}

#[test]
fn w043_assignee_hint_is_the_bare_adapter_when_there_is_no_role() {
    let agent = make_agent("agt_1", "Alpha", AgentStatus::Idle);
    let options = configs::agent_choices(&[agent]);
    assert_eq!(options[0].hint, "claude");
}

#[test]
fn w044_assignee_hint_at_exactly_sixty_chars_is_not_truncated() {
    let mut agent = make_agent("agt_1", "Alpha", AgentStatus::Idle);
    let role_60 = "r".repeat(60);
    agent.role = Some(role_60.clone());
    let options = configs::agent_choices(&[agent]);
    assert_eq!(options[0].hint, format!("[claude] {role_60}"));
    assert_eq!(role_60.chars().count(), 60);
}

#[test]
fn w045_assignee_hint_at_sixty_one_chars_truncates_to_fifty_nine_plus_ellipsis() {
    let mut agent = make_agent("agt_1", "Alpha", AgentStatus::Idle);
    agent.role = Some("r".repeat(61));
    let options = configs::agent_choices(&[agent]);
    let expected = format!("[claude] {}…", "r".repeat(59));
    assert_eq!(options[0].hint, expected);
}

#[test]
fn w046_disabled_agents_are_excluded_from_assignee_options() {
    let agents = vec![
        make_agent("agt_1", "Alpha", AgentStatus::Idle),
        make_agent("agt_2", "Beta", AgentStatus::Disabled),
    ];
    let options = configs::agent_choices(&agents);
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].value, "agt_1");
}

#[test]
fn w047_assignee_step_skips_when_there_are_at_most_one_option() {
    let no_agents = configs::task_wizard_steps(&[]);
    assert!(find(&no_agents, "assignee").is_skipped(&Values::new()));

    let one_agent = vec![make_agent("agt_1", "Alpha", AgentStatus::Idle)];
    let with_agent = configs::task_wizard_steps(&one_agent);
    assert!(!find(&with_agent, "assignee").is_skipped(&Values::new()));
}

#[test]
fn w048_task_wizard_to_input_parses_priority_and_strips_empties() {
    let values = vals(&[("title", "Ship it"), ("priority", "2"), ("description", "")]);
    let (title, opts, assignee) = configs::task_wizard_to_input(&values);
    assert_eq!(title, "Ship it");
    assert_eq!(opts.priority, Some(2));
    assert_eq!(opts.description, None);
    assert_eq!(assignee, None);
}

#[test]
fn w049_edit_task_wizard_to_fields_always_writes_description() {
    let values = vals(&[("title", "T"), ("priority", "1"), ("description", "")]);
    let (fields, assignee) = configs::edit_task_wizard_to_fields(&values);
    assert_eq!(
        fields.description,
        Some(String::new()),
        "clears, rather than leaving untouched"
    );
    assert_eq!(fields.priority, Some(1));
    assert_eq!(assignee, None);
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. NEW TEAM
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn w050_new_team_has_four_steps_in_order() {
    let steps = configs::team_wizard_steps(&[], &[]);
    assert_eq!(
        step_ids(&steps),
        vec!["name", "lead", "members", "description"]
    );
}

#[test]
fn w051_team_lead_options_have_no_empty_sentinel() {
    let agents = vec![make_agent("agt_1", "Alpha", AgentStatus::Idle)];
    let steps = configs::team_wizard_steps(&agents, &[]);
    let options = steps_options(find(&steps, "lead"));
    assert_eq!(options, vec![Choice::new("agt_1", "Alpha", "claude")]);
}

#[test]
fn w052_team_members_options_filter_out_the_lead() {
    let agents = vec![
        make_agent("agt_1", "Alpha", AgentStatus::Idle),
        make_agent("agt_2", "Beta", AgentStatus::Idle),
    ];
    let steps = configs::team_wizard_steps(&agents, &[]);
    let members = find(&steps, "members");
    let options = members.options.resolve(&vals(&[("lead", "agt_1")]));
    assert_eq!(options.len(), 1);
    assert_eq!(options[0].value, "agt_2");
}

#[test]
fn w053_team_members_step_skips_when_no_agent_other_than_the_lead_exists() {
    let one_agent = vec![make_agent("agt_1", "Alpha", AgentStatus::Idle)];
    let steps = configs::team_wizard_steps(&one_agent, &[]);
    assert!(find(&steps, "members").is_skipped(&vals(&[("lead", "agt_1")])));

    let two_agents = vec![
        make_agent("agt_1", "Alpha", AgentStatus::Idle),
        make_agent("agt_2", "Beta", AgentStatus::Idle),
    ];
    let steps = configs::team_wizard_steps(&two_agents, &[]);
    assert!(!find(&steps, "members").is_skipped(&vals(&[("lead", "agt_1")])));
}

#[test]
fn w054_team_wizard_to_input_splits_members_and_keeps_description_optional() {
    let values = vals(&[
        ("name", "Core"),
        ("lead", "agt_1"),
        ("members", "agt_2,agt_3"),
        ("description", ""),
    ]);
    let input = configs::team_wizard_to_input(&values);
    assert_eq!(input.name, "Core");
    assert_eq!(input.lead_agent_id, "agt_1");
    assert_eq!(
        input.member_agent_ids,
        Some(vec!["agt_2".into(), "agt_3".into()])
    );
    assert_eq!(input.description, None);
}

#[test]
fn w055_team_wizard_to_input_treats_an_empty_members_string_as_none() {
    let values = vals(&[("name", "Core"), ("lead", "agt_1"), ("members", "")]);
    let input = configs::team_wizard_to_input(&values);
    assert_eq!(input.member_agent_ids, None);
}

#[test]
fn w056_team_name_validation_excludes_nothing_and_checks_collisions() {
    let teams = vec![make_team("team_1", "Core", "agt_1", &["agt_1"])];
    let steps = configs::team_wizard_steps(&[], &teams);
    let validate = find(&steps, "name").validate.as_ref().unwrap();
    assert_eq!(validate("Core"), Some(TEAM_NAME_TAKEN.to_string()));
    assert_eq!(validate("Other"), None);
    assert_eq!(validate("  "), Some(NAME_REQUIRED.to_string()));
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. New Goal / Edit Goal
// ═══════════════════════════════════════════════════════════════════════════

const GOAL_TITLE_DESC: &str = "Be specific — agents work better with clear, measurable objectives";
const GOAL_ASSIGNEE_DESC: &str =
    "Assigned agent gets autonomous mode — it will plan and execute without prompts";
const GOAL_DESCRIPTION_DESC: &str =
    "Context matters — include tech stack, constraints, and what \"done\" looks like";

#[test]
fn w057_new_goal_has_three_steps_with_their_descriptions() {
    let agents = vec![make_agent("agt_1", "a", AgentStatus::Idle)];
    let steps = configs::goal_wizard_steps(&agents);
    assert_eq!(step_ids(&steps), vec!["title", "assignee", "description"]);
    assert_eq!(
        find(&steps, "title").description.as_deref(),
        Some(GOAL_TITLE_DESC)
    );
    assert_eq!(
        find(&steps, "assignee").description.as_deref(),
        Some(GOAL_ASSIGNEE_DESC)
    );
    assert_eq!(
        find(&steps, "description").description.as_deref(),
        Some(GOAL_DESCRIPTION_DESC)
    );
}

#[test]
fn w058_edit_goal_defaults_come_from_the_goal() {
    let mut goal = make_goal("goal_1", "Ship v2", orch_domain::GoalStatus::Active);
    goal.assignee = Some("agt_1".into());
    goal.description = "do it".into();
    let steps = configs::edit_goal_wizard_steps(&goal, &[]);
    let empty = Values::new();
    assert_eq!(
        find(&steps, "title").default_value.resolve(&empty),
        "Ship v2"
    );
    assert_eq!(
        find(&steps, "assignee").default_value.resolve(&empty),
        "agt_1"
    );
    assert_eq!(
        find(&steps, "description").default_value.resolve(&empty),
        "do it"
    );
}

#[test]
fn w059_new_goal_sentinel_hint_mentions_autonomous_agents() {
    let agents = vec![make_agent("agt_1", "a", AgentStatus::Idle)];
    let steps = configs::goal_wizard_steps(&agents);
    let options = steps_options(find(&steps, "assignee"));
    assert_eq!(
        options[0],
        Choice::new("", "Any agent", "auto-assign to autonomous agents")
    );
}

#[test]
fn w060_edit_goal_sentinel_keeps_the_current_assignee() {
    let agents = vec![make_agent("agt_1", "a", AgentStatus::Idle)];
    let goal = make_goal("goal_1", "G", orch_domain::GoalStatus::Active);
    let steps = configs::edit_goal_wizard_steps(&goal, &agents);
    let options = steps_options(find(&steps, "assignee"));
    assert_eq!(
        options[0],
        Choice::new("", "Keep current", "leave the assignee unchanged")
    );
}

#[test]
fn w061_goal_wizard_to_input_drops_empty_assignee_and_description() {
    let values = vals(&[("title", "Ship v2"), ("assignee", ""), ("description", "")]);
    let input = configs::goal_wizard_to_input(&values);
    assert_eq!(input.title, "Ship v2");
    assert_eq!(input.assignee, None);
    assert_eq!(input.description, None);
}

#[test]
fn w062_edit_goal_wizard_to_fields_always_writes_description() {
    let values = vals(&[("title", "T"), ("assignee", "agt_1"), ("description", "")]);
    let fields = configs::edit_goal_wizard_to_fields(&values);
    assert_eq!(fields.description, Some(String::new()));
    assert_eq!(fields.assignee, Some("agt_1".into()));
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. SETTINGS
// ═══════════════════════════════════════════════════════════════════════════

fn notifications(toast: bool, bell: bool) -> orch_domain::NotificationPreferences {
    orch_domain::NotificationPreferences { toast, bell }
}

#[test]
fn w063_every_setting_is_exactly_one_step() {
    for setting in [
        "palette",
        "activity-filter",
        "max-concurrent",
        "notifications-toast",
        "notifications-bell",
    ] {
        let steps =
            configs::config_wizard_steps(setting, "amber", "all", 4, notifications(true, true));
        assert_eq!(steps.len(), 1, "{setting} must be one step");
    }
}

#[test]
fn w064_setting_kebab_case_maps_to_a_snake_case_step_id() {
    let cases = [
        ("palette", "palette"),
        ("activity-filter", "activity_filter"),
        ("max-concurrent", "max_concurrent"),
        ("notifications-toast", "notifications_toast"),
        ("notifications-bell", "notifications_bell"),
    ];
    for (setting, id) in cases {
        let steps =
            configs::config_wizard_steps(setting, "amber", "all", 4, notifications(true, true));
        assert_eq!(steps[0].id, id, "{setting} -> {id}");
    }
}

#[test]
fn w065_an_unknown_setting_yields_no_steps() {
    let steps = configs::config_wizard_steps("bogus", "amber", "all", 4, notifications(true, true));
    assert!(steps.is_empty());
}

#[test]
fn w066_palette_option_order_is_exact() {
    assert_eq!(
        configs::palette_options(),
        vec![
            Choice::new("amber", "Amber", "warm ORCH default"),
            Choice::new("ocean", "Ocean", "cool blue control room"),
            Choice::new("forest", "Forest", "calm green operations"),
            Choice::new("violet", "Violet", "high-contrast purple"),
            Choice::new(
                "light",
                "Light",
                "high-contrast palette for light terminals"
            ),
        ]
    );
}

#[test]
fn w067_max_concurrent_options_use_singular_and_plural_units() {
    let options = configs::max_concurrent_options();
    assert_eq!(
        options[0],
        Choice::new("1", "1 agent", "~0.5 GB RAM, 1 subprocess")
    );
    assert_eq!(
        options[1],
        Choice::new("2", "2 agents", "~1 GB RAM, 2 subprocesses")
    );
    assert_eq!(
        options.last().unwrap(),
        &Choice::new("10", "10 agents", "~5 GB RAM, 10 subprocesses")
    );
    assert_eq!(options.len(), 7);
}

#[test]
fn w068_toggle_options_are_on_and_off() {
    assert_eq!(
        configs::toggle_options(),
        vec![
            Choice::new("true", "On", ""),
            Choice::new("false", "Off", "")
        ]
    );
}

#[test]
fn w069_setting_defaults_reflect_the_live_values() {
    let palette =
        configs::config_wizard_steps("palette", "ocean", "all", 4, notifications(true, true));
    assert_eq!(palette[0].default_value.resolve(&Values::new()), "ocean");

    let max = configs::config_wizard_steps(
        "max-concurrent",
        "amber",
        "all",
        6,
        notifications(true, true),
    );
    assert_eq!(max[0].default_value.resolve(&Values::new()), "6");

    let toast_off = configs::config_wizard_steps(
        "notifications-toast",
        "amber",
        "all",
        4,
        notifications(false, true),
    );
    assert_eq!(toast_off[0].default_value.resolve(&Values::new()), "false");
}

#[test]
fn w070_config_wizard_title_uppercases_and_spaces_the_setting() {
    assert_eq!(
        configs::config_wizard_title("palette"),
        "SETTINGS — PALETTE"
    );
    assert_eq!(
        configs::config_wizard_title("max-concurrent"),
        "SETTINGS — MAX CONCURRENT"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. Validation catalogue + debounce / synchronous semantics
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn w071_every_validation_message_is_verbatim() {
    assert_eq!(
        configs::validate_title(""),
        Some(TITLE_REQUIRED.to_string())
    );
    assert_eq!(
        configs::validate_title("  "),
        Some(TITLE_REQUIRED.to_string())
    );
    assert_eq!(configs::validate_title("ok"), None);

    assert_eq!(
        configs::validate_priority("0"),
        Some(PRIORITY_RANGE.to_string())
    );
    assert_eq!(
        configs::validate_priority("5"),
        Some(PRIORITY_RANGE.to_string())
    );
    assert_eq!(
        configs::validate_priority("1.5"),
        Some(PRIORITY_RANGE.to_string())
    );
    assert_eq!(
        configs::validate_priority("abc"),
        Some(PRIORITY_RANGE.to_string())
    );
    assert_eq!(configs::validate_priority("3"), None);

    let agents = vec![make_agent("agt_1", "Alpha", AgentStatus::Idle)];
    assert_eq!(
        configs::validate_agent_name("", &agents, None),
        Some(NAME_REQUIRED.to_string())
    );
    assert_eq!(
        configs::validate_agent_name("Alpha", &agents, None),
        Some(AGENT_NAME_TAKEN.to_string())
    );

    let teams = vec![make_team("team_1", "Core", "agt_1", &["agt_1"])];
    assert_eq!(
        configs::validate_team_name("", &teams),
        Some(NAME_REQUIRED.to_string())
    );
    assert_eq!(
        configs::validate_team_name("Core", &teams),
        Some(TEAM_NAME_TAKEN.to_string())
    );

    assert_eq!(
        configs::validate_shell_command(""),
        Some(COMMAND_REQUIRED_FOR_SHELL.to_string())
    );
    assert_eq!(configs::validate_shell_command("npm test"), None);

    assert_eq!(FIX_THE_ERROR_ABOVE, "Fix the error above");
}

#[test]
fn w072_text_validation_debounces_for_exactly_300ms() {
    let steps = configs::task_wizard_steps(&[]);
    let title_step = steps.into_iter().find(|s| s.id == "title").unwrap();
    let mut wizard = Wizard::new(WizardKind::Task, "T", vec![title_step]);
    type_into(&mut wizard, "x", 0);
    wizard.handle_key(key(KeyCode::Backspace), 10);
    assert_eq!(wizard.visible_error(), None, "not yet dirty-validated");
    wizard.poll(10 + VALIDATION_DEBOUNCE_MS - 1);
    assert_eq!(
        wizard.visible_error(),
        None,
        "still inside the debounce window"
    );
    wizard.poll(10 + VALIDATION_DEBOUNCE_MS);
    assert_eq!(wizard.visible_error(), Some(TITLE_REQUIRED));
}

#[test]
fn w073_select_validation_is_synchronous_with_no_poll() {
    // An artificially out-of-range option demonstrates that `validate_priority`
    // blocks immediately on a select confirm — no `poll()` call at all.
    let step = WizardStep::new("priority", "Priority", StepKind::Select)
        .options(vec![Choice::new("5", "Five", "")])
        .validate(configs::validate_priority);
    let mut wizard = Wizard::new(WizardKind::Task, "T", vec![step]);
    assert_eq!(
        wizard.handle_key(key(KeyCode::Enter), 0),
        WizardOutcome::Consumed
    );
    assert_eq!(wizard.visible_error(), Some(PRIORITY_RANGE));
}

#[test]
fn w074_number_keys_bypass_select_validation() {
    let step = WizardStep::new("priority", "Priority", StepKind::Select)
        .options(vec![Choice::new("5", "Five", "")])
        .validate(configs::validate_priority);
    let mut wizard = Wizard::new(WizardKind::Task, "T", vec![step]);
    match wizard.handle_key(key(KeyCode::Char('1')), 0) {
        WizardOutcome::Complete(values) => assert_eq!(values["priority"], "5"),
        other => panic!("expected completion despite the invalid value, got {other:?}"),
    }
}

#[test]
fn w075_confirm_revalidates_and_defeats_a_stale_debounced_error() {
    let steps = configs::task_wizard_steps(&[]);
    let title_step = steps.into_iter().find(|s| s.id == "title").unwrap();
    let mut wizard = Wizard::new(WizardKind::Task, "T", vec![title_step]);
    wizard.handle_key(key(KeyCode::Backspace), 0);
    wizard.poll(400);
    assert_eq!(wizard.visible_error(), Some(TITLE_REQUIRED));
    type_into(&mut wizard, "Ship it", 400);
    assert_eq!(
        wizard.handle_key(key(KeyCode::Enter), 420),
        WizardOutcome::Complete(vals(&[("title", "Ship it")]))
    );
}

#[test]
fn w076_blocked_confirm_shows_fix_the_error_above_for_2000ms() {
    let steps = configs::team_wizard_steps(&[], &[]);
    let name_step = steps.into_iter().find(|s| s.id == "name").unwrap();
    let mut wizard = Wizard::new(WizardKind::Team, "T", vec![name_step]);
    wizard.handle_key(key(KeyCode::Enter), 0);
    assert!(wizard.enter_block_hint());
    wizard.poll(ENTER_BLOCK_HINT_MS - 1);
    assert!(wizard.enter_block_hint());
    wizard.poll(ENTER_BLOCK_HINT_MS);
    assert!(!wizard.enter_block_hint());
}

#[test]
fn w077_errors_clear_on_both_forward_and_backward_navigation() {
    // One agent only: `lead` has a single option and `members` is skipped, so
    // confirming `name` lands directly on `lead` — exactly two active steps.
    let steps = configs::team_wizard_steps(
        &[make_agent("agt_1", "Alpha", AgentStatus::Idle)],
        &[make_team("team_1", "Core", "agt_1", &["agt_1"])],
    );
    let mut wizard = Wizard::new(WizardKind::Team, "T", steps);
    type_into(&mut wizard, "Core", 0);
    wizard.handle_key(key(KeyCode::Enter), 0);
    wizard.poll(VALIDATION_DEBOUNCE_MS);
    assert_eq!(wizard.visible_error(), Some(TEAM_NAME_TAKEN));

    // Fix it and confirm — the error must clear going forward.
    type_into(&mut wizard, "x", VALIDATION_DEBOUNCE_MS);
    wizard.handle_key(key(KeyCode::Enter), VALIDATION_DEBOUNCE_MS + 10);
    assert_eq!(wizard.step_id(), "lead");
    assert_eq!(wizard.visible_error(), None);

    // Backspace on the (empty-less) select step goes back — the stale error
    // must not resurface.
    wizard.handle_key(key(KeyCode::Backspace), 0);
    assert_eq!(wizard.step_id(), "name");
    assert_eq!(wizard.visible_error(), None);
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. Engine keys, wired through real catalogue steps
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn w078_tab_confirms_a_real_text_step_and_a_real_select_step() {
    let steps = configs::task_wizard_steps(&[]);
    let mut wizard = Wizard::new(WizardKind::Task, "T", steps);
    type_into(&mut wizard, "Ship it", 0);
    assert_eq!(
        wizard.handle_key(key(KeyCode::Tab), 0),
        WizardOutcome::Consumed
    );
    assert_eq!(wizard.step_id(), "priority");
    assert_eq!(
        wizard.handle_key(key(KeyCode::Tab), 0),
        WizardOutcome::Consumed
    );
    assert_eq!(
        wizard.step_id(),
        "description",
        "assignee has <=1 option and is skipped"
    );
}

#[test]
fn w079_ctrl_enter_confirms_a_real_textarea_while_plain_enter_inserts_a_newline() {
    let agents = vec![make_agent("agt_1", "a", AgentStatus::Idle)];
    let steps = configs::goal_wizard_steps(&agents);
    let description_step = steps.into_iter().find(|s| s.id == "description").unwrap();
    let mut wizard = Wizard::new(WizardKind::Goal, "T", vec![description_step]);
    type_into(&mut wizard, "line1", 0);
    wizard.handle_key(key(KeyCode::Enter), 0);
    type_into(&mut wizard, "line2", 0);
    assert_eq!(wizard.textarea_value(), "line1\nline2");
    match wizard.handle_key(ctrl(KeyCode::Enter), 0) {
        WizardOutcome::Complete(values) => assert_eq!(values["description"], "line1\nline2"),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn w080_backspace_on_a_real_select_step_goes_back_and_never_past_step_zero() {
    let steps =
        configs::config_wizard_steps("palette", "amber", "all", 4, notifications(true, true));
    let mut wizard = Wizard::new(WizardKind::Config, "T", steps);
    assert_eq!(
        wizard.handle_key(key(KeyCode::Backspace), 0),
        WizardOutcome::Cancelled
    );
}

#[test]
fn w081_browse_mode_on_the_real_model_step_submits_the_value() {
    let mut catalog = empty_catalog();
    catalog.insert(
        "claude".into(),
        vec![
            ModelOption {
                value: "opus".into(),
                label: "Opus".into(),
                hint: "big".into(),
            },
            ModelOption {
                value: "sonnet".into(),
                label: "Sonnet".into(),
                hint: "mid".into(),
            },
        ],
    );
    let steps = configs::agent_wizard_steps(&[], &[], &catalog, "claude");
    let model_step = steps.into_iter().find(|s| s.id == "model").unwrap();
    let mut wizard = Wizard::new(WizardKind::Agent, "T", vec![model_step]);
    // Values is empty, so `get_suggestions` resolves against adapter == "".
    type_into(&mut wizard, "son", 0);
    wizard.handle_key(key(KeyCode::Down), 0);
    assert!(wizard.is_browsing());
    match wizard.handle_key(key(KeyCode::Enter), 0) {
        WizardOutcome::Complete(values) => assert_eq!(values["model"], "sonnet"),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn w082_callback_suggestion_on_the_real_agent_name_step_reports_the_template_key() {
    let steps = configs::agent_wizard_steps(&[], &[], &empty_catalog(), "claude");
    let name_step = steps.into_iter().find(|s| s.id == "name").unwrap();
    let mut wizard = Wizard::new(WizardKind::Agent, "T", vec![name_step]);
    wizard.handle_key(key(KeyCode::Down), 0);
    assert_eq!(
        wizard.handle_key(key(KeyCode::Enter), 0),
        WizardOutcome::SuggestionSelected("backend-dev".into())
    );
}

#[test]
fn w083_multiline_paste_on_a_real_textarea_normalises_crlf() {
    let steps = configs::task_wizard_steps(&[]);
    let description_step = steps.into_iter().find(|s| s.id == "description").unwrap();
    let mut wizard = Wizard::new(WizardKind::Task, "T", vec![description_step]);
    wizard.insert_paste("one\r\ntwo\nthree", 0);
    assert_eq!(wizard.textarea_value(), "one\ntwo\nthree");
}

#[test]
fn w084_ctrl_v_fires_paste_only_when_the_wizard_declares_paste_capable() {
    let task_wizard = configs::new_task_wizard(&[]);
    assert!(task_wizard.paste_capable, "task wizards accept image paste");
    let goal_wizard = configs::new_goal_wizard(&[]);
    assert!(!goal_wizard.paste_capable, "goal wizards do not");

    let mut paste_capable_wizard = task_wizard;
    assert_eq!(
        paste_capable_wizard.handle_key(ctrl(KeyCode::Char('v')), 0),
        WizardOutcome::PasteImage
    );
    let mut not_paste_capable_wizard = goal_wizard;
    assert_eq!(
        not_paste_capable_wizard.handle_key(ctrl(KeyCode::Char('v')), 0),
        WizardOutcome::Consumed,
        "ignored, not a crash"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. End-to-end via the app harness
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn w085_a_then_n_opens_the_new_agent_wizard() {
    let mut harness = Harness::new();
    harness.key('a');
    assert_eq!(harness.app.state.view, ViewId::Agents);
    harness.key('n');
    harness.assert_text("Agent name");
}

#[test]
fn w086_seven_downs_on_the_provider_step_select_shell() {
    let mut harness = Harness::new();
    harness.key('a');
    harness.key('n');
    harness.type_str("Test Runner");
    harness.press(KeyCode::Enter);
    for _ in 0..7 {
        harness.key('j');
    }
    let wizard = harness.app.state.wizard.as_ref().expect("wizard open");
    assert_eq!(wizard.step_id(), "adapter");
    assert_eq!(wizard.select_index(), 7);
    assert_eq!(wizard.options()[7].value, "shell");
}

#[test]
fn w087_completing_the_shell_path_calls_add_agent() {
    let mut harness = Harness::new();
    harness.key('a');
    harness.key('n');
    harness.type_str("Test Runner");
    harness.press(KeyCode::Enter);
    for _ in 0..7 {
        harness.key('j');
    }
    harness.press(KeyCode::Enter); // confirm "shell"
    harness.type_str("npm test");
    harness.press(KeyCode::Enter); // confirm command -> submit
    harness.assert_called(|call| {
        matches!(
            call,
            Call::AddAgent { name, adapter, opts }
                if name == "Test Runner"
                    && adapter == "shell"
                    && opts.command.as_deref() == Some("npm test")
                    && opts.approval_policy.as_deref() == Some("auto")
        )
    });
    assert_eq!(
        harness.calls().len(),
        1,
        "no team join without a chosen team"
    );
    harness.refute_called(|call| matches!(call, Call::JoinTeam { .. }));
    harness.assert_message("Creating agent \"Test Runner\"...");
    harness.assert_message("✓ Created agent \"Test Runner\"");
    harness.refute_message("Joined team");
}

#[test]
fn w088_t_then_n_opens_the_new_task_wizard() {
    let mut harness = Harness::new();
    harness.key('t');
    harness.key('n');
    harness.assert_text("Task title");
}

#[test]
fn w089_esc_at_step_zero_cancels_the_wizard_and_returns_to_normal_mode() {
    let mut harness = Harness::new();
    harness.key('a');
    harness.key('n');
    assert!(harness.app.state.wizard.is_some());
    harness.press(KeyCode::Esc);
    assert!(harness.app.state.wizard.is_none());
}

#[test]
fn w090_slash_agent_shop_opens_the_agent_shop_wizard() {
    let mut harness = Harness::new();
    harness.run_command("/agent shop");
    harness.assert_text("AGENT SHOP");
    harness.refute_text("EDIT AGENT");
}

#[test]
fn w091_ctrl_s_inside_the_new_agent_wizard_opens_the_agent_shop() {
    let mut harness = Harness::new();
    harness.key('a');
    harness.key('n');
    harness.assert_text("NEW AGENT");
    harness.press_mod(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(
        harness.app.state.wizard.as_ref().map(|w| w.kind.clone()),
        Some(WizardKind::AgentShop)
    );
}

#[test]
fn w092_a_key_release_event_is_ignored_even_while_a_wizard_is_open() {
    let mut harness = Harness::new();
    harness.key('a');
    harness.key('n');
    harness.release(KeyCode::Char('x'));
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.text_value(), "", "a release event must not type");
}

#[test]
fn w093_the_real_app_renders_the_debounced_error_only_after_300ms_with_no_sleep() {
    let mut harness = Harness::new();
    harness.key('t');
    harness.key('n');
    harness.press(KeyCode::Backspace); // empty + required -> schedules nothing yet
    harness.type_str("x");
    harness.press(KeyCode::Backspace); // dirty, debounce scheduled
    harness.refute_text(TITLE_REQUIRED);
    let before = harness.now();
    harness.advance(VALIDATION_DEBOUNCE_MS - 1);
    harness.refute_text(TITLE_REQUIRED);
    harness.advance(1);
    assert_eq!(harness.now(), before + VALIDATION_DEBOUNCE_MS);
    harness.assert_text(TITLE_REQUIRED);
}

// ── Select-list display (agent shop T0 F4/F5) ───────────────────────────────

fn shop_at(width: u16, height: u16) -> Harness {
    let mut harness = Harness::sized(orch_tui::backend::FakeBackend::new(), width, height);
    harness.run_command("/agent shop");
    harness.assert_text("Agent Shop — choose a template");
    harness
}

/// The rendered row holding `needle`, without its trailing padding.
fn row_with(harness: &mut Harness, needle: &str) -> String {
    let text = harness.text();
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{text}"))
        .trim_end()
        .to_string()
}

#[test]
fn w094_two_digit_option_numbers_are_separated_from_their_labels() {
    let mut harness = shop_at(120, 40);
    let text = harness.text();
    for (number, name) in [
        (10, "Content Creator"),
        (11, "Growth Hacker"),
        (15, "Full-Stack Developer"),
    ] {
        assert!(
            text.contains(&format!(" {number} {name}")),
            "{number} {name}:\n{text}"
        );
        assert!(!text.contains(&format!("{number}{name}")), "{text}");
    }
    // One-digit numbers keep their column, aligned with the two-digit ones.
    let one = row_with(&mut harness, "Frontend Developer");
    let ten = row_with(&mut harness, "Content Creator");
    assert_eq!(one.find("Frontend"), ten.find("Content"), "{one}\n{ten}");
}

#[test]
fn w095_an_option_too_wide_for_the_row_ends_in_an_ellipsis() {
    for width in [60, 80] {
        let mut harness = shop_at(width, 24);
        let qa = row_with(&mut harness, "QA Engineer");
        assert!(qa.contains("QA Engineer ─ "), "{width}: {qa:?}");
        assert!(qa.ends_with('…'), "{width}: cut silently: {qa:?}");
    }
    // Wide enough: nothing is cut.
    let mut harness = shop_at(120, 40);
    let qa = row_with(&mut harness, "QA Engineer");
    assert!(!qa.contains('…'), "{qa:?}");
}

#[test]
fn w096_long_and_unicode_option_labels_are_cut_on_cell_boundaries() {
    let long = "A Very Long Role Name That Keeps Going Well Past Any Reasonable Column Budget";
    let options = vec![
        Choice::new("long", long, "never reached"),
        Choice::new(
            "cjk",
            "研究员 👩‍👩‍👧 Café",
            "分析市场数据并撰写报告，分析市场数据并撰写报告 עברית 👍🏽 résumé",
        ),
    ];
    let wizard = Wizard::new(
        WizardKind::AgentShop,
        "AGENT SHOP",
        vec![WizardStep::new("shop_template", "Pick", StepKind::Select).options(options)],
    );
    let mut harness = Harness::sized(orch_tui::backend::FakeBackend::new(), 60, 24);
    harness.app.open_wizard(wizard);

    let row = row_with(&mut harness, "A Very Long");
    assert!(row.ends_with('…'), "{row:?}");
    assert!(!row.contains("never reached"), "{row:?}");

    // The buffer pads a wide glyph's second cell, so the label reads spaced.
    let row = row_with(&mut harness, "研 究 员");
    assert!(row.contains("Café ─ "), "{row:?}");
    assert!(row.ends_with('…'), "{row:?}");
    assert!(!row.contains("résumé"), "{row:?}");
}

#[test]
fn w097_a_list_longer_than_its_window_shows_the_selection_position() {
    let mut harness = shop_at(80, 24);
    harness.assert_text("1 of 15");
    for _ in 0..3 {
        harness.press(KeyCode::Down);
    }
    harness.assert_text("4 of 15");

    // When every option is on screen there is nothing to locate.
    let mut harness = shop_at(120, 40);
    harness.refute_text(" of 15");
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. Agent picker: type-ahead filter and order (R7, R8)
// ═══════════════════════════════════════════════════════════════════════════

/// Five agents in catalogue order, one disabled. Names, adapters, roles and
/// statuses are chosen so each order (Original, Name, Adapter, Status) lists
/// the four eligible agents differently.
fn picker_roster() -> Vec<orch_domain::Agent> {
    let agent = |id: &str, name: &str, adapter: &str, role: &str, status: AgentStatus| {
        let mut a = make_agent(id, name, status);
        a.adapter = adapter.into();
        a.role = (!role.is_empty()).then(|| role.to_string());
        a
    };
    vec![
        agent(
            "agt_a",
            "Alpha",
            "claude",
            "Frontend reviewer",
            AgentStatus::Running,
        ),
        agent(
            "agt_b",
            "beta",
            "codex",
            "Backend builder",
            AgentStatus::Idle,
        ),
        agent("agt_c", "Gamma", "shell", "", AgentStatus::Error),
        agent(
            "agt_d",
            "Delta",
            "claude",
            "Never listed",
            AgentStatus::Disabled,
        ),
        agent("agt_e", "alpha-2", "opencode", "Docs", AgentStatus::Idle),
    ]
}

fn labels(choices: &[Choice]) -> Vec<String> {
    choices.iter().map(|c| c.label.clone()).collect()
}

/// The NEW TASK wizard's real assignee step, alone.
fn assignee_wizard(agents: &[orch_domain::Agent]) -> Wizard {
    let step = configs::task_wizard_steps(agents)
        .into_iter()
        .find(|s| s.id == "assignee")
        .unwrap();
    Wizard::new(WizardKind::Task, "T", vec![step])
}

/// A harness on the NEW TASK wizard's assignee step.
fn harness_at_assignee(width: u16, height: u16) -> Harness {
    let backend = orch_tui::backend::FakeBackend::new().with_agents(picker_roster());
    let mut harness = Harness::sized(backend, width, height);
    harness.key('t');
    harness.key('n');
    harness.type_str("Ship it");
    harness.press(KeyCode::Enter); // title
    harness.press(KeyCode::Enter); // priority
    assert_eq!(
        harness.app.state.wizard.as_ref().unwrap().step_id(),
        "assignee"
    );
    harness
}

#[test]
fn agent_picker_filters_as_you_type() {
    let mut wizard = assignee_wizard(&picker_roster());
    assert_eq!(
        labels(&wizard.options()),
        ["Auto-assign", "Alpha", "beta", "Gamma", "alpha-2"]
    );
    // Name, case-insensitively; `j` and `k` are filter text, not navigation.
    type_into(&mut wizard, "ALP", 0);
    assert_eq!(
        labels(&wizard.options()),
        ["Auto-assign", "Alpha", "alpha-2"]
    );
    assert_eq!(
        wizard.select_index(),
        1,
        "the cursor lands on the first match"
    );
    // Esc clears the filter and stays on the step.
    assert_eq!(
        wizard.handle_key(key(KeyCode::Esc), 0),
        WizardOutcome::Consumed
    );
    assert_eq!(labels(&wizard.options()).len(), 5);
    // Adapter, then role.
    type_into(&mut wizard, "codex", 0);
    assert_eq!(labels(&wizard.options()), ["Auto-assign", "beta"]);
    wizard.handle_key(key(KeyCode::Esc), 0);
    type_into(&mut wizard, "review", 0);
    assert_eq!(labels(&wizard.options()), ["Auto-assign", "Alpha"]);
    // Backspace edits the filter.
    for _ in 0.."review".len() {
        wizard.handle_key(key(KeyCode::Backspace), 0);
    }
    type_into(&mut wizard, "alpha", 0);
    // Digits still pick, by the number the filtered row shows.
    match wizard.handle_key(key(KeyCode::Char('3')), 0) {
        WizardOutcome::Complete(values) => assert_eq!(values["assignee"], "agt_e"),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn agent_picker_filter_row_and_status_render_on_screen() {
    let mut harness = harness_at_assignee(100, 30);
    harness.assert_text("> type to filter");
    harness.assert_text("Alpha ─ running · [claude] Frontend reviewer");
    harness.type_str("alp");
    harness.assert_text("> alp");
    harness.refute_text("beta");
    // Through the editor session: Backspace and Esc edit the filter before
    // they leave the step.
    harness.press(KeyCode::Backspace);
    harness.assert_text("> al█");
    harness.press(KeyCode::Esc);
    harness.assert_text("beta");
    let wizard = harness.app.state.wizard.as_ref().unwrap();
    assert_eq!(wizard.step_id(), "assignee");
    harness.refute_text("Leave this step?");
    // With the filter empty, Esc leaves the step: the title was typed, so
    // the wizard's leave guard asks first.
    harness.press(KeyCode::Esc);
    harness.assert_text("Leave this step?");
}

#[test]
fn agent_picker_rejects_free_text() {
    let mut wizard = assignee_wizard(&picker_roster());
    // Typed text that lists no agent is refused, even with the sentinel on
    // screen: Enter and Tab keep the step open with an error.
    type_into(&mut wizard, "nobody", 0);
    assert_eq!(labels(&wizard.options()), ["Auto-assign"]);
    for confirm in [KeyCode::Enter, KeyCode::Tab] {
        assert_eq!(wizard.handle_key(key(confirm), 0), WizardOutcome::Consumed);
        assert_eq!(wizard.step_id(), "assignee");
        assert!(wizard.visible_error().is_some(), "{confirm:?}");
    }
    // An agent id is not a name: it matches nothing, so it assigns nothing.
    wizard.handle_key(key(KeyCode::Esc), 0);
    type_into(&mut wizard, "agt_b", 0);
    assert_eq!(
        wizard.handle_key(key(KeyCode::Enter), 0),
        WizardOutcome::Consumed
    );
    // A pasted digit is filter text, not a pick.
    wizard.handle_key(key(KeyCode::Esc), 0);
    wizard.insert_paste("2", 0);
    assert_eq!(labels(&wizard.options()), ["Auto-assign", "alpha-2"]);
    // A typed name submits the matching agent's id; the sentinel needs its
    // own digit.
    match wizard.handle_key(key(KeyCode::Enter), 0) {
        WizardOutcome::Complete(values) => assert_eq!(values["assignee"], "agt_e"),
        other => panic!("expected completion, got {other:?}"),
    }
    let mut wizard = assignee_wizard(&picker_roster());
    type_into(&mut wizard, "nobody", 0);
    match wizard.handle_key(key(KeyCode::Char('1')), 0) {
        WizardOutcome::Complete(values) => assert_eq!(values["assignee"], ""),
        other => panic!("expected completion, got {other:?}"),
    }

    // A team lead has no sentinel: a filter that lists nobody cannot submit
    // an empty lead.
    let lead = configs::team_wizard_steps(&picker_roster(), &[])
        .into_iter()
        .find(|s| s.id == "lead")
        .unwrap();
    let mut wizard = Wizard::new(WizardKind::Team, "T", vec![lead]);
    type_into(&mut wizard, "zzz", 0);
    assert!(wizard.options().is_empty());
    assert_eq!(
        wizard.handle_key(key(KeyCode::Enter), 0),
        WizardOutcome::Consumed
    );
    assert_eq!(wizard.step_id(), "lead");
    assert!(wizard.visible_error().is_some());
}

#[test]
fn agent_picker_order_cycle() {
    let mut harness = harness_at_assignee(100, 30);
    let options = |h: &Harness| labels(&h.app.state.wizard.as_ref().unwrap().options());
    let selected = |h: &Harness| {
        let w = h.app.state.wizard.as_ref().unwrap();
        w.options()[w.select_index()].value.clone()
    };
    assert_eq!(
        options(&harness),
        ["Auto-assign", "Alpha", "beta", "Gamma", "alpha-2"]
    );
    harness.assert_text("Ctrl+O order: Original");
    // The cursor rests on `beta` and follows it through every order.
    harness.press(KeyCode::Down).press(KeyCode::Down);
    assert_eq!(selected(&harness), "agt_b");
    for (order, expected) in [
        ("Name", ["Auto-assign", "Alpha", "alpha-2", "beta", "Gamma"]),
        (
            "Adapter",
            ["Auto-assign", "Alpha", "beta", "alpha-2", "Gamma"],
        ),
        (
            "Status",
            ["Auto-assign", "beta", "alpha-2", "Alpha", "Gamma"],
        ),
        (
            "Original",
            ["Auto-assign", "Alpha", "beta", "Gamma", "alpha-2"],
        ),
    ] {
        harness.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(options(&harness), expected, "{order}");
        assert_eq!(selected(&harness), "agt_b", "{order}");
        harness.assert_text(&format!("Ctrl+O order: {order}"));
        harness.assert_text(&format!("order: {order} · Ctrl+O"));
    }
    // The order composes with the filter, sentinel still first.
    harness.press_mod(KeyCode::Char('o'), KeyModifiers::CONTROL);
    harness.type_str("a");
    assert_eq!(
        options(&harness),
        ["Auto-assign", "Alpha", "alpha-2", "beta", "Gamma"],
        "every agent's name, adapter or role holds an `a`"
    );
    harness.type_str("l");
    assert_eq!(options(&harness), ["Auto-assign", "Alpha", "alpha-2"]);
}

#[test]
fn agent_picker_team_members_keep_hidden_selections() {
    let members = configs::team_wizard_steps(&picker_roster(), &[])
        .into_iter()
        .find(|s| s.id == "members")
        .unwrap();
    let wizard = Wizard::new(WizardKind::Team, "T", vec![members]);
    let backend = orch_tui::backend::FakeBackend::new();
    let mut harness = Harness::sized(backend, 100, 30);
    harness.app.open_wizard(wizard);
    harness.type_str("alp");
    harness.key(' ');
    harness.press(KeyCode::Down);
    harness.key(' ');
    harness.press(KeyCode::Esc); // clear the filter, keep both ticks
    harness.type_str("beta");
    harness.key(' ');
    harness.assert_text("3 selected · 2 hidden by the filter");
    let wizard = harness.app.state.wizard.as_mut().unwrap();
    match wizard.handle_key(key(KeyCode::Enter), 0) {
        WizardOutcome::Complete(values) => assert_eq!(values["members"], "agt_a,agt_e,agt_b"),
        other => panic!("expected completion, got {other:?}"),
    }
}
