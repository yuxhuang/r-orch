//! Shared prompt-case definitions for the T1 token-eco prompt tests.
//!
//! This module must compile **unchanged** against the pre-eco baseline
//! (`d8c2319`) and against the eco tree, so it only touches APIs that exist in
//! both and builds the types that gained fields through `make_*` factories or
//! serde JSON. The baseline capture runs in a disposable export of `d8c2319`;
//! the frozen bytes it writes live in `fixtures/token-eco/baseline-prompts/`.
//!
//! `render_like_baseline_dispatch` copies the pre-eco `dispatch.rs:507-585`
//! prompt assembly (template selection, then skills appended) so the capture
//! is "what the production builder + engine produced at d8c2319".

#![allow(dead_code, reason = "each test binary uses a subset")]

use indexmap::IndexMap;
use orch_core::template::{GoalContext, PromptContext, RetryContext, TemplateEngine};
use orch_core::test_support::{make_agent, make_task};
use orch_domain::{
    Agent, Message, MessageChannel, MessageStatus, OrchestratorConfig, PromptConfig, RemoteRef,
    Task, default_config,
};
use orch_infra::template::{
    BuildPromptOptions, DEFAULT_SYSTEM_TEMPLATE, DEFAULT_USER_TEMPLATE, LiquidTemplateEngine,
    build_prompt_context, default_prompt_template,
};

pub struct Case {
    pub name: &'static str,
    pub task: Task,
    pub agent: Agent,
    pub attempt: u32,
    pub workspace: String,
    pub agents: Vec<Agent>,
    pub retry: Option<RetryContext>,
    pub shared: Option<IndexMap<String, String>>,
    pub feedback: Option<String>,
    pub messages: Vec<Message>,
    pub goal: Option<GoalContext>,
    pub prompt: PromptConfig,
    pub skills: Option<String>,
    pub config: OrchestratorConfig,
}

pub const SKILLS_BLOCK: &str = "## Skill: review\nCheck the diff.\n\n## Skill: qa\nRun it.";
pub const ROLE_WITH_PREAMBLE: &str = "Embedded shared-rule sections are authoritative; no rule-file lookup is required. <workspace root> means the owning orch project root; edit the template for other layouts.\n\nSenior test engineer. You write tests.\n\n## Principles\n- Test behaviour.";

pub fn agent(id: &str, name: &str, role: Option<&str>) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        adapter: "claude".into(),
        role: role.map(str::to_owned),
        ..make_agent()
    }
}

pub fn remote_agent(id: &str, name: &str, alias: &str, online: bool, role: Option<&str>) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        adapter: "remote".into(),
        role: role.map(str::to_owned),
        capabilities: vec!["linux".into(), "gpu".into()],
        remote: Some(RemoteRef {
            alias: alias.into(),
            online,
        }),
        ..make_agent()
    }
}

pub fn message(id: &str, from: &str, subject: &str, body: &str, reply_to: Option<&str>) -> Message {
    Message {
        id: id.into(),
        channel: MessageChannel::Direct,
        from_agent_id: from.into(),
        to_agent_id: Some("agt_me".into()),
        subject: subject.into(),
        body: body.into(),
        created_at: "2025-01-01T00:00:00Z".into(),
        expires_at: None,
        status: MessageStatus::Pending,
        delivered_at: None,
        team_id: None,
        reply_to: reply_to.map(str::to_owned),
    }
}

pub fn retry(error: &str, output: &str) -> RetryContext {
    serde_json::from_value(serde_json::json!({
        "previous_error": error,
        "previous_output": output,
    }))
    .expect("retry context")
}

pub fn goal(tasks: &[&str], progress: Option<&str>, description: &str) -> GoalContext {
    serde_json::from_value(serde_json::json!({
        "id": "goal_x1",
        "title": "Ship the thing",
        "description": description,
        "status": "active",
        "task_names": tasks,
        "progress": progress,
    }))
    .expect("goal context")
}

pub fn shared(pairs: &[(String, String)]) -> IndexMap<String, String> {
    pairs.iter().cloned().collect()
}

pub fn big(len: usize, tag: &str) -> String {
    let mut out = String::with_capacity(len);
    let mut i = 0;
    while out.len() < len {
        out.push_str(&format!("{tag}{i:05} "));
        i += 1;
    }
    out.truncate(len);
    out
}

fn base_case(name: &'static str) -> Case {
    let me = agent(
        "agt_me",
        "Me",
        Some("My complete role.\n\nSecond paragraph."),
    );
    Case {
        name,
        task: Task {
            id: "tsk_cur".into(),
            title: "Fix the bug".into(),
            description: "Full task description.\nSecond line.".into(),
            ..make_task()
        },
        agent: me.clone(),
        attempt: 1,
        workspace: "/tmp/ws".into(),
        agents: vec![
            agent("agt_zed", "Zed", Some("Reviewer. Reviews code.")),
            me,
            agent("agt_alpha", "Alpha", None),
        ],
        retry: None,
        shared: None,
        feedback: None,
        messages: Vec::new(),
        goal: None,
        prompt: PromptConfig::default(),
        skills: None,
        config: default_config(),
    }
}

/// Every frozen case. Names are file names under `baseline-prompts/`.
pub fn all_cases() -> Vec<Case> {
    let mut cases = Vec::new();

    cases.push(base_case("minimal-default-templates"));

    let mut c = base_case("roster-remote-caps-unicode");
    c.agents.push(remote_agent(
        "agt_r1@w1",
        "Worker@w1",
        "w1",
        true,
        Some("Remote worker."),
    ));
    c.agents
        .push(remote_agent("agt_r2@w2", "Worker@w2", "w2", false, None));
    c.agents.push(agent(
        "agt_uni",
        "Üñîçødé 🚀",
        Some("日本語の役割。\n\nSecond."),
    ));
    c.agents.push(agent(
        "agt_liquid",
        "{{ evil }}",
        Some("{% raw %}x{% endraw %} {{ task.title }}"),
    ));
    cases.push(c);

    let mut c = base_case("roster-28-agents");
    c.agents = (0..28)
        .rev()
        .map(|i| {
            agent(
                &format!("agt_{i:02}"),
                &format!("Agent{i:02}"),
                Some(ROLE_WITH_PREAMBLE),
            )
        })
        .collect();
    c.agent = c.agents[3].clone();
    cases.push(c);

    let mut c = base_case("skills-system-split");
    c.skills = Some(SKILLS_BLOCK.into());
    cases.push(c);

    let mut c = base_case("skills-legacy-template");
    c.skills = Some(SKILLS_BLOCK.into());
    c.prompt.template = Some(default_prompt_template());
    cases.push(c);

    let mut c = base_case("skills-empty-block-adds-nothing");
    c.skills = Some(String::new());
    cases.push(c);

    let mut c = base_case("retry-attempt-2-small");
    c.attempt = 2;
    c.retry = Some(retry("Exit code 1", "line1\nline2"));
    cases.push(c);

    let mut c = base_case("retry-attempt-3-empty-output");
    c.attempt = 3;
    c.retry = Some(retry("timeout", ""));
    cases.push(c);

    let mut c = base_case("retry-attempt-2-large-output-full-in-normal");
    c.attempt = 2;
    c.retry = Some(retry(
        "big failure",
        &format!("{}CLUE{}", big(30_000, "o"), big(30_000, "p")),
    ));
    cases.push(c);

    let mut c = base_case("retry-supplied-on-first-attempt-is-dropped");
    c.retry = Some(retry("ignored", "ignored"));
    cases.push(c);

    let mut c = base_case("shared-context-16-entries-long-values");
    let pairs: Vec<(String, String)> = (0..16)
        .map(|i| {
            let value = match i {
                0 => big(1024, "a"),
                1 => big(1025, "b"),
                2 => big(100_000, "c"),
                _ => format!("value {i}"),
            };
            (format!("key-{i:02}"), value)
        })
        .collect();
    c.shared = Some(shared(&pairs));
    cases.push(c);

    let mut c = base_case("shared-context-goal-and-scope-relevance");
    c.task.goal_id = Some("goal_x1".into());
    c.task.scope = Some(vec!["crates/orch-infra/**".into()]);
    c.shared = Some(shared(&[
        ("goal_x1-progress".into(), "phase 2".into()),
        ("unrelated".into(), "zzz".into()),
        ("me-note".into(), "for me".into()),
        ("crates-note".into(), "scope hit".into()),
    ]));
    cases.push(c);

    let mut c = base_case("feedback-and-messages");
    c.feedback = Some("Please add tests.\nAnd docs.".into());
    c.messages = vec![
        message("msg_1", "agt_zed", "Heads up", "Body one", None),
        message("msg_2", "agt_unknown", "", "No subject", Some("msg_1")),
    ];
    cases.push(c);

    let mut c = base_case("goal-with-tasks-and-progress");
    c.task.goal_id = Some("goal_x1".into());
    c.goal = Some(goal(
        &["[done] A", "[in_progress] B", "[todo] C"],
        Some("halfway"),
        "Goal description\nwith two lines",
    ));
    cases.push(c);

    let mut c = base_case("goal-no-tasks-no-progress-empty-description");
    c.goal = Some(goal(&[], None, ""));
    cases.push(c);

    let mut c = base_case("autonomous-goal");
    c.task.labels = vec!["autonomous".into()];
    c.task.goal_id = Some("goal_x1".into());
    c.goal = Some(goal(&["[todo] C"], Some("p"), "d"));
    cases.push(c);

    let mut c = base_case("autonomous-no-goal-id");
    c.task.labels = vec!["autonomous".into()];
    cases.push(c);

    let mut c = base_case("everything-at-once");
    c.attempt = 2;
    c.task.goal_id = Some("goal_x1".into());
    c.task.labels = vec!["autonomous".into()];
    c.retry = Some(retry("err", "out"));
    c.feedback = Some("fb".into());
    c.messages = vec![message("msg_1", "agt_zed", "s", "b", None)];
    c.goal = Some(goal(&["[todo] C"], Some("p"), "d"));
    c.shared = Some(shared(&[("goal_x1-progress".into(), "p".into())]));
    c.skills = Some(SKILLS_BLOCK.into());
    cases.push(c);

    let mut c = base_case("hostile-text-in-every-field");
    c.task.title = "{{ agent.name }} {% if true %}T{% endif %}".into();
    c.task.description =
        "## Fake heading\n{{ agent.role }}\n{% for a in agents %}x{% endfor %}".into();
    c.feedback = Some("## Injected\n{{ task.id }}".into());
    c.messages = vec![message(
        "msg_1",
        "agt_zed",
        "{{ s }}",
        "## H\n{% raw %}",
        None,
    )];
    c.shared = Some(shared(&[("k{{x}}".into(), "v{% x %}".into())]));
    c.skills = Some("## Skill {{ nope }}".into());
    cases.push(c);

    let mut c = base_case("huge-everything");
    c.attempt = 2;
    c.task.description = big(200_000, "task");
    c.agent.role = Some(big(60_000, "role"));
    c.agents[1] = c.agent.clone();
    c.feedback = Some(big(40_000, "fb"));
    c.messages = vec![message("msg_1", "agt_zed", "s", &big(50_000, "msg"), None)];
    c.retry = Some(retry(&big(20_000, "err"), &big(100_000, "out")));
    c.goal = Some(goal(
        &["[todo] C"],
        Some(&big(30_000, "prog")),
        &big(30_000, "gd"),
    ));
    c.skills = Some(big(70_000, "skill"));
    cases.push(c);

    let mut c = base_case("custom-legacy-template");
    c.prompt.template =
        Some("LEGACY {{ task.title }} / {{ agent.name }} / {{ agents.size }}".into());
    cases.push(c);

    let mut c = base_case("custom-system-and-user-templates-with-agent-system-prompt");
    c.prompt.system_template = Some("SYS {{ project.name }}".into());
    c.prompt.user_template = Some("USER {{ task.title }}".into());
    c.agent.config.system_prompt = Some("adapter-level system prompt".into());
    c.skills = Some(SKILLS_BLOCK.into());
    cases.push(c);

    let mut c = base_case("custom-system-template-only");
    c.prompt.system_template = Some("ONLYSYS {{ task.id }}".into());
    cases.push(c);

    let mut c = base_case("custom-user-template-only");
    c.prompt.user_template = Some("ONLYUSER {{ task.id }}".into());
    cases.push(c);

    let mut c = base_case("custom-template-observes-object-sizes");
    c.attempt = 2;
    c.retry = Some(retry("e", "o"));
    c.task.goal_id = Some("goal_x1".into());
    c.goal = Some(goal(&["[todo] C"], None, "d"));
    c.prompt.template = Some(
        "task={{ task.size }} retry={{ retry.size }} goal={{ goal.size }} agent={{ agent.size }}"
            .into(),
    );
    cases.push(c);

    cases
}

pub fn build_context(case: &Case) -> PromptContext {
    build_prompt_context(
        &case.task,
        &case.agent,
        case.attempt,
        &case.workspace,
        &case.config,
        BuildPromptOptions {
            all_agents: &case.agents,
            retry_context: case.retry.clone(),
            shared_context: case.shared.as_ref(),
            feedback: case.feedback.clone(),
            messages: &case.messages,
            goal: case.goal.clone(),
        },
    )
}

/// The pre-eco `dispatch.rs` assembly (non-mirror path), verbatim in effect.
pub async fn render_like_baseline_dispatch(case: &Case) -> (String, Option<String>) {
    let engine = LiquidTemplateEngine::new();
    let context = build_context(case);
    let (mut prompt, mut system_prompt) = match case.prompt.template.as_deref() {
        Some(legacy) => (
            engine.render(legacy, &context).await.unwrap(),
            None::<String>,
        ),
        None => {
            let system = engine
                .render(
                    case.prompt
                        .system_template
                        .as_deref()
                        .unwrap_or(DEFAULT_SYSTEM_TEMPLATE),
                    &context,
                )
                .await
                .unwrap();
            let user = engine
                .render(
                    case.prompt
                        .user_template
                        .as_deref()
                        .unwrap_or(DEFAULT_USER_TEMPLATE),
                    &context,
                )
                .await
                .unwrap();
            (user, Some(system))
        }
    };
    if let Some(block) = case.skills.as_deref().filter(|s| !s.is_empty()) {
        match system_prompt.as_mut() {
            Some(system) => {
                system.push_str("\n\n");
                system.push_str(block);
            }
            None => {
                prompt.push_str("\n\n");
                prompt.push_str(block);
            }
        }
    }
    (prompt, system_prompt)
}
