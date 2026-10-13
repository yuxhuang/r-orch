use std::collections::HashMap;

use indexmap::IndexMap;
use orch_core::template::{
    AgentContext, AgentInfo, GoalContext, MAX_CONTEXT_ENTRIES, MessageContext, ProjectContext,
    PromptContext, RetryContext, SharedContextEntry, TaskContext,
};
use orch_domain::{Agent, Message, OrchestratorConfig, Task};

/// Inputs to [`filter_relevant_context`].
#[derive(Debug, Clone, Default)]
pub struct ContextFilterInput<'a> {
    pub agent_name: &'a str,
    pub agent_role: Option<&'a str>,
    pub goal_id: Option<&'a str>,
    pub task_scope: Option<&'a [String]>,
}

/// JS `\s` — `char::is_whitespace` differs on U+0085 and U+FEFF.
fn is_js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

fn is_word_separator(c: char) -> bool {
    is_js_space(c) || c == '_' || c == '-'
}

fn first_word(s: &str) -> &str {
    s.split(is_word_separator).next().unwrap_or("")
}

/// JS `String.length` (UTF-16 code units).
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Keywords used for prefix matching, e.g. "Backend A" → `["backend"]`.
pub fn extract_role_keywords(agent_name_lower: &str, role: Option<&str>) -> Vec<String> {
    let mut keywords: Vec<String> = Vec::new();
    let name_first = first_word(agent_name_lower);
    if js_len(name_first) > 1 {
        keywords.push(name_first.to_owned());
    }
    if agent_name_lower.contains("front") || agent_name_lower.contains("tui") {
        keywords.extend(["front-end", "frontend", "tui"].map(String::from));
    }
    if agent_name_lower.contains("market") || agent_name_lower.contains("cmo") {
        keywords.extend(["marketer", "marketing", "cmo"].map(String::from));
    }
    if let Some(role) = role.filter(|r| !r.is_empty()) {
        let role_lower = role.to_lowercase();
        let word = first_word(&role_lower);
        if js_len(word) > 2 && !keywords.iter().any(|k| k == word) {
            keywords.push(word.to_owned());
        }
    }
    keywords
}

const GENERIC_PREFIXES: [&str; 6] = ["bug-", "perf-", "stability-", "docs-", "arch-", "spec-"];

fn has_generic_prefix(key: &str) -> bool {
    GENERIC_PREFIXES.iter().any(|p| {
        key.as_bytes()
            .get(..p.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(p.as_bytes()))
    })
}

fn scope_base(pattern: &str) -> String {
    let without_stars: String = pattern.chars().filter(|&c| c != '*').collect();
    without_stars.trim_end_matches('/').to_owned()
}

/// Score shared-context entries by relevance to the current agent/task and keep
/// at most [`MAX_CONTEXT_ENTRIES`] of them, best first. Ties keep input order
/// (the sort is stable, as V8's is). Values are never truncated.
pub fn filter_relevant_context(
    all_context: &IndexMap<String, String>,
    filter: &ContextFilterInput<'_>,
) -> Vec<SharedContextEntry> {
    if all_context.is_empty() {
        return Vec::new();
    }
    let agent_lower = filter.agent_name.to_lowercase();
    let role_keywords = extract_role_keywords(&agent_lower, filter.agent_role);
    let goal_lower = filter
        .goal_id
        .filter(|g| !g.is_empty())
        .map(str::to_lowercase);
    let scope_bases: Vec<String> = filter
        .task_scope
        .unwrap_or_default()
        .iter()
        .map(|p| scope_base(p).to_lowercase())
        .collect();

    let mut scored: Vec<(&String, &String, u32)> = all_context
        .iter()
        .map(|(key, value)| {
            let key_lower = key.to_lowercase();
            let value_lower = value.to_lowercase();
            let mut score = 0;
            if goal_lower
                .as_deref()
                .is_some_and(|g| key_lower.starts_with(g))
            {
                score += 10;
            }
            if key_lower.contains(&agent_lower) || value_lower.contains(&agent_lower) {
                score += 8;
            }
            if scope_bases
                .iter()
                .any(|b| !b.is_empty() && (key_lower.contains(b) || value_lower.contains(b)))
            {
                score += 6;
            }
            if role_keywords.iter().any(|kw| {
                key_lower.strip_prefix(kw.as_str()).is_some_and(|rest| {
                    rest.starts_with('-') || rest.starts_with('_')
                })
            }) {
                score += 4;
            }
            if has_generic_prefix(key) {
                score += 1;
            }
            (key, value, score)
        })
        .collect();

    scored.sort_by(|a, b| b.2.cmp(&a.2));

    let mut relevant: Vec<_> = scored
        .iter()
        .filter(|e| e.2 > 0)
        .take(MAX_CONTEXT_ENTRIES)
        .copied()
        .collect();
    if relevant.len() < MAX_CONTEXT_ENTRIES {
        let room = MAX_CONTEXT_ENTRIES - relevant.len();
        relevant.extend(scored.iter().filter(|e| e.2 == 0).take(room).copied());
    }
    relevant
        .into_iter()
        .map(|(k, v, _)| (k.clone(), v.clone()))
        .collect()
}

/// Optional inputs to [`build_prompt_context`].
#[derive(Debug, Clone, Default)]
pub struct BuildPromptOptions<'a> {
    pub all_agents: &'a [Agent],
    pub retry_context: Option<RetryContext>,
    pub shared_context: Option<&'a IndexMap<String, String>>,
    pub feedback: Option<String>,
    pub messages: &'a [Message],
    pub goal: Option<GoalContext>,
}

pub fn build_prompt_context(
    task: &Task,
    agent: &Agent,
    attempt: u32,
    workspace_path: &str,
    config: &OrchestratorConfig,
    options: BuildPromptOptions<'_>,
) -> PromptContext {
    let agent_by_id: HashMap<&str, &Agent> = options
        .all_agents
        .iter()
        .map(|a| (a.id.as_str(), a))
        .collect();
    let messages = (!options.messages.is_empty()).then(|| {
        options
            .messages
            .iter()
            .map(|m| MessageContext {
                id: m.id.clone(),
                from: agent_by_id
                    .get(m.from_agent_id.as_str())
                    .map_or_else(|| m.from_agent_id.clone(), |a| a.name.clone()),
                subject: m.subject.clone(),
                body: m.body.clone(),
                sent_at: m.created_at.clone(),
                reply_to: m.reply_to.clone(),
            })
            .collect()
    });

    let shared_context = options
        .shared_context
        .filter(|c| !c.is_empty())
        .map(|c| {
            filter_relevant_context(
                c,
                &ContextFilterInput {
                    agent_name: &agent.name,
                    agent_role: agent.role.as_deref(),
                    goal_id: task.goal_id.as_deref(),
                    task_scope: task.scope.as_deref(),
                },
            )
        });

    PromptContext {
        project: ProjectContext {
            name: config.project.name.clone(),
            description: config.project.description.clone(),
        },
        task: TaskContext {
            id: task.id.clone(),
            title: task.title.clone(),
            description: task.description.clone(),
            priority: task.priority,
            labels: task.labels.clone(),
            scope: task.scope.clone(),
            is_autonomous: task.is_autonomous(),
            goal_id: task.goal_id.clone(),
        },
        agent: AgentContext {
            id: agent.id.clone(),
            name: agent.name.clone(),
            role: agent.role.clone(),
        },
        agents: options
            .all_agents
            .iter()
            .map(|a| AgentInfo {
                id: a.id.clone(),
                name: a.name.clone(),
                role: if a.id == agent.id { None } else { a.role.clone() },
                adapter: a.adapter.clone(),
            })
            .collect(),
        attempt: (attempt > 1).then_some(i64::from(attempt)),
        workspace_path: workspace_path.to_owned(),
        retry: if attempt > 1 { options.retry_context } else { None },
        feedback: options.feedback,
        shared_context,
        messages,
        goal: options.goal,
    }
}
