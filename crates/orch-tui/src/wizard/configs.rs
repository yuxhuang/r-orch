//! The wizard catalogue — step definitions, skip
//! predicates, validators and the value → entity mappers.
//!
//! **Correctness is critical here**: an inverted skip predicate or a
//! wrong default writes corrupt `Task` / `Agent` / `Goal` / `Team` records
//! through the UI.

use orch_domain::{
    AGENT_SHOP_TEMPLATES, Agent, AgentShopTemplate, AgentStatus, Goal, ModelTier, Task, Team,
    TeamStatus, is_adapter_kind, resolve_model,
};

use crate::backend::{
    AgentFields, AgentOpts, CreateTaskOpts, GoalFields, ModelCatalog, ModelOption, TaskFields,
};
use crate::format::cap_line;
use crate::wizard::engine::{
    Choice, StepKind, SuggestionMode, Values, Wizard, WizardKind, WizardStep,
};
use crate::wizard::picker::AgentPicker;

/// Role sentinel meaning "type your own".
pub const CUSTOM_ROLE: &str = "__custom__";

/// Adapters that support `--effort`.
pub const EFFORT_ADAPTERS: [&str; 3] = ["claude", "pi", "grok"];

const AGENT_ROLE_MAX_LEN: usize = 60;

fn choices(rows: &[(&str, &str, &str)]) -> Vec<Choice> {
    rows.iter()
        .map(|(value, label, hint)| Choice::new(value, label, hint))
        .collect()
}

/// `ADAPTERS` — the provider select, in order. Shell is 8th, so `7 × j` reaches
/// it.
pub fn adapter_options() -> Vec<Choice> {
    choices(&[
        ("claude", "Claude", "Claude Code CLI"),
        ("opencode", "OpenCode", "OpenCode — multi-provider"),
        ("codex", "Codex", "OpenAI Codex CLI"),
        ("cursor", "Cursor", "Cursor Agent CLI"),
        ("pi", "Pi", "Pi coding agent RPC"),
        ("grok", "Grok", "Grok CLI"),
        ("antigravity", "Antigravity", "Google Antigravity CLI (agy)"),
        ("shell", "Shell", "custom shell command"),
    ])
}

/// `EFFORT_OPTIONS`.
pub fn effort_options() -> Vec<Choice> {
    choices(&[
        ("", "Default", "no override — use model default"),
        ("high", "High", "deepest reasoning, best quality, slowest"),
        ("medium", "Medium", "balanced speed and quality"),
        ("low", "Low", "fastest responses, minimal reasoning"),
    ])
}

/// `PRIORITY_OPTIONS`.
pub fn priority_options() -> Vec<Choice> {
    choices(&[
        ("1", "P1 Critical", "urgent, do first"),
        ("2", "P2 High", "important"),
        ("3", "P3 Medium", "default priority"),
        ("4", "P4 Low", "nice to have"),
    ])
}

/// `ROLE_PRESETS`.
pub fn role_presets() -> Vec<Choice> {
    choices(&[
        ("", "Skip", "no role description"),
        (
            "Full-stack developer",
            "Full-stack developer",
            "general purpose",
        ),
        ("Frontend developer", "Frontend developer", "React, CSS, UI"),
        (
            "Backend developer",
            "Backend developer",
            "APIs, databases, services",
        ),
        (
            "DevOps engineer",
            "DevOps engineer",
            "CI/CD, infra, deploys",
        ),
        (
            "QA / Test engineer",
            "QA / Test engineer",
            "testing, quality",
        ),
        ("Code reviewer", "Code reviewer", "review PRs, find bugs"),
        ("Technical writer", "Technical writer", "docs, READMEs"),
        (CUSTOM_ROLE, "Custom...", "type your own"),
    ])
}

/// `MAX_CONCURRENT_OPTIONS`.
pub fn max_concurrent_options() -> Vec<Choice> {
    [1u32, 2, 3, 4, 6, 8, 10]
        .iter()
        .map(|n| {
            let label = if *n == 1 {
                "1 agent".to_string()
            } else {
                format!("{n} agents")
            };
            let gb = format!("{:.1}", *n as f64 * 0.5);
            let gb = gb.trim_end_matches(".0").to_string();
            let subprocesses = if *n == 1 {
                "1 subprocess".to_string()
            } else {
                format!("{n} subprocesses")
            };
            Choice {
                value: n.to_string(),
                label,
                hint: format!("~{gb} GB RAM, {subprocesses}"),
            }
        })
        .collect()
}

/// `ACTIVITY_FILTER_OPTIONS`.
pub fn activity_filter_options() -> Vec<Choice> {
    choices(&[
        ("all", "All", "show everything"),
        ("text", "Text", "agent output only"),
        ("tools", "Tools", "tool calls, results, files"),
        ("errors", "Errors", "errors only"),
        ("events", "Events", "lifecycle, system events"),
    ])
}

/// `PALETTE_OPTIONS`.
pub fn palette_options() -> Vec<Choice> {
    choices(&[
        ("amber", "Amber", "warm ORCH default"),
        ("ocean", "Ocean", "cool blue control room"),
        ("forest", "Forest", "calm green operations"),
        ("violet", "Violet", "high-contrast purple"),
        (
            "light",
            "Light",
            "high-contrast palette for light terminals",
        ),
    ])
}

/// `TOGGLE_OPTIONS`.
pub fn toggle_options() -> Vec<Choice> {
    choices(&[("true", "On", ""), ("false", "Off", "")])
}

/// The built-in model options for an adapter, used when no catalog was discovered.
pub fn fallback_model_options(adapter: &str) -> Vec<ModelOption> {
    let rows: &[(&str, &str, &str)] = match adapter {
        "claude" => &[
            ("", "Default", "use Claude configured default"),
            ("opus", "Opus", "latest Opus alias"),
            ("sonnet", "Sonnet", "latest Sonnet alias"),
        ],
        "codex" => &[("", "Default", "use Codex configured default")],
        "cursor" => &[
            ("", "Default", "use Cursor configured default"),
            ("auto", "Auto", "let Cursor choose"),
        ],
        "opencode" => &[("", "Default", "use model configured in OpenCode")],
        "pi" => &[(
            "",
            "Default",
            "use Pi configured default; run /login in Pi if unavailable",
        )],
        "grok" => &[("", "Default", "use Grok configured default")],
        "antigravity" => &[("", "Default", "use Antigravity configured default")],
        "shell" => &[("", "Default", "model is not used by the shell adapter")],
        // An unknown adapter gets exactly one neutral option.
        _ => &[("", "Default", "use adapter default")],
    };
    rows.iter()
        .map(|(value, label, hint)| ModelOption {
            value: (*value).into(),
            label: (*label).into(),
            hint: (*hint).into(),
        })
        .collect()
}

/// The discovered catalog when non-empty,
/// otherwise the per-adapter fallback.
pub fn model_options(adapter: Option<&str>, catalog: &ModelCatalog) -> Vec<ModelOption> {
    let resolved = adapter.unwrap_or("claude");
    if !is_adapter_kind(resolved) {
        return fallback_model_options(resolved);
    }
    match catalog.get(resolved) {
        Some(list) if !list.is_empty() => list.clone(),
        _ => fallback_model_options(resolved),
    }
}

fn to_choices(options: Vec<ModelOption>) -> Vec<Choice> {
    options
        .into_iter()
        .map(|o| Choice {
            value: o.value,
            label: o.label,
            hint: o.hint,
        })
        .collect()
}

/// MCP skills use the `pack:skill` form and only work with the Claude CLI.
/// Inlined rather than taken from `orch-engine::agent_factory`: `orch-tui` must
/// not depend on the engine.
pub fn is_mcp_skill(skill: &str) -> bool {
    skill.contains(':')
}

/// Non-disabled agents, hint `[{adapter}] {first role line}`
/// capped at 60 chars.
pub fn agent_choices(agents: &[Agent]) -> Vec<Choice> {
    agents
        .iter()
        .filter(|a| a.status != AgentStatus::Disabled)
        .map(|a| {
            let role_line = a
                .role
                .as_deref()
                .unwrap_or("")
                .split('\n')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let role = cap_line(&role_line, AGENT_ROLE_MAX_LEN);
            let hint = if role.is_empty() {
                a.adapter.clone()
            } else {
                format!("[{}] {role}", a.adapter)
            };
            Choice {
                value: a.id.clone(),
                label: a.name.clone(),
                hint,
            }
        })
        .collect()
}

/// The sentinel plus the agent list.
pub fn agent_options_with_sentinel(
    agents: &[Agent],
    empty_label: &str,
    empty_hint: &str,
) -> Vec<Choice> {
    let mut out = vec![Choice::new("", empty_label, empty_hint)];
    out.extend(agent_choices(agents));
    out
}

/// `None` plus every **active** team.
pub fn team_options(teams: &[Team]) -> Vec<Choice> {
    let mut out = vec![Choice::new("", "None", "no team")];
    out.extend(
        teams
            .iter()
            .filter(|t| t.status == TeamStatus::Active)
            .map(|t| Choice {
                value: t.id.clone(),
                label: t.name.clone(),
                hint: format!("{} members", t.members.len()),
            }),
    );
    out
}

// ── Validators ───────────────────────────────────

pub const TITLE_REQUIRED: &str = "Title is required";
pub const PRIORITY_RANGE: &str = "Priority must be 1-4";
pub const NAME_REQUIRED: &str = "Name is required";
pub const AGENT_NAME_TAKEN: &str = "Agent with this name already exists";
pub const TEAM_NAME_TAKEN: &str = "Team with this name already exists";
pub const COMMAND_REQUIRED_FOR_SHELL: &str = "Command is required for Shell";

pub fn validate_title(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        Some(TITLE_REQUIRED.into())
    } else {
        None
    }
}

pub fn validate_priority(value: &str) -> Option<String> {
    // `Number(v)`: reject non-integers and anything outside 1..=4.
    match value.trim().parse::<f64>() {
        Ok(n) if n.fract() == 0.0 && (1.0..=4.0).contains(&n) => None,
        _ => Some(PRIORITY_RANGE.into()),
    }
}

pub fn validate_agent_name(
    value: &str,
    agents: &[Agent],
    exclude_id: Option<&str>,
) -> Option<String> {
    if value.trim().is_empty() {
        return Some(NAME_REQUIRED.into());
    }
    let taken = agents
        .iter()
        .filter(|a| exclude_id != Some(a.id.as_str()))
        .any(|a| a.name == value.trim());
    if taken {
        Some(AGENT_NAME_TAKEN.into())
    } else {
        None
    }
}

pub fn validate_team_name(value: &str, teams: &[Team]) -> Option<String> {
    if value.trim().is_empty() {
        return Some(NAME_REQUIRED.into());
    }
    if teams.iter().any(|t| t.name == value.trim()) {
        Some(TEAM_NAME_TAKEN.into())
    } else {
        None
    }
}

pub fn validate_shell_command(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        Some(COMMAND_REQUIRED_FOR_SHELL.into())
    } else {
        None
    }
}

fn value_of(values: &Values, key: &str) -> String {
    values.get(key).cloned().unwrap_or_default()
}

// ── NEW AGENT ──────────────────────────────────────────────────────────────

/// Ten
/// steps, six conditional.
pub fn agent_wizard_steps(
    agents: &[Agent],
    teams: &[Team],
    catalog: &ModelCatalog,
    default_adapter: &str,
) -> Vec<WizardStep> {
    let team_opts = team_options(teams);
    let team_opts_len = team_opts.len();
    let agents_owned = agents.to_vec();
    let catalog_owned = catalog.clone();

    vec![
        WizardStep::new("name", "Agent name", StepKind::Text)
            .placeholder("e.g. alpha, frontend-bot, reviewer")
            .required()
            .validate(move |v| validate_agent_name(v, &agents_owned, None))
            .suggestions(shop_template_choices()),
        WizardStep::new("adapter", "Provider", StepKind::Select)
            .options(adapter_options())
            .default_value(default_adapter),
        WizardStep::new("command", "Command", StepKind::Text)
            .placeholder("e.g. npm test or bash scripts/deploy.sh")
            .description("Runs from task workspace · exit 0 = done · non-zero = failed")
            .required()
            .skip(|v| value_of(v, "adapter") != "shell")
            .validate(validate_shell_command),
        WizardStep::new("model", "Model", StepKind::Text)
            .placeholder("Default, type a model ID, or press ↓ to browse")
            .get_suggestions(move |v| {
                to_choices(model_options(
                    v.get("adapter").map(String::as_str),
                    &catalog_owned,
                ))
            })
            .suggestion_mode(SuggestionMode::Value)
            .suggestions_label("available models")
            .skip(|v| value_of(v, "adapter") == "shell"),
        WizardStep::new("effort", "Reasoning effort", StepKind::Select)
            .options(effort_options())
            .skip(|v| !EFFORT_ADAPTERS.contains(&value_of(v, "adapter").as_str())),
        WizardStep::new("role", "Role / specialization", StepKind::Select)
            .options(role_presets())
            .skip(|v| value_of(v, "adapter") == "shell"),
        WizardStep::new("role_custom", "Describe the role", StepKind::Textarea)
            .placeholder("e.g. Specialist in React and TypeScript")
            .skip(|v| value_of(v, "adapter") == "shell" || value_of(v, "role") != CUSTOM_ROLE),
        WizardStep::new("skills", "Skills (comma-separated)", StepKind::Text)
            .placeholder("e.g. feature-dev:feature-dev, testing-suite:generate-tests")
            .skip(|v| value_of(v, "adapter") == "shell"),
        // Hidden — only populated by shop templates.
        WizardStep::new("approval_policy", "Approval policy", StepKind::Text).always_skip(),
        WizardStep::new("team", "Join team", StepKind::Select)
            .options(team_opts)
            .skip(move |v| value_of(v, "adapter") == "shell" || team_opts_len <= 1),
    ]
}

/// The agent-name step's suggestions: all 15 shop templates, callback mode.
pub fn shop_template_choices() -> Vec<Choice> {
    AGENT_SHOP_TEMPLATES
        .iter()
        .map(|t| Choice::new(t.key, t.name, t.description))
        .collect()
}

/// The NEW AGENT wizard's values mapped to an agent input.
pub struct AgentWizardInput {
    pub name: String,
    pub adapter: String,
    pub opts: AgentOpts,
    pub team_id: Option<String>,
}

pub fn agent_wizard_to_input(values: &Values, default_adapter: &str) -> AgentWizardInput {
    let adapter = {
        let a = value_of(values, "adapter");
        if a.is_empty() {
            default_adapter.to_string()
        } else {
            a
        }
    };
    let is_shell = adapter == "shell";

    let role = if is_shell {
        None
    } else if value_of(values, "role") == CUSTOM_ROLE {
        non_empty(value_of(values, "role_custom"))
    } else {
        non_empty(value_of(values, "role"))
    };

    let skills = if is_shell {
        None
    } else {
        let raw = value_of(values, "skills");
        if raw.is_empty() {
            None
        } else {
            let list: Vec<String> = raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            Some(list)
        }
    };

    // Shell agents are forced to `auto` regardless of input.
    let approval_policy = if is_shell {
        Some("auto".to_string())
    } else {
        Some(non_empty(value_of(values, "approval_policy")).unwrap_or_else(|| "auto".into()))
    };

    AgentWizardInput {
        name: value_of(values, "name"),
        adapter: adapter.clone(),
        opts: AgentOpts {
            command: if is_shell {
                non_empty(value_of(values, "command").trim().to_string())
            } else {
                None
            },
            model: if is_shell {
                None
            } else {
                non_empty(value_of(values, "model"))
            },
            effort: if is_shell {
                None
            } else {
                non_empty(value_of(values, "effort"))
            },
            role,
            approval_policy,
            skills,
        },
        team_id: if is_shell {
            None
        } else {
            non_empty(value_of(values, "team"))
        },
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

// ── AGENT SHOP ─────────────────────────────────────────────────────────────

/// One select step over the 15 templates.
pub fn shop_wizard_steps() -> Vec<WizardStep> {
    vec![
        WizardStep::new(
            "shop_template",
            "Agent Shop — choose a template",
            StepKind::Select,
        )
        .options(shop_template_choices()),
    ]
}

/// Pre-fill the NEW AGENT steps from an agent-shop template.
pub fn apply_shop_template(
    base_steps: Vec<WizardStep>,
    template: &AgentShopTemplate,
    default_adapter: &str,
) -> Vec<WizardStep> {
    let preferred = resolve_model(default_adapter, template.tier);
    let available = {
        let mut probe = Values::new();
        probe.insert("adapter".into(), default_adapter.to_string());
        base_steps
            .iter()
            .find(|s| s.id == "model")
            .map(|step| {
                let suggestions = step.suggestions.resolve(&probe);
                if suggestions.is_empty() {
                    step.options.resolve(&probe)
                } else {
                    suggestions
                }
            })
            .unwrap_or_default()
    };
    let resolved_model = if available.iter().any(|o| o.value == preferred) {
        preferred.to_string()
    } else if available.iter().any(|o| o.value.is_empty()) {
        String::new()
    } else {
        available
            .first()
            .map(|o| o.value.clone())
            .unwrap_or_default()
    };

    let skills: Vec<&str> = if default_adapter == "claude" {
        template.skills.to_vec()
    } else {
        template
            .skills
            .iter()
            .copied()
            .filter(|s| !is_mcp_skill(s))
            .collect()
    };

    base_steps
        .into_iter()
        .map(|step| match step.id.as_str() {
            "name" => step.default_value(template.name),
            "adapter" => step.default_value(default_adapter),
            "model" => step.default_value(&resolved_model),
            "role" => step.default_value(CUSTOM_ROLE),
            "role_custom" => step.default_value(template.role),
            "skills" => step.default_value(&skills.join(", ")),
            "approval_policy" => step.default_value(template.approval_policy.as_str()),
            _ => step,
        })
        .collect()
}

// ── EDIT AGENT ─────────────────────────────────────────────────────────────

/// The EDIT AGENT steps: eight, being the NEW AGENT ids minus `skills` and
/// `approval_policy`.
///
/// Every skip predicate falls back to `agent.adapter` when the user has not
/// (yet) chosen one.
pub fn edit_agent_wizard_steps(
    agent: &Agent,
    agents: &[Agent],
    teams: &[Team],
    catalog: &ModelCatalog,
) -> Vec<WizardStep> {
    let presets = role_presets();
    let role_in_presets = agent
        .role
        .as_deref()
        .is_some_and(|r| presets.iter().any(|p| p.value == r));
    let role_default = if role_in_presets {
        agent.role.clone().unwrap_or_default()
    } else if agent.role.as_deref().is_some_and(|r| !r.is_empty()) {
        CUSTOM_ROLE.to_string()
    } else {
        String::new()
    };
    let role_custom_default = if !role_in_presets {
        agent.role.clone().unwrap_or_default()
    } else {
        String::new()
    };

    let team_opts = team_options(teams);
    let team_opts_len = team_opts.len();
    let current_team = teams
        .iter()
        .find(|t| t.members.iter().any(|m| m.agent_id == agent.id))
        .map(|t| t.id.clone())
        .unwrap_or_default();

    let agent_adapter = agent.adapter.clone();
    let adapter_or = {
        let fallback = agent_adapter.clone();
        move |v: &Values| -> String {
            let chosen = value_of(v, "adapter");
            if chosen.is_empty() {
                fallback.clone()
            } else {
                chosen
            }
        }
    };

    let agents_owned = agents.to_vec();
    let agent_id = agent.id.clone();
    let catalog_owned = catalog.clone();
    let model_default = agent.config.model.clone().unwrap_or_default();

    let a1 = adapter_or.clone();
    let a2 = adapter_or.clone();
    let a3 = adapter_or.clone();
    let a4 = adapter_or.clone();
    let a5 = adapter_or.clone();
    let a6 = adapter_or.clone();
    let a7 = adapter_or.clone();
    let a8 = adapter_or.clone();
    let agent_adapter_for_model = agent.adapter.clone();

    vec![
        WizardStep::new("name", "Agent name", StepKind::Text)
            .default_value(&agent.name)
            .required()
            .validate(move |v| validate_agent_name(v, &agents_owned, Some(&agent_id))),
        WizardStep::new("adapter", "Provider", StepKind::Select)
            .options(adapter_options())
            .default_value(&agent.adapter),
        WizardStep::new("command", "Command", StepKind::Text)
            .default_value(agent.config.command.as_deref().unwrap_or(""))
            .placeholder("e.g. npm test or bash scripts/deploy.sh")
            .description("Runs from task workspace · exit 0 = done · non-zero = failed")
            .required()
            .skip(move |v| a1(v) != "shell")
            .validate(validate_shell_command),
        WizardStep::new("model", "Model", StepKind::Text)
            .placeholder("Default, type a model ID, or press ↓ to browse")
            .get_suggestions(move |v| {
                let adapter = a2(v);
                to_choices(model_options(Some(&adapter), &catalog_owned))
            })
            .suggestion_mode(SuggestionMode::Value)
            .suggestions_label("available models")
            // Changing adapter clears the model.
            .get_default_value(move |v| {
                if a3(v) == agent_adapter_for_model {
                    model_default.clone()
                } else {
                    String::new()
                }
            })
            .skip(move |v| a4(v) == "shell"),
        WizardStep::new("effort", "Reasoning effort", StepKind::Select)
            .options(effort_options())
            .default_value(agent.config.effort.map(|e| e.as_str()).unwrap_or(""))
            .skip(move |v| !EFFORT_ADAPTERS.contains(&a5(v).as_str())),
        WizardStep::new("role", "Role / specialization", StepKind::Select)
            .options(role_presets())
            .default_value(&role_default)
            .skip(move |v| a6(v) == "shell"),
        WizardStep::new("role_custom", "Describe the role", StepKind::Textarea)
            .default_value(&role_custom_default)
            .placeholder("e.g. Specialist in React and TypeScript")
            .skip(move |v| a7(v) == "shell" || value_of(v, "role") != CUSTOM_ROLE),
        WizardStep::new("team", "Team", StepKind::Select)
            .options(team_opts)
            .default_value(&current_team)
            .skip(move |v| a8(v) == "shell" || team_opts_len <= 1),
    ]
}

/// The EDIT AGENT wizard's values mapped to the fields to change.
///
/// `effort` is preserved as `""` so the field can be *cleared* — deliberately
/// different from the create mapper, which drops empties.
///
/// `role` is **not** clearable: the `Skip` preset maps to `None`
/// ("unchanged"). The engine would accept a clear, but making `Skip` erase a
/// stored role would change what the same keystrokes write to existing data.
pub fn edit_agent_wizard_to_fields(values: &Values) -> (AgentFields, Option<String>) {
    let adapter = values.get("adapter").cloned();
    let is_shell = adapter.as_deref() == Some("shell");
    let role = if value_of(values, "role") == CUSTOM_ROLE {
        non_empty(value_of(values, "role_custom"))
    } else {
        non_empty(value_of(values, "role"))
    };

    let fields = AgentFields {
        name: values.get("name").cloned(),
        adapter: adapter.clone(),
        role: if is_shell { None } else { role },
        command: if is_shell {
            Some(value_of(values, "command").trim().to_string())
        } else if adapter.is_some() {
            // Switching away from shell clears the command.
            Some(String::new())
        } else {
            None
        },
        model: if is_shell {
            None
        } else {
            values.get("model").cloned()
        },
        effort: if is_shell {
            None
        } else {
            values.get("effort").cloned()
        },
    };
    let team_id = if is_shell {
        None
    } else {
        non_empty(value_of(values, "team"))
    };
    (fields, team_id)
}

// ── NEW / EDIT TASK ────────────────────────────────────────────────────────

/// The NEW TASK steps.
pub fn task_wizard_steps(agents: &[Agent]) -> Vec<WizardStep> {
    let options =
        agent_options_with_sentinel(agents, "Auto-assign", "orchestrator picks the best agent");
    let options_len = options.len();
    vec![
        WizardStep::new("title", "Task title", StepKind::Text)
            .placeholder("What needs to be done?")
            .required()
            .validate(validate_title),
        WizardStep::new("priority", "Priority", StepKind::Select)
            .options(priority_options())
            .default_value("3")
            .validate(validate_priority),
        WizardStep::new("assignee", "Assignee", StepKind::Select)
            .options(options)
            .agent_picker(AgentPicker::new(agents))
            .skip(move |_| options_len <= 1),
        WizardStep::new("description", "Description", StepKind::Textarea)
            .placeholder("Optional details, context, acceptance criteria..."),
    ]
}

/// The EDIT TASK steps.
pub fn edit_task_wizard_steps(task: &Task, agents: &[Agent]) -> Vec<WizardStep> {
    let options = agent_options_with_sentinel(agents, "None / Auto", "remove assignee");
    let options_len = options.len();
    vec![
        WizardStep::new("title", "Task title", StepKind::Text)
            .default_value(&task.title)
            .required()
            .validate(validate_title),
        WizardStep::new("priority", "Priority", StepKind::Select)
            .options(priority_options())
            .default_value(&task.priority.to_string())
            .validate(validate_priority),
        WizardStep::new("assignee", "Assignee", StepKind::Select)
            .options(options)
            .agent_picker(AgentPicker::new(agents))
            .default_value(task.assignee.as_deref().unwrap_or(""))
            .skip(move |_| options_len <= 1),
        WizardStep::new("description", "Description", StepKind::Textarea)
            .default_value(&task.description)
            .placeholder("Optional details..."),
    ]
}

/// Plus the chosen assignee, which the submit path
/// applies with a second call.
pub fn task_wizard_to_input(values: &Values) -> (String, CreateTaskOpts, Option<String>) {
    let title = value_of(values, "title");
    let opts = CreateTaskOpts {
        priority: non_empty(value_of(values, "priority")).and_then(|p| p.parse::<i64>().ok()),
        description: non_empty(value_of(values, "description")),
        attachments: None,
    };
    (title, opts, non_empty(value_of(values, "assignee")))
}

/// `description` is always written (`?? ''`)
/// so it can be cleared.
pub fn edit_task_wizard_to_fields(values: &Values) -> (TaskFields, Option<String>) {
    let fields = TaskFields {
        title: values.get("title").cloned(),
        priority: non_empty(value_of(values, "priority")).and_then(|p| p.parse::<i64>().ok()),
        description: Some(value_of(values, "description")),
        attachments: None,
    };
    (fields, non_empty(value_of(values, "assignee")))
}

// ── NEW TEAM ───────────────────────────────────────────────────────────────

/// The NEW TEAM steps.
pub fn team_wizard_steps(agents: &[Agent], teams: &[Team]) -> Vec<WizardStep> {
    let agent_opts = agent_choices(agents);
    let teams_owned = teams.to_vec();
    let for_members = agent_opts.clone();
    let for_skip = agent_opts.clone();

    vec![
        WizardStep::new("name", "Team name", StepKind::Text)
            .placeholder("e.g. frontend, backend, qa")
            .required()
            .validate(move |v| validate_team_name(v, &teams_owned)),
        // No empty sentinel — a team must have a lead.
        WizardStep::new("lead", "Team lead", StepKind::Select)
            .options(agent_opts)
            .agent_picker(AgentPicker::new(agents)),
        WizardStep::new("members", "Team members", StepKind::Multiselect)
            .agent_picker(AgentPicker::new(agents))
            .get_options(move |v| {
                let lead = value_of(v, "lead");
                for_members
                    .iter()
                    .filter(|a| a.value != lead)
                    .cloned()
                    .collect()
            })
            .skip(move |v| {
                let lead = value_of(v, "lead");
                !for_skip.iter().any(|a| a.value != lead)
            }),
        WizardStep::new("description", "Description", StepKind::Textarea)
            .placeholder("Optional team purpose..."),
    ]
}

/// The NEW TEAM wizard's values mapped to a team input.
pub fn team_wizard_to_input(values: &Values) -> orch_domain::CreateTeamInput {
    let members: Vec<String> = value_of(values, "members")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    orch_domain::CreateTeamInput {
        name: value_of(values, "name"),
        description: non_empty(value_of(values, "description")),
        lead_agent_id: value_of(values, "lead"),
        member_agent_ids: if members.is_empty() {
            None
        } else {
            Some(members)
        },
        config: None,
    }
}

// ── GOALS ──────────────────────────────────────────────────────────────────

const GOAL_TITLE_DESC: &str = "Be specific — agents work better with clear, measurable objectives";
const GOAL_ASSIGNEE_DESC: &str =
    "Assigned agent gets autonomous mode — it will plan and execute without prompts";
const GOAL_DESCRIPTION_DESC: &str =
    "Context matters — include tech stack, constraints, and what \"done\" looks like";

/// The NEW GOAL steps.
pub fn goal_wizard_steps(agents: &[Agent]) -> Vec<WizardStep> {
    let options =
        agent_options_with_sentinel(agents, "Any agent", "auto-assign to autonomous agents");
    let options_len = options.len();
    vec![
        WizardStep::new("title", "Goal title", StepKind::Text)
            .placeholder("e.g. \"Implement OAuth2 login with Google and GitHub\"")
            .description(GOAL_TITLE_DESC)
            .required()
            .validate(validate_title),
        WizardStep::new("assignee", "Assignee", StepKind::Select)
            .description(GOAL_ASSIGNEE_DESC)
            .options(options)
            .agent_picker(AgentPicker::new(agents))
            .skip(move |_| options_len <= 1),
        WizardStep::new("description", "Description", StepKind::Textarea)
            .placeholder("Success criteria, constraints, technical context...")
            .description(GOAL_DESCRIPTION_DESC),
    ]
}

/// The EDIT GOAL steps — the sentinel hint becomes
/// `auto-assign`.
pub fn edit_goal_wizard_steps(goal: &Goal, agents: &[Agent]) -> Vec<WizardStep> {
    let options = agent_options_with_sentinel(agents, "Any agent", "auto-assign");
    let options_len = options.len();
    vec![
        WizardStep::new("title", "Goal title", StepKind::Text)
            .default_value(&goal.title)
            .description(GOAL_TITLE_DESC)
            .required()
            .validate(validate_title),
        WizardStep::new("assignee", "Assignee", StepKind::Select)
            .description(GOAL_ASSIGNEE_DESC)
            .options(options)
            .agent_picker(AgentPicker::new(agents))
            .default_value(goal.assignee.as_deref().unwrap_or(""))
            .skip(move |_| options_len <= 1),
        WizardStep::new("description", "Description", StepKind::Textarea)
            .default_value(&goal.description)
            .placeholder("Success criteria, constraints, technical context...")
            .description(GOAL_DESCRIPTION_DESC),
    ]
}

/// The NEW GOAL wizard's values mapped to a goal input.
pub fn goal_wizard_to_input(values: &Values) -> orch_domain::CreateGoalInput {
    orch_domain::CreateGoalInput {
        title: value_of(values, "title"),
        description: non_empty(value_of(values, "description")),
        assignee: non_empty(value_of(values, "assignee")),
    }
}

/// `description` is always written.
pub fn edit_goal_wizard_to_fields(values: &Values) -> GoalFields {
    GoalFields {
        title: values.get("title").cloned(),
        description: Some(value_of(values, "description")),
        assignee: non_empty(value_of(values, "assignee")),
    }
}

// ── SETTINGS ───────────────────────────────────────────────────────────────

/// Always exactly one step. The step id is
/// the setting with hyphens replaced by underscores.
pub fn config_wizard_steps(
    setting: &str,
    palette: &str,
    filter: &str,
    max_concurrent: u32,
    notifications: orch_domain::NotificationPreferences,
) -> Vec<WizardStep> {
    match setting {
        "palette" => vec![
            WizardStep::new("palette", "Color palette", StepKind::Select)
                .options(palette_options())
                .default_value(palette),
        ],
        "activity-filter" => vec![
            WizardStep::new(
                "activity_filter",
                "Activity filter preset",
                StepKind::Select,
            )
            .options(activity_filter_options())
            .default_value(filter),
        ],
        "max-concurrent" => vec![
            WizardStep::new("max_concurrent", "Max concurrent agents", StepKind::Select)
                .options(max_concurrent_options())
                .default_value(&max_concurrent.to_string()),
        ],
        "notifications-toast" => vec![
            WizardStep::new(
                "notifications_toast",
                "Toast notifications",
                StepKind::Select,
            )
            .options(toggle_options())
            .default_value(if notifications.toast { "true" } else { "false" }),
        ],
        "notifications-bell" => vec![
            WizardStep::new("notifications_bell", "Bell on completion", StepKind::Select)
                .options(toggle_options())
                .default_value(if notifications.bell { "true" } else { "false" }),
        ],
        _ => Vec::new(),
    }
}

/// `SETTINGS — {SETTING}` with hyphens replaced by spaces, uppercased.
pub fn config_wizard_title(setting: &str) -> String {
    format!("SETTINGS — {}", setting.replace('-', " ").to_uppercase())
}

// ── Wizard constructors ────────────────────────────────────────────────────

pub fn new_agent_wizard(
    agents: &[Agent],
    teams: &[Team],
    catalog: &ModelCatalog,
    default_adapter: &str,
) -> Wizard {
    Wizard::new(
        WizardKind::Agent,
        "NEW AGENT",
        agent_wizard_steps(agents, teams, catalog, default_adapter),
    )
}

pub fn agent_shop_wizard() -> Wizard {
    Wizard::new(WizardKind::AgentShop, "AGENT SHOP", shop_wizard_steps())
}

pub fn agent_from_shop_wizard(
    template: &AgentShopTemplate,
    agents: &[Agent],
    teams: &[Team],
    catalog: &ModelCatalog,
    default_adapter: &str,
) -> Wizard {
    let base = agent_wizard_steps(agents, teams, catalog, default_adapter);
    let steps = apply_shop_template(base, template, default_adapter);
    Wizard::new(
        WizardKind::AgentFromShop,
        &format!("NEW AGENT — {}", template.name),
        steps,
    )
    .with_template(template.key)
}

pub fn edit_agent_wizard(
    agent: &Agent,
    agents: &[Agent],
    teams: &[Team],
    catalog: &ModelCatalog,
) -> Wizard {
    Wizard::new(
        WizardKind::EditAgent,
        "EDIT AGENT",
        edit_agent_wizard_steps(agent, agents, teams, catalog),
    )
    .with_target(&agent.id)
}

pub fn new_task_wizard(agents: &[Agent]) -> Wizard {
    Wizard::new(WizardKind::Task, "NEW TASK", task_wizard_steps(agents)).with_paste(true)
}

pub fn edit_task_wizard(task: &Task, agents: &[Agent]) -> Wizard {
    Wizard::new(
        WizardKind::EditTask,
        "EDIT TASK",
        edit_task_wizard_steps(task, agents),
    )
    .with_target(&task.id)
    .with_paste(true)
}

pub fn new_team_wizard(agents: &[Agent], teams: &[Team]) -> Wizard {
    Wizard::new(
        WizardKind::Team,
        "NEW TEAM",
        team_wizard_steps(agents, teams),
    )
}

pub fn new_goal_wizard(agents: &[Agent]) -> Wizard {
    Wizard::new(WizardKind::Goal, "New Goal", goal_wizard_steps(agents))
}

pub fn edit_goal_wizard(goal: &Goal, agents: &[Agent]) -> Wizard {
    Wizard::new(
        WizardKind::EditGoal,
        &format!("Edit Goal: {}", goal.title),
        edit_goal_wizard_steps(goal, agents),
    )
    .with_target(&goal.id)
}

pub fn config_wizard(
    setting: &str,
    palette: &str,
    filter: &str,
    max_concurrent: u32,
    notifications: orch_domain::NotificationPreferences,
) -> Wizard {
    Wizard::new(
        WizardKind::Config,
        &config_wizard_title(setting),
        config_wizard_steps(setting, palette, filter, max_concurrent, notifications),
    )
    .with_setting(setting)
}

/// `resolve_model` wrapper used by the shop path, kept here so the tier → model
/// mapping has one call site.
pub fn model_for_tier(adapter: &str, tier: ModelTier) -> &'static str {
    resolve_model(adapter, tier)
}
