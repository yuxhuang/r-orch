//! T1 / F4 — definition and project-config mutation guards.
//!
//! Vectors: D1-2 (engine side: locked key-only update, extension keys survive,
//! concurrent writer serialised, write failure leaves the file), D1-4
//! (mutation side: every offender listed before any write, remedies work, no
//! partial config/agent write), D5-6 (final-candidate effort validation:
//! rejection leaves the definition AND the index byte-identical; clearing
//! effort in the same mutation succeeds), C1/F2 (an agent mutation under
//! `.definition.lock` validates against the project config read inside it,
//! not the engine's construction snapshot), ruling 13 (shell effort).
//!
//! Real file stores, the real `AgentService` and the real engine mutation
//! functions over a disposable directory; no adapter is ever invoked.

mod token_eco_support;

use std::time::Duration;

use orch_domain::{Agent, AgentConfig, PromptMode, ReasoningEffort};
use orch_engine::UpdateAgentFields;
use orch_engine::prompt_mode::{
    DefinitionLock, set_project_prompt_mode, toggle_project_prompt_mode,
};
use orch_infra::storage::{FileConfigStore, fs_utils};
use serde_json::{Value, json};
use token_eco_support::{ADAPTER, Rig, RigSpec, claude_agent, config_with};

fn base_raw() -> Value {
    json!({
        "project": {"name": "demo"},
        "x_extension": {"nested": [1, 2, {"deep": null}], "flag": true},
        "defaults": {"agent": {"prompt_mode": "normal", "x_future_agent_key": "keep-me"}}
    })
}

async fn rig_with(raw: Value, agents: Vec<Agent>) -> Rig {
    Rig::new(RigSpec {
        raw_config: raw,
        agents,
        tasks: vec![],
        skills: vec![],
        config_store: true,
    })
    .await
}

async fn raw_of(rig: &Rig) -> Value {
    FileConfigStore::new(rig.paths.clone())
        .read_raw()
        .await
        .unwrap()
}

/// Remove the one key a mode switch is allowed to change.
fn without_mode(mut raw: Value) -> Value {
    raw["defaults"]["agent"]
        .as_object_mut()
        .unwrap()
        .remove("prompt_mode");
    raw
}

// ── D1-2 ────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn toggling_twice_changes_only_the_mode_key_and_returns_to_the_original_value() {
    let rig = rig_with(base_raw(), vec![]).await;
    let agents = rig.engine.agents.clone();

    let first = toggle_project_prompt_mode(rig.dir.path(), agents.as_ref())
        .await
        .unwrap();
    assert_eq!(first, PromptMode::Eco);
    let after_first = raw_of(&rig).await;
    assert_eq!(after_first["defaults"]["agent"]["prompt_mode"], "eco");
    assert_eq!(
        without_mode(after_first),
        without_mode(base_raw()),
        "key-only write"
    );

    let second = toggle_project_prompt_mode(rig.dir.path(), agents.as_ref())
        .await
        .unwrap();
    assert_eq!(second, PromptMode::Normal);
    assert_eq!(
        raw_of(&rig).await,
        base_raw(),
        "unknown extension keys survive both writes"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn setting_the_mode_it_already_has_does_not_rewrite_the_file() {
    let rig = rig_with(base_raw(), vec![]).await;
    let before = rig.config_bytes();
    let modified = std::fs::metadata(rig.config_path())
        .unwrap()
        .modified()
        .unwrap();

    set_project_prompt_mode(
        rig.dir.path(),
        rig.engine.agents.as_ref(),
        PromptMode::Normal,
    )
    .await
    .unwrap();

    assert_eq!(rig.config_bytes(), before);
    assert_eq!(
        std::fs::metadata(rig.config_path())
            .unwrap()
            .modified()
            .unwrap(),
        modified,
        "an unchanged value must not touch the file"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_toggle_waits_for_the_definition_lock_and_keeps_a_concurrent_writers_change() {
    let rig = rig_with(base_raw(), vec![]).await;
    let lock_path = rig.paths.root().join(".definition.lock");
    let held = fs_utils::mutation_lock(&lock_path).await.unwrap();

    let root = rig.dir.path().to_path_buf();
    let agents = rig.engine.agents.clone();
    let toggle =
        tokio::spawn(async move { toggle_project_prompt_mode(&root, agents.as_ref()).await });
    // The toggle polls the lock every 10 ms; give it many chances to (wrongly)
    // proceed while we still hold it.
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !toggle.is_finished(),
        "the toggle must wait for the held lock"
    );
    assert_eq!(
        raw_of(&rig).await["defaults"]["agent"]["prompt_mode"],
        "normal"
    );

    // A concurrent `config set`-style writer changes another key under the lock.
    let store = FileConfigStore::new(rig.paths.clone());
    let mut raw = store.read_raw().await.unwrap();
    raw["project"]["name"] = json!("renamed-by-cli");
    store.write_raw(&raw, false).await.unwrap();
    drop(held);

    assert_eq!(toggle.await.unwrap().unwrap(), PromptMode::Eco);
    let merged = raw_of(&rig).await;
    assert_eq!(merged["defaults"]["agent"]["prompt_mode"], "eco");
    assert_eq!(
        merged["project"]["name"], "renamed-by-cli",
        "the toggle re-read the file inside the lock; the other writer's change must survive"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_write_failure_leaves_the_config_bytes_untouched_and_reports_an_error() {
    use std::os::unix::fs::PermissionsExt;
    // An existing agent makes `agents/` exist, so only the config publication can fail.
    let rig = rig_with(base_raw(), vec![claude_agent("agt_a", "Alpha")]).await;
    // The lock file must already exist: only the config publication may fail.
    drop(
        fs_utils::mutation_lock(&rig.paths.root().join(".definition.lock"))
            .await
            .unwrap(),
    );
    let before = rig.config_bytes();
    let root = rig.paths.root();
    let original = std::fs::metadata(&root).unwrap().permissions();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();

    // Root (or a permissive filesystem) can still write into a 0555 directory;
    // then the failure cannot be provoked and there is nothing to assert.
    let probe = root.join(".probe-writable");
    let provokable = std::fs::write(&probe, b"x").is_err();
    let _ = std::fs::remove_file(&probe);

    let result = toggle_project_prompt_mode(rig.dir.path(), rig.engine.agents.as_ref()).await;

    std::fs::set_permissions(&root, original).unwrap();
    if provokable {
        assert!(
            result.is_err(),
            "an unwritable directory must surface as an error, not a silent no-op"
        );
        assert_eq!(
            rig.config_bytes(),
            before,
            "failed publish left the file as it was"
        );
    }
    assert!(
        !std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".tmp")),
        "no temp file is left behind"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_known_field_refuses_the_switch_and_leaves_the_file_alone() {
    let rig = rig_with(base_raw(), vec![]).await;
    // Corrupt a known numeric field by hand (bypassing the validating writer).
    let mut text = String::from_utf8(rig.config_bytes()).unwrap();
    text.push_str("scheduling:\n  poll_interval_ms: not-a-number\n");
    std::fs::write(rig.config_path(), &text).unwrap();
    let before = rig.config_bytes();

    let err = toggle_project_prompt_mode(rig.dir.path(), rig.engine.agents.as_ref()).await;

    assert!(
        err.is_err(),
        "a corrupt known field must not be silently rewritten"
    );
    assert_eq!(rig.config_bytes(), before);
}

// ── D1-4 (mutation side) ────────────────────────────────────────────────

fn custom_template_raw(mode: &str) -> Value {
    config_with(
        Some(mode),
        json!({"prompt": {"system_template": "CUSTOM {{ task.title }}"}}),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_switch_to_eco_lists_every_offender_and_writes_nothing() {
    let inherits = claude_agent("agt_inherit", "Inheritor");
    let mut eco_override = claude_agent("agt_ecoov", "EcoOverride");
    eco_override.config.prompt_mode = Some(PromptMode::Eco);
    let mut pinned_normal = claude_agent("agt_norm", "PinnedNormal");
    pinned_normal.config.prompt_mode = Some(PromptMode::Normal);
    let rig = rig_with(
        custom_template_raw("normal"),
        vec![inherits, eco_override, pinned_normal],
    )
    .await;
    let config_before = rig.config_bytes();
    let agents_before = rig.agents_snapshot();

    let err = set_project_prompt_mode(rig.dir.path(), rig.engine.agents.as_ref(), PromptMode::Eco)
        .await
        .expect_err("custom template + eco must be refused");
    let message = err.to_string();

    assert!(message.contains("prompt.system_template"), "{message}");
    assert!(
        message.contains("agt_inherit") && message.contains("inherits project eco"),
        "{message}"
    );
    assert!(
        message.contains("agt_ecoov") && message.contains("agent override eco"),
        "{message}"
    );
    assert!(
        !message.contains("agt_norm"),
        "a pinned-normal agent is not an offender: {message}"
    );
    assert!(
        message.contains("--prompt-mode normal") && message.contains("remove the custom templates"),
        "{message}"
    );
    assert_eq!(rig.config_bytes(), config_before, "no partial config write");
    assert_eq!(
        rig.agents_snapshot(),
        agents_before,
        "no agent or index write"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pinning_the_offenders_to_normal_is_a_working_remedy() {
    let mut eco_override = claude_agent("agt_ecoov", "EcoOverride");
    eco_override.config.prompt_mode = Some(PromptMode::Eco);
    let rig = rig_with(
        custom_template_raw("normal"),
        vec![claude_agent("agt_inherit", "Inheritor"), eco_override],
    )
    .await;
    let svc = &rig.engine.agent_service;
    for id in ["agt_inherit", "agt_ecoov"] {
        svc.update(
            id,
            UpdateAgentFields {
                prompt_mode: Some(Some(PromptMode::Normal)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    let mode = set_project_prompt_mode(rig.dir.path(), rig.engine.agents.as_ref(), PromptMode::Eco)
        .await
        .expect("all agents are pinned normal, so project eco is compatible");

    assert_eq!(mode, PromptMode::Eco);
    assert_eq!(
        raw_of(&rig).await["prompt"]["system_template"],
        "CUSTOM {{ task.title }}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_the_custom_template_is_the_other_working_remedy() {
    let rig = rig_with(
        custom_template_raw("normal"),
        vec![claude_agent("agt_a", "Alpha")],
    )
    .await;
    assert!(
        set_project_prompt_mode(rig.dir.path(), rig.engine.agents.as_ref(), PromptMode::Eco)
            .await
            .is_err()
    );
    let store = FileConfigStore::new(rig.paths.clone());
    let mut raw = store.read_raw().await.unwrap();
    raw.as_object_mut().unwrap().remove("prompt");
    store.write_raw(&raw, false).await.unwrap();

    set_project_prompt_mode(rig.dir.path(), rig.engine.agents.as_ref(), PromptMode::Eco)
        .await
        .expect("no custom template left");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_agent_mutation_surface_refuses_an_eco_candidate_under_a_custom_template() {
    let rig = rig_with(
        custom_template_raw("normal"),
        vec![claude_agent("agt_a", "Alpha")],
    )
    .await;
    let svc = &rig.engine.agent_service;
    let before = rig.agents_snapshot();

    // flag-only update
    let err = svc
        .update(
            "agt_a",
            UpdateAgentFields {
                prompt_mode: Some(Some(PromptMode::Eco)),
                ..Default::default()
            },
        )
        .await
        .expect_err("update to eco");
    assert!(err.to_string().contains("--prompt-mode normal"), "{err}");

    // full replacement
    let mut config = rig.agent("agt_a").await.config;
    config.prompt_mode = Some(PromptMode::Eco);
    let err = svc
        .replace_definition("agt_a", "Alpha".into(), "claude".into(), None, config)
        .await
        .expect_err("replace to eco");
    assert!(
        err.to_string().contains("eco prompt mode is incompatible"),
        "{err}"
    );

    // create
    let err = svc
        .create(orch_domain::CreateAgentInput {
            name: "Fresh".into(),
            adapter: "claude".into(),
            prompt_mode: Some(PromptMode::Eco),
            ..Default::default()
        })
        .await
        .expect_err("create as eco");
    assert!(
        err.to_string().contains("eco prompt mode is incompatible"),
        "{err}"
    );

    assert_eq!(
        rig.agents_snapshot(),
        before,
        "no definition or index byte changed"
    );
    assert_eq!(
        svc.list().await.unwrap().len(),
        1,
        "the refused create left no agent"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unrelated_edit_of_a_hand_edited_eco_plus_template_agent_is_refused_with_the_remedy() {
    // Project eco + custom template was hand-written: the stored combination
    // is invalid, so even an unrelated edit must be refused (and say how to fix).
    let rig = rig_with(
        custom_template_raw("eco"),
        vec![claude_agent("agt_a", "Alpha")],
    )
    .await;
    let before = rig.agents_snapshot();

    let err = rig
        .engine
        .agent_service
        .update(
            "agt_a",
            UpdateAgentFields {
                role: Some("A different role".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("final candidate is invalid");

    assert!(err.to_string().contains("inherits project eco"), "{err}");
    assert_eq!(rig.agents_snapshot(), before);

    // The remedy in the message works: pin this agent to normal.
    rig.engine
        .agent_service
        .update(
            "agt_a",
            UpdateAgentFields {
                prompt_mode: Some(Some(PromptMode::Normal)),
                role: Some("A different role".into()),
                ..Default::default()
            },
        )
        .await
        .expect("pinning to normal repairs it in the same mutation");
}

// ── D5-6 ────────────────────────────────────────────────────────────────

fn claude_with_effort(model: &str, effort: ReasoningEffort) -> Agent {
    let mut a = claude_agent("agt_e", "Effortful");
    a.config = AgentConfig {
        model: Some(model.into()),
        effort: Some(effort),
        ..a.config
    };
    a
}

#[tokio::test(flavor = "multi_thread")]
async fn changing_the_model_while_keeping_an_incompatible_effort_fails_and_leaves_the_store_untouched()
 {
    // claude-opus-5-5 offers xhigh; the 4.6 row does not.
    let rig = rig_with(
        json!({}),
        vec![claude_with_effort(
            "claude-opus-5-5",
            ReasoningEffort::Xhigh,
        )],
    )
    .await;
    let before = rig.agents_snapshot();

    let err = rig
        .engine
        .agent_service
        .update(
            "agt_e",
            UpdateAgentFields {
                model: Some("claude-sonnet-4-6".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("xhigh is not offered by claude-sonnet-4-6");
    let message = err.to_string();

    assert!(
        message.contains("xhigh") && message.contains("claude-sonnet-4-6"),
        "{message}"
    );
    assert!(
        message.contains("--effort default"),
        "remedy named: {message}"
    );
    assert_eq!(
        rig.agents_snapshot(),
        before,
        "definition and index byte-identical"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn clearing_effort_in_the_same_mutation_makes_the_model_change_succeed() {
    let rig = rig_with(
        json!({}),
        vec![claude_with_effort(
            "claude-opus-5-5",
            ReasoningEffort::Xhigh,
        )],
    )
    .await;

    let updated = rig
        .engine
        .agent_service
        .update(
            "agt_e",
            UpdateAgentFields {
                model: Some("claude-sonnet-4-6".into()),
                effort: Some(None),
                ..Default::default()
            },
        )
        .await
        .expect("one atomic mutation: new model, effort cleared");

    assert_eq!(updated.config.effort, None);
    assert_eq!(
        rig.agent("agt_e").await.config.model.as_deref(),
        Some("claude-sonnet-4-6")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_adapter_with_a_level_the_new_adapter_lacks_is_refused_and_replacement_agrees() {
    let rig = rig_with(
        json!({}),
        vec![claude_with_effort("claude-opus-5-5", ReasoningEffort::Max)],
    )
    .await;
    let before = rig.agents_snapshot();

    let update = rig
        .engine
        .agent_service
        .update(
            "agt_e",
            UpdateAgentFields {
                adapter: Some("antigravity".into()),
                model: Some(String::new()),
                ..Default::default()
            },
        )
        .await;
    assert!(update.is_err(), "antigravity offers only low/medium/high");

    let mut config = rig.agent("agt_e").await.config;
    config.model = None;
    let replace = rig
        .engine
        .agent_service
        .replace_definition(
            "agt_e",
            "Effortful".into(),
            "antigravity".into(),
            None,
            config.clone(),
        )
        .await;
    assert!(
        replace.is_err(),
        "file replacement runs the same final-candidate check"
    );
    assert_eq!(rig.agents_snapshot(), before);

    config.effort = None;
    rig.engine
        .agent_service
        .replace_definition(
            "agt_e",
            "Effortful".into(),
            "antigravity".into(),
            None,
            config,
        )
        .await
        .expect("adapter switch with effort cleared in the same replacement");
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_an_agent_with_an_unsupported_new_effort_creates_nothing() {
    let rig = rig_with(json!({}), vec![]).await;
    let err = rig
        .engine
        .agent_service
        .create(orch_domain::CreateAgentInput {
            name: "Grokker".into(),
            adapter: "grok".into(),
            effort: Some(ReasoningEffort::Xhigh),
            ..Default::default()
        })
        .await
        .expect_err("grok offers only low/medium/high in this release");
    assert!(err.to_string().to_lowercase().contains("grok"), "{err}");
    assert!(rig.engine.agents.list().await.unwrap().is_empty());
    assert!(
        rig.agents_snapshot()
            .keys()
            .all(|p| !p.to_string_lossy().contains("Grokker")),
        "no definition file"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_opaque_native_model_suffix_survives_clearing_the_generic_effort() {
    let rig = rig_with(json!({}), vec![]).await;
    let mut agent = claude_agent("agt_oc", "OpenCoder");
    agent.adapter = "opencode".into();
    agent.config.model = Some("openrouter/anthropic/claude-opus-5.5#high".into());
    agent.config.effort = Some(ReasoningEffort::High);
    rig.engine.agents.save(&agent).await.unwrap();

    let updated = rig
        .engine
        .agent_service
        .update(
            "agt_oc",
            UpdateAgentFields {
                effort: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(updated.config.effort, None);
    assert_eq!(
        updated.config.model.as_deref(),
        Some("openrouter/anthropic/claude-opus-5.5#high"),
        "clearing generic effort never rewrites the authored model string"
    );
}

// ── C1 / F2: the final candidate meets the config read inside the lock ──

fn pinned_normal(id: &str, name: &str) -> Agent {
    let mut agent = claude_agent(id, name);
    agent.config.prompt_mode = Some(PromptMode::Normal);
    agent
}

fn inherit() -> UpdateAgentFields {
    UpdateAgentFields {
        prompt_mode: Some(None),
        ..Default::default()
    }
}

/// QA-1 / review-b4 F1 in one long-lived engine (the TUI's shape): custom
/// template, every agent pinned normal, engine built while the project was
/// normal; then another writer (the TUI's F3, or `orch config set` in another
/// process) legally switches the project to eco. An edit back to "inherit"
/// would now render eco against the custom template, so every locked
/// surface refuses it and writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn an_inherit_edit_after_a_concurrent_switch_to_eco_is_refused_on_every_locked_surface() {
    let rig = rig_with(
        custom_template_raw("normal"),
        vec![
            pinned_normal("agt_a", "Alpha"),
            pinned_normal("agt_b", "Beta"),
        ],
    )
    .await;
    set_project_prompt_mode(rig.dir.path(), rig.engine.agents.as_ref(), PromptMode::Eco)
        .await
        .expect("every agent overrides normal, so project eco is legal");
    let svc = &rig.engine.agent_service;
    let before = rig.agents_snapshot();

    let lock = DefinitionLock::acquire(rig.dir.path()).await.unwrap();
    assert_eq!(lock.config().defaults.agent.prompt_mode, PromptMode::Eco);

    let err = svc
        .update_locked(&lock, "agt_a", inherit())
        .await
        .expect_err("flag/wizard update to inherit");
    assert!(err.to_string().contains("inherits project eco"), "{err}");

    let mut config = rig.agent("agt_a").await.config;
    config.prompt_mode = None;
    let err = svc
        .replace_definition_locked(&lock, "agt_a", "Alpha".into(), ADAPTER.into(), None, config)
        .await
        .expect_err("replacement to inherit");
    assert!(err.to_string().contains("inherits project eco"), "{err}");

    let err = svc
        .create_locked(
            &lock,
            orch_domain::CreateAgentInput {
                name: "Fresh".into(),
                adapter: ADAPTER.into(),
                ..Default::default()
            },
        )
        .await
        .expect_err("a new agent inherits by default");
    assert!(err.to_string().contains("inherits project eco"), "{err}");

    drop(lock);
    assert_eq!(
        rig.agents_snapshot(),
        before,
        "no definition or index byte changed"
    );
    assert_eq!(
        svc.list().await.unwrap().len(),
        2,
        "the refused create left no agent"
    );
}

/// The reverse stale case: an engine built while the project was eco must
/// not refuse a legitimate inherit edit after the project went back to normal.
#[tokio::test(flavor = "multi_thread")]
async fn an_inherit_edit_after_a_concurrent_switch_back_to_normal_is_accepted_under_the_lock() {
    let rig = rig_with(
        custom_template_raw("eco"),
        vec![pinned_normal("agt_a", "Alpha")],
    )
    .await;
    set_project_prompt_mode(
        rig.dir.path(),
        rig.engine.agents.as_ref(),
        PromptMode::Normal,
    )
    .await
    .unwrap();

    let lock = DefinitionLock::acquire(rig.dir.path()).await.unwrap();
    let authored = rig
        .engine
        .agent_service
        .update_locked(&lock, "agt_a", inherit())
        .await
        .expect("the project is normal now, so inherit is compatible");
    drop(lock);

    assert_eq!(authored.agent.config.prompt_mode, None);
    assert_eq!(rig.agent("agt_a").await.config.prompt_mode, None);
}

/// `DefinitionLock::acquire` really takes the lock every project-config writer
/// takes, so the config it reads cannot change until it is dropped.
#[tokio::test(flavor = "multi_thread")]
async fn a_definition_lock_waits_for_a_held_lock_before_reading_the_config() {
    let rig = rig_with(base_raw(), vec![]).await;
    let held = fs_utils::mutation_lock(&rig.paths.root().join(".definition.lock"))
        .await
        .unwrap();

    let root = rig.dir.path().to_path_buf();
    let acquire = tokio::spawn(async move {
        DefinitionLock::acquire(&root)
            .await
            .map(|lock| lock.config().defaults.agent.prompt_mode)
    });
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !acquire.is_finished(),
        "acquire must wait for the held lock"
    );

    // The holder changes the mode before releasing: the waiter sees it.
    let store = FileConfigStore::new(rig.paths.clone());
    let mut raw = store.read_raw().await.unwrap();
    raw["defaults"]["agent"]["prompt_mode"] = json!("eco");
    store.write_raw(&raw, false).await.unwrap();
    drop(held);

    assert_eq!(acquire.await.unwrap().unwrap(), PromptMode::Eco);
}

// ── ruling 13: shell effort ─────────────────────────────────────────────

fn shell_agent(id: &str) -> Agent {
    let mut agent = claude_agent(id, "Sheller");
    agent.adapter = "shell".into();
    agent.config.command = Some("printf shell-ok".into());
    agent
}

fn with_effort(effort: ReasoningEffort) -> UpdateAgentFields {
    UpdateAgentFields {
        effort: Some(Some(effort)),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_effort_for_a_shell_agent_is_refused_before_the_normalization_drops_it() {
    let mut claude = claude_with_effort("claude-opus-5-5", ReasoningEffort::Max);
    claude.id = "agt_c".into();
    let rig = rig_with(json!({}), vec![shell_agent("agt_s"), claude]).await;
    let svc = &rig.engine.agent_service;
    let lock = DefinitionLock::acquire(rig.dir.path()).await.unwrap();
    let before = rig.agents_snapshot();

    let err = svc
        .update_locked(&lock, "agt_s", with_effort(ReasoningEffort::Max))
        .await
        .expect_err("shell has no effort channel");
    let message = err.to_string();
    assert!(
        message.contains("agent agt_s:")
            && message.contains("does not accept effort `max` for adapter `shell`")
            && message.contains("--effort default"),
        "{message}"
    );

    // Switching into shell carries the stored new spelling onto the target.
    let err = svc
        .update_locked(
            &lock,
            "agt_c",
            UpdateAgentFields {
                adapter: Some("shell".into()),
                command: Some("printf shell-ok".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("max carried into shell");
    assert!(
        err.to_string().contains("`max` for adapter `shell`"),
        "{err}"
    );

    let err = svc
        .create_locked(
            &lock,
            orch_domain::CreateAgentInput {
                name: "NewShell".into(),
                adapter: "shell".into(),
                command: Some("printf shell-ok".into()),
                effort: Some(ReasoningEffort::Xhigh),
                ..Default::default()
            },
        )
        .await
        .expect_err("xhigh on a new shell agent");
    assert!(
        err.to_string().contains("`xhigh` for adapter `shell`"),
        "{err}"
    );
    assert_eq!(rig.agents_snapshot(), before, "nothing written");

    // Clearing the effort in the same switch is the remedy.
    let authored = svc
        .update_locked(
            &lock,
            "agt_c",
            UpdateAgentFields {
                adapter: Some("shell".into()),
                command: Some("printf shell-ok".into()),
                effort: Some(None),
                ..Default::default()
            },
        )
        .await
        .expect("switch with effort cleared");
    assert_eq!(authored.agent.config.effort, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_legacy_effort_requested_for_a_shell_agent_is_ignored_with_a_warning_and_not_stored() {
    let rig = rig_with(json!({}), vec![shell_agent("agt_s")]).await;
    let svc = &rig.engine.agent_service;
    let lock = DefinitionLock::acquire(rig.dir.path()).await.unwrap();

    let authored = svc
        .update_locked(&lock, "agt_s", with_effort(ReasoningEffort::High))
        .await
        .expect("legacy high is not newly rejected");
    assert_eq!(authored.agent.config.effort, None);
    assert_eq!(
        rig.agent("agt_s").await.config.effort,
        None,
        "stored without effort"
    );
    assert!(
        authored.diagnostics.len() == 1
            && authored.diagnostics[0].agent_id == "agt_s"
            && authored.diagnostics[0]
                .message
                .starts_with("legacy-ignored"),
        "{:?}",
        authored.diagnostics
    );

    let created = svc
        .create_locked(
            &lock,
            orch_domain::CreateAgentInput {
                name: "NewShell".into(),
                adapter: "shell".into(),
                command: Some("printf shell-ok".into()),
                effort: Some(ReasoningEffort::Low),
                ..Default::default()
            },
        )
        .await
        .expect("legacy low on a new shell agent");
    assert_eq!(created.agent.config.effort, None);
    assert!(
        created
            .diagnostics
            .iter()
            .any(|d| d.message.starts_with("legacy-ignored")),
        "{:?}",
        created.diagnostics
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_legacy_shell_effort_refuses_an_unrelated_edit_until_cleared() {
    let mut stored = shell_agent("agt_s");
    stored.config.effort = Some(ReasoningEffort::Medium);
    let rig = rig_with(json!({}), vec![stored]).await;
    let svc = &rig.engine.agent_service;
    let lock = DefinitionLock::acquire(rig.dir.path()).await.unwrap();
    let before = rig.agents_snapshot();

    let err = svc
        .update_locked(
            &lock,
            "agt_s",
            UpdateAgentFields {
                role: Some("Unrelated".into()),
                ..Default::default()
            },
        )
        .await
        .expect_err("stored incompatible combination (D5-7)");
    assert!(err.to_string().contains("--effort default"), "{err}");
    assert_eq!(rig.agents_snapshot(), before);

    svc.update_locked(
        &lock,
        "agt_s",
        UpdateAgentFields {
            role: Some("Unrelated".into()),
            effort: Some(None),
            ..Default::default()
        },
    )
    .await
    .expect("clearing repairs it in the same mutation");
}
