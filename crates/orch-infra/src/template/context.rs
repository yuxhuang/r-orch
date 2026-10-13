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
                key_lower
                    .strip_prefix(kw.as_str())
                    .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('_'))
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

    let shared_context = options.shared_context.filter(|c| !c.is_empty()).map(|c| {
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
                role: if a.id == agent.id {
                    None
                } else {
                    a.role.clone()
                },
                adapter: a.adapter.clone(),
            })
            .collect(),
        attempt: (attempt > 1).then_some(i64::from(attempt)),
        workspace_path: workspace_path.to_owned(),
        retry: if attempt > 1 {
            options.retry_context
        } else {
            None
        },
        feedback: options.feedback,
        shared_context,
        messages,
        goal: options.goal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orch_core::test_support::{make_agent, make_task};
    use orch_domain::{MessageChannel, MessageStatus, default_config};

    fn retry() -> RetryContext {
        RetryContext {
            previous_error: "boom".into(),
            previous_output: "out".into(),
        }
    }

    fn build(attempt: u32, options: BuildPromptOptions<'_>) -> PromptContext {
        build_prompt_context(
            &make_task(),
            &make_agent(),
            attempt,
            "/ws",
            &default_config(),
            options,
        )
    }

    fn ctx(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn keys(entries: &[SharedContextEntry]) -> Vec<&str> {
        entries.iter().map(|(k, _)| k.as_str()).collect()
    }

    fn filter<'a>(name: &'a str) -> ContextFilterInput<'a> {
        ContextFilterInput {
            agent_name: name,
            ..Default::default()
        }
    }

    #[test]
    fn first_attempt_has_no_attempt_or_retry_even_when_supplied() {
        let c = build(
            1,
            BuildPromptOptions {
                retry_context: Some(retry()),
                ..Default::default()
            },
        );
        assert_eq!(c.attempt, None);
        assert_eq!(c.retry, None);
    }

    #[test]
    fn later_attempt_carries_the_retry_context() {
        let c = build(
            2,
            BuildPromptOptions {
                retry_context: Some(retry()),
                ..Default::default()
            },
        );
        assert_eq!(c.attempt, Some(2));
        assert_eq!(c.retry, Some(retry()));
        assert_eq!(build(2, BuildPromptOptions::default()).retry, None);
    }

    #[test]
    fn team_listing_blanks_only_the_current_agents_role() {
        let me = Agent {
            role: Some("Mine".into()),
            ..make_agent()
        };
        let other = Agent {
            id: "agt_other".into(),
            name: "other".into(),
            role: Some("Senior Backend Developer\n## WORKFLOW\nx".into()),
            ..make_agent()
        };
        let no_role = Agent {
            id: "agt_none".into(),
            role: None,
            ..make_agent()
        };
        let all = [me.clone(), other.clone(), no_role];
        let c = build_prompt_context(
            &make_task(),
            &me,
            1,
            "/ws",
            &default_config(),
            BuildPromptOptions {
                all_agents: &all,
                ..Default::default()
            },
        );
        assert_eq!(c.agent.role.as_deref(), Some("Mine"));
        assert_eq!(c.agents[0].role, None);
        assert_eq!(c.agents[1].role, other.role);
        assert_eq!(c.agents[2].role, None);
    }

    #[test]
    fn messages_resolve_sender_names_and_empty_means_none() {
        let sender = Agent {
            id: "agt_s".into(),
            name: "Sender".into(),
            ..make_agent()
        };
        let msg = |from: &str| Message {
            id: "msg_1".into(),
            channel: MessageChannel::Direct,
            from_agent_id: from.into(),
            to_agent_id: None,
            subject: "s".into(),
            body: "b".into(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            expires_at: None,
            status: MessageStatus::Pending,
            delivered_at: None,
            team_id: None,
            reply_to: None,
        };
        let all = [sender];
        let c = build(
            1,
            BuildPromptOptions {
                all_agents: &all,
                messages: &[msg("agt_s"), msg("agt_unknown")],
                ..Default::default()
            },
        );
        let m = c.messages.unwrap();
        assert_eq!(m[0].from, "Sender");
        assert_eq!(m[1].from, "agt_unknown");
        assert_eq!(build(1, BuildPromptOptions::default()).messages, None);
    }

    #[test]
    fn autonomous_label_and_goal_id_flow_into_the_task_context() {
        let task = Task {
            labels: vec!["autonomous".into()],
            goal_id: Some("goal_1".into()),
            ..make_task()
        };
        let c = build_prompt_context(
            &task,
            &make_agent(),
            1,
            "/ws",
            &default_config(),
            BuildPromptOptions::default(),
        );
        assert!(c.task.is_autonomous);
        assert_eq!(c.task.goal_id.as_deref(), Some("goal_1"));
    }

    #[test]
    fn shared_context_is_filtered_and_empty_means_none() {
        let all = ctx(&[("a", "1")]);
        let c = build(
            1,
            BuildPromptOptions {
                shared_context: Some(&all),
                ..Default::default()
            },
        );
        assert_eq!(c.shared_context, Some(vec![("a".into(), "1".into())]));
        let empty = IndexMap::new();
        let c = build(
            1,
            BuildPromptOptions {
                shared_context: Some(&empty),
                ..Default::default()
            },
        );
        assert_eq!(c.shared_context, None);
    }

    #[test]
    fn filter_ranks_goal_then_agent_then_scope_then_role_then_generic() {
        let all = ctx(&[
            ("zz", "none"),
            ("bug-1", "x"),
            ("backend-x", "x"),
            ("api", "x"),
            ("n", "mentions backend a"),
            ("goal_1-progress", "x"),
        ]);
        let scope = vec!["api/**".to_owned()];
        let got = filter_relevant_context(
            &all,
            &ContextFilterInput {
                agent_name: "Backend A",
                goal_id: Some("goal_1"),
                task_scope: Some(&scope),
                ..Default::default()
            },
        );
        assert_eq!(
            keys(&got),
            ["goal_1-progress", "n", "api", "backend-x", "bug-1", "zz"]
        );
    }

    #[test]
    fn filter_caps_at_fifteen_and_pads_with_unscored_entries() {
        let pairs: Vec<(String, String)> =
            (0..30).map(|i| (format!("k{i:02}"), "v".into())).collect();
        let all: IndexMap<_, _> = pairs.into_iter().collect();
        let got = filter_relevant_context(&all, &filter("Zed"));
        assert_eq!(got.len(), MAX_CONTEXT_ENTRIES);
        assert_eq!(got[0].0, "k00");
    }

    #[test]
    fn long_values_are_never_truncated() {
        let long = "x".repeat(600);
        let all = ctx(&[("k", &long)]);
        assert_eq!(filter_relevant_context(&all, &filter("Zed"))[0].1, long);
    }

    #[test]
    fn role_keywords_cover_name_special_cases_and_role() {
        assert_eq!(extract_role_keywords("backend a", None), ["backend"]);
        assert_eq!(
            extract_role_keywords("front-end", None),
            ["front", "front-end", "frontend", "tui"]
        );
        assert_eq!(
            extract_role_keywords("cmo", Some("Marketing lead")),
            ["cmo", "marketer", "marketing", "cmo"]
        );
        assert_eq!(
            extract_role_keywords("a b", Some("qa")),
            Vec::<String>::new()
        );
    }
}
