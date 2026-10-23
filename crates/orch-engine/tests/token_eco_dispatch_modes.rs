//! T1 / F4 — prompt-mode ownership through the real dispatch path.
//!
//! Vectors: D1-3 (override retention, next-dispatch refresh, restart, active
//! run unchanged), D1-4 (hand-edited custom template + eco fails before any
//! spawn), D3-4 end-to-end (selected context entries and full values),
//! D3-11 (overwrite / TTL expiry after the dispatch captured its context).
//!
//! The assertions read what a child would actually receive (system + user
//! text handed to `Adapter::execute`), rendered by the real Liquid engine
//! from file-backed stores in a disposable directory.

mod token_eco_support;

use orch_domain::{AgentStatus, PromptMode, TaskStatus};
use orch_engine::prompt_mode::set_project_prompt_mode;
use serde_json::json;
use token_eco_support::{Rig, RigSpec, claude_agent, config_with, task};

fn spec(agents: Vec<orch_domain::Agent>, tasks: Vec<orch_domain::Task>, mode: &str) -> RigSpec {
    RigSpec {
        raw_config: config_with(Some(mode), json!({})),
        agents,
        tasks,
        skills: vec![],
        config_store: true,
    }
}

// ── D1-3 ────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_project_switch_changes_the_next_dispatch_of_an_inheriting_agent_only() {
    let inherits = claude_agent("agt_inherit", "Inheritor");
    let mut pinned = claude_agent("agt_pinned", "Pinned");
    pinned.config.prompt_mode = Some(PromptMode::Normal);
    let rig = Rig::new(spec(
        vec![inherits.clone(), pinned.clone()],
        vec![task("tsk_1", Some("agt_inherit"))],
        "normal",
    ))
    .await;

    rig.dispatch().await;
    let first = rig.adapter.for_task("tsk_1");
    assert!(
        !first.is_eco(),
        "project normal must render the normal prompt"
    );
    assert_eq!(first.config.prompt_mode, Some(PromptMode::Normal));

    // The real mutation surface: key-only write under the definition lock.
    set_project_prompt_mode(rig.dir.path(), rig.engine.agents.as_ref(), PromptMode::Eco)
        .await
        .expect("switch to eco");

    rig.reseed_task(task("tsk_2", Some("agt_inherit"))).await;
    rig.reseed_task(task("tsk_3", Some("agt_pinned"))).await;
    rig.dispatch().await;

    let second = rig.adapter.for_task("tsk_2");
    assert!(
        second.is_eco(),
        "the inheriting agent follows the new project mode"
    );
    assert_eq!(second.config.prompt_mode, Some(PromptMode::Eco));
    let third = rig.adapter.for_task("tsk_3");
    assert!(
        !third.is_eco(),
        "an explicit normal override survives project eco"
    );
    assert_eq!(third.config.prompt_mode, Some(PromptMode::Normal));
}

#[tokio::test(flavor = "multi_thread")]
async fn dispatch_never_writes_the_captured_mode_into_a_stored_definition() {
    let inherits = claude_agent("agt_inherit", "Inheritor");
    let rig = Rig::new(spec(
        vec![inherits],
        vec![task("tsk_1", Some("agt_inherit"))],
        "eco",
    ))
    .await;
    let agent_files_before: Vec<_> = rig
        .agents_snapshot()
        .into_iter()
        .filter(|(p, _)| p.extension().is_some_and(|e| e == "yml"))
        .collect();

    rig.dispatch().await;

    assert!(rig.adapter.for_task("tsk_1").is_eco());
    assert_eq!(
        rig.agent("agt_inherit").await.config.prompt_mode,
        None,
        "absent means live inheritance; dispatch must not copy the project default"
    );
    // Runtime fields (status/stats) legitimately change; the authored mode must
    // not appear in the definition file.
    let after: String = rig
        .agents_snapshot()
        .into_iter()
        .filter(|(p, _)| p.extension().is_some_and(|e| e == "yml"))
        .map(|(_, b)| String::from_utf8(b).unwrap())
        .collect();
    assert!(!after.contains("prompt_mode"), "definition files: {after}");
    assert!(!agent_files_before.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_runs_recorded_prompt_is_not_rewritten_by_a_later_switch() {
    let rig = Rig::new(spec(
        vec![claude_agent("agt_a", "Alpha")],
        vec![task("tsk_1", Some("agt_a"))],
        "eco",
    ))
    .await;
    rig.dispatch().await;
    let before = rig.runs().await;
    assert_eq!(before.len(), 1);
    let recorded = before[0].prompt.clone();

    set_project_prompt_mode(
        rig.dir.path(),
        rig.engine.agents.as_ref(),
        PromptMode::Normal,
    )
    .await
    .expect("switch back");

    let after = rig.runs().await;
    assert_eq!(
        after[0].prompt, recorded,
        "the active/finished run keeps its prompt"
    );
    assert!(recorded.contains("Title of tsk_1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_restart_the_override_is_retained_and_inheritance_follows_the_file() {
    let inherits = claude_agent("agt_inherit", "Inheritor");
    let mut pinned = claude_agent("agt_pinned", "Pinned");
    pinned.config.prompt_mode = Some(PromptMode::Normal);
    let rig = Rig::new(spec(vec![inherits, pinned], vec![], "eco")).await;
    let rig = rig.restart(true).await;

    rig.reseed_task(task("tsk_i", Some("agt_inherit"))).await;
    rig.reseed_task(task("tsk_p", Some("agt_pinned"))).await;
    rig.dispatch().await;

    assert!(rig.adapter.for_task("tsk_i").is_eco());
    assert!(!rig.adapter.for_task("tsk_p").is_eco());
    assert_eq!(
        rig.agent("agt_pinned").await.config.prompt_mode,
        Some(PromptMode::Normal)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_observer_process_switch_applies_without_restarting_the_dispatcher() {
    // The TUI/CLI that flips the mode is a different process: it only edits
    // config.yml. The running orchestrator must see it on its next dispatch.
    let rig = Rig::new(spec(
        vec![claude_agent("agt_a", "Alpha")],
        vec![task("tsk_1", Some("agt_a"))],
        "normal",
    ))
    .await;
    rig.dispatch().await;
    assert!(!rig.adapter.for_task("tsk_1").is_eco());

    rig.set_project_mode_raw(PromptMode::Eco).await;
    rig.reseed_task(task("tsk_2", Some("agt_a"))).await;
    rig.dispatch().await;
    assert!(rig.adapter.for_task("tsk_2").is_eco());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_config_store_the_startup_snapshot_decides() {
    let mut s = spec(
        vec![claude_agent("agt_a", "Alpha")],
        vec![task("tsk_1", Some("agt_a"))],
        "normal",
    );
    s.config_store = false;
    let rig = Rig::new(s).await;
    rig.set_project_mode_raw(PromptMode::Eco).await;
    rig.dispatch().await;
    assert!(
        !rig.adapter.for_task("tsk_1").is_eco(),
        "no store is wired, so the on-disk switch must not be seen"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn eco_and_normal_agree_on_which_tasks_run_and_leave_the_same_task_state() {
    // Mode is presentation only: the same store dispatches the same task set.
    let mut modes = Vec::new();
    for mode in ["normal", "eco"] {
        let rig = Rig::new(spec(
            vec![
                claude_agent("agt_a", "Alpha"),
                claude_agent("agt_b", "Beta"),
            ],
            vec![
                task("tsk_1", Some("agt_a")),
                task("tsk_2", Some("agt_b")),
                task("tsk_3", None),
            ],
            mode,
        ))
        .await;
        rig.dispatch().await;
        let mut ids: Vec<String> = rig
            .adapter
            .calls()
            .iter()
            .map(|c| c.task_id().to_owned())
            .collect();
        ids.sort();
        let mut statuses = Vec::new();
        for id in ["tsk_1", "tsk_2", "tsk_3"] {
            statuses.push(format!("{id}:{}", rig.task(id).await.status));
        }
        modes.push((ids, statuses));
    }
    assert_eq!(modes[0], modes[1]);
}

// ── D1-4 (dispatch side) ────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_edited_project_eco_with_a_custom_template_fails_before_any_spawn() {
    let raw = config_with(
        Some("eco"),
        json!({"prompt": {"system_template": "CUSTOM {{ task.title }}"}}),
    );
    let rig = Rig::new(RigSpec {
        raw_config: raw,
        agents: vec![claude_agent("agt_a", "Alpha")],
        tasks: vec![task("tsk_1", Some("agt_a"))],
        skills: vec![],
        config_store: true,
    })
    .await;

    let _ = rig.dispatch_result().await;

    assert!(
        rig.adapter.calls().is_empty(),
        "no model child may be spawned"
    );
    assert!(
        rig.runs().await.is_empty(),
        "no run is created for a preflight failure"
    );
    assert_eq!(rig.task("tsk_1").await.status, TaskStatus::Todo);
    assert_eq!(rig.agent("agt_a").await.status, AgentStatus::Idle);
    let state = rig.engine.state.read().await.unwrap();
    assert!(
        state.claimed.is_empty(),
        "claim released: {:?}",
        state.claimed
    );
    assert!(state.running.is_empty());
    // Dispatch rechecks a hand-written file with the SAME diagnostic the
    // mutation surfaces use: it names the agent and where its eco comes from.
    let errors: Vec<String> = rig
        .events()
        .into_iter()
        .filter_map(|e| match e {
            orch_domain::OrchestratorEvent::OrchestratorError { error, .. } => Some(error),
            _ => None,
        })
        .collect();
    assert!(
        errors.iter().any(|e| e.contains("affected agents")
            && e.contains("agt_a")
            && e.contains("inherits project eco")
            && e.contains("--prompt-mode normal")),
        "{errors:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_eco_override_against_a_project_custom_template_fails_but_a_normal_agent_still_runs()
 {
    let raw = config_with(
        Some("normal"),
        json!({"prompt": {"system_template": "CUSTOM-SYSTEM {{ task.title }}"}}),
    );
    let mut eco_agent = claude_agent("agt_eco", "EcoAgent");
    eco_agent.config.prompt_mode = Some(PromptMode::Eco);
    let rig = Rig::new(RigSpec {
        raw_config: raw,
        agents: vec![eco_agent, claude_agent("agt_norm", "NormAgent")],
        tasks: vec![
            task("tsk_e", Some("agt_eco")),
            task("tsk_n", Some("agt_norm")),
        ],
        skills: vec![],
        config_store: true,
    })
    .await;

    let _ = rig.dispatch_result().await;

    let calls = rig.adapter.calls();
    assert_eq!(calls.len(), 1, "only the normal agent spawns: {calls:?}");
    assert_eq!(calls[0].task_id(), "tsk_n");
    assert!(
        calls[0]
            .system_prompt
            .as_deref()
            .unwrap()
            .starts_with("CUSTOM-SYSTEM Title of tsk_n"),
        "a normal agent renders the custom template byte-for-byte"
    );
    assert_eq!(rig.task("tsk_e").await.status, TaskStatus::Todo);
    assert_eq!(rig.agent("agt_eco").await.status, AgentStatus::Idle);
}

// ── D3-4 end to end and D3-11 ───────────────────────────────────────────

const SUFFIX: &str = "HIDDEN-SCOPE-RULE-ONLY-AT-THE-END";

fn long_value(len: usize) -> String {
    let mut v = "x".repeat(len - SUFFIX.len());
    v.push_str(SUFFIX);
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn eco_keeps_the_exact_selected_context_entries_and_their_full_values() {
    let mut selected = Vec::new();
    for mode in ["normal", "eco"] {
        let rig = Rig::new(spec(
            vec![claude_agent("agt_a", "Alpha")],
            vec![task("tsk_1", Some("agt_a"))],
            mode,
        ))
        .await;
        // 16 relevant entries (generic `spec-` prefix scores 1): the existing
        // 15-entry cap applies identically in both modes.
        for i in 0..16 {
            let value = match i {
                0 => long_value(1_025),
                1 => long_value(100_000),
                _ => format!("v{i}"),
            };
            rig.engine
                .context
                .set(&format!("spec-{i:02}"), &value, None)
                .await
                .unwrap();
        }
        rig.dispatch().await;
        let call = rig.adapter.for_task("tsk_1");
        let text = call.all_text();
        assert_eq!(
            text.matches(SUFFIX).count(),
            2,
            "{mode}: both long values keep their hidden suffix in full"
        );
        let keys: Vec<usize> = (0..16)
            .filter(|i| text.contains(&format!("spec-{i:02}")))
            .collect();
        selected.push(keys);
    }
    assert_eq!(selected[0].len(), 15, "the existing cap is 15 entries");
    assert_eq!(
        selected[0], selected[1],
        "eco selects exactly what normal selects"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_overwrite_and_a_ttl_expiry_after_dispatch_do_not_alter_the_dispatched_evidence() {
    let rig = Rig::new(spec(
        vec![claude_agent("agt_a", "Alpha")],
        vec![task("tsk_1", Some("agt_a"))],
        "eco",
    ))
    .await;
    rig.engine
        .context
        .set("spec-rule", &long_value(2_000), Some(60_000))
        .await
        .unwrap();
    rig.engine
        .context
        .set("spec-overwritten", "ORIGINAL-VALUE", None)
        .await
        .unwrap();
    rig.dispatch().await;
    let dispatched = rig.adapter.for_task("tsk_1").all_text();
    assert!(dispatched.contains(SUFFIX));
    assert!(dispatched.contains("ORIGINAL-VALUE"));
    let recorded_prompt = rig.runs().await[0].prompt.clone();

    // After the snapshot was captured: overwrite one key, expire the other.
    rig.engine
        .context
        .set("spec-overwritten", "CHANGED-LATER", None)
        .await
        .unwrap();
    rig.clock.advance_ms(61_000);

    // Later lookups see the new state ...
    assert!(rig.engine.context.get("spec-rule").await.unwrap().is_none());
    assert_eq!(
        rig.engine
            .context
            .get("spec-overwritten")
            .await
            .unwrap()
            .unwrap()
            .value,
        "CHANGED-LATER"
    );
    // ... but the evidence the run was given is unchanged and still complete.
    assert_eq!(rig.runs().await[0].prompt, recorded_prompt);
    assert!(
        rig.adapter
            .for_task("tsk_1")
            .all_text()
            .contains("ORIGINAL-VALUE")
    );

    // A new dispatch reflects the current state, which is what the prompt's
    // "current state" lookups would also show: no snapshot-recovery claim.
    rig.reseed_task(task("tsk_2", Some("agt_a"))).await;
    rig.dispatch().await;
    let later = rig.adapter.for_task("tsk_2").all_text();
    assert!(later.contains("CHANGED-LATER"));
    assert!(
        !later.contains(SUFFIX),
        "the expired entry is gone for a new dispatch"
    );
}

// ── Goal progress is kept in full ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn eco_keeps_the_complete_goal_progress_value_supplied_at_dispatch() {
    use orch_domain::CreateGoalInput;
    let rig = Rig::new(spec(vec![claude_agent("agt_a", "Alpha")], vec![], "eco")).await;
    let goal = rig
        .engine
        .goal_service
        .create(CreateGoalInput {
            title: "Big goal".into(),
            description: Some("Goal description kept whole".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut t = task("tsk_g", Some("agt_a"));
    t.goal_id = Some(goal.id.clone());
    rig.reseed_task(t).await;
    let progress = long_value(50_000);
    rig.engine
        .context
        .set(&format!("{}-progress", goal.id), &progress, None)
        .await
        .unwrap();

    rig.dispatch().await;

    let call = rig.adapter.for_task("tsk_g");
    assert!(
        call.prompt.contains(&progress),
        "complete progress, no byte cap"
    );
    assert!(call.prompt.contains("Goal description kept whole"));
}

// ── configuration preflight ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_config_after_startup_stops_the_dispatch_before_any_run_and_never_falls_back_to_normal()
 {
    let rig = Rig::new(spec(
        vec![claude_agent("agt_a", "Alpha")],
        vec![task("tsk_1", Some("agt_a"))],
        "eco",
    ))
    .await;
    // The file is damaged after the process started: the startup snapshot is
    // still fine, only the per-dispatch re-read can notice.
    std::fs::write(rig.config_path(), "defaults: [this is not a mapping").unwrap();

    let _ = rig.dispatch_result().await;

    assert!(
        rig.adapter.calls().is_empty(),
        "a silent fall back to normal would have spawned a child"
    );
    assert!(rig.runs().await.is_empty());
    assert_eq!(rig.task("tsk_1").await.status, TaskStatus::Todo);
    assert_eq!(rig.agent("agt_a").await.status, AgentStatus::Idle);
    let state = rig.engine.state.read().await.unwrap();
    assert!(state.claimed.is_empty() && state.running.is_empty());
    let errors: Vec<String> = rig
        .events()
        .into_iter()
        .filter_map(|e| match e {
            orch_domain::OrchestratorEvent::OrchestratorError { error, .. } => Some(error),
            _ => None,
        })
        .collect();
    assert!(
        errors.iter().any(|e| e.contains("configuration preflight")),
        "the failure is visible: {errors:?}"
    );
}
