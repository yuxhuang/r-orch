//! What the frozen domain types actually emit as YAML.
//!
//! This is **not** WP-1's `write_yaml` — there is no timestamp guard here. The
//! point is to pin down, with evidence rather than reasoning, exactly which
//! scalars `serde_yaml_ng` leaves unquoted, because an unquoted
//! `created_at: 2026-10-13T16:37:58.959Z` re-parses in the TypeScript binary as
//! a JavaScript `Date` and makes `MessageStore.list()` throw
//! (PARITY.md §6.4, ARCHITECTURE §5.3, R1).
//!
//! WP-1 owns the fix: a post-emit pass inside `write_yaml` that force-quotes
//! any plain scalar js-yaml's default schema would resolve as a YAML 1.1
//! timestamp. These tests are the "before" half of that work.

use orch_domain::{
    Agent, AgentConfig, AgentStats, AgentStatus, ApprovalPolicy, Goal, GoalStatus, Message,
    MessageChannel, MessageStatus, Task, TaskStatus,
};

fn task() -> Task {
    Task {
        id: "tsk_k3GVRqg".into(),
        title: "Second task".into(),
        description: String::new(),
        status: TaskStatus::Todo,
        priority: 3,
        assignee: None,
        labels: vec![],
        depends_on: vec![],
        created_at: "2026-10-13T16:37:33.426Z".into(),
        updated_at: "2026-10-13T16:37:33.426Z".into(),
        attempts: 0,
        max_attempts: 3,
        workspace_mode: None,
        workspace: None,
        proof: None,
        review_criteria: None,
        review_results: None,
        scope: None,
        feedback: None,
        goal_id: None,
        attachments: None,
    }
}

#[test]
fn task_yaml_keeps_the_interface_field_order_and_omits_optionals() {
    let yaml = serde_yaml_ng::to_string(&task()).expect("serializes");
    let keys: Vec<&str> = yaml
        .lines()
        .filter(|l| !l.starts_with(' ') && l.contains(':'))
        .map(|l| l.split(':').next().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "title",
            "description",
            "status",
            "priority",
            "labels",
            "depends_on",
            "created_at",
            "updated_at",
            "attempts",
            "max_attempts",
        ]
    );
    assert!(!yaml.contains("assignee"));
    assert!(!yaml.contains("goalId"));
}

#[test]
fn enums_emit_their_snake_case_string_values() {
    let mut t = task();
    t.status = TaskStatus::InProgress;
    t.workspace_mode = Some(orch_domain::WorkspaceMode::Worktree);
    let yaml = serde_yaml_ng::to_string(&t).expect("serializes");
    assert!(yaml.contains("status: in_progress\n"), "{yaml}");
    assert!(yaml.contains("workspace_mode: worktree\n"), "{yaml}");
}

#[test]
fn goal_id_keeps_its_camel_case_key_in_yaml() {
    let mut t = task();
    t.goal_id = Some("goal_717sF1B".into());
    let yaml = serde_yaml_ng::to_string(&t).expect("serializes");
    assert!(yaml.contains("goalId: goal_717sF1B\n"), "{yaml}");
}

#[test]
fn an_agent_env_map_round_trips_in_declaration_order() {
    let agent = Agent {
        id: "agt_veWG_kK".into(),
        name: "shelly".into(),
        adapter: "shell".into(),
        role: None,
        config: AgentConfig {
            command: Some("echo hello".into()),
            approval_policy: Some(ApprovalPolicy::Auto),
            env: Some(
                [
                    ("ZZZ".to_owned(), "1".to_owned()),
                    ("AAA".to_owned(), "2".to_owned()),
                ]
                .into_iter()
                .collect(),
            ),
            ..AgentConfig::default()
        },
        status: AgentStatus::Idle,
        current_task: None,
        autonomous: None,
        stats: AgentStats::default(),
        last_error: None,
    };
    let yaml = serde_yaml_ng::to_string(&agent).expect("serializes");
    let zzz = yaml.find("ZZZ").expect("ZZZ present");
    let aaa = yaml.find("AAA").expect("AAA present");
    assert!(zzz < aaa, "insertion order is not preserved:\n{yaml}");

    let back: Agent = serde_yaml_ng::from_str(&yaml).expect("round-trips");
    assert_eq!(back, agent);
}

/// The hazard, pinned. Every one of these is a **string** in Rust and must
/// come back as a string in the TypeScript binary.
#[test]
fn timestamp_scalars_are_emitted_unquoted_which_is_the_cross_compat_hazard() {
    let yaml = serde_yaml_ng::to_string(&task()).expect("serializes");
    assert!(
        yaml.contains("created_at: 2026-10-13T16:37:33.426Z\n"),
        "if this now emits quoted, re-check whether WP-1's guard is still needed:\n{yaml}"
    );
}

/// The set of timestamp-shaped fields WP-1's guard has to cover, enumerated
/// across every entity so none is forgotten.
#[test]
fn every_timestamp_field_across_the_entities_emits_unquoted() {
    const TS: &str = "2026-10-13T16:37:33.426Z";

    let mut t = task();
    t.created_at = TS.into();
    t.updated_at = TS.into();

    let goal = Goal {
        id: "goal_1".into(),
        title: "t".into(),
        description: String::new(),
        status: GoalStatus::Active,
        assignee: None,
        created_at: TS.into(),
        updated_at: Some(TS.into()),
    };

    let message = Message {
        id: "msg_1".into(),
        channel: MessageChannel::Direct,
        from_agent_id: "cli".into(),
        to_agent_id: Some("agt_creator".into()),
        subject: "Hi".into(),
        body: "hello".into(),
        created_at: TS.into(),
        expires_at: Some(TS.into()),
        status: MessageStatus::Pending,
        delivered_at: Some(TS.into()),
        team_id: None,
        reply_to: None,
    };

    let documents = [
        (
            "task",
            serde_yaml_ng::to_string(&t).unwrap(),
            vec!["created_at", "updated_at"],
        ),
        (
            "goal",
            serde_yaml_ng::to_string(&goal).unwrap(),
            vec!["created_at", "updated_at"],
        ),
        (
            "message",
            serde_yaml_ng::to_string(&message).unwrap(),
            vec!["created_at", "expires_at", "delivered_at"],
        ),
    ];

    for (entity, yaml, fields) in &documents {
        for field in fields {
            assert!(
                yaml.contains(&format!("{field}: {TS}\n")),
                "{entity}.{field} is not an unquoted timestamp — the guard's \
                 field coverage may have changed:\n{yaml}"
            );
        }
    }
}

/// Scalars that are safe either way, so WP-1's guard does not need to widen to
/// them (PARITY.md §6.4).
#[test]
fn boolean_looking_and_numeric_looking_strings_are_already_quoted() {
    let mut t = task();
    for value in [
        "yes", "no", "on", "off", "true", "false", "null", "123", "1.5",
    ] {
        t.title = value.into();
        let yaml = serde_yaml_ng::to_string(&t).expect("serializes");
        let back: Task = serde_yaml_ng::from_str(&yaml).expect("round-trips");
        assert_eq!(back.title, value, "{value} did not survive:\n{yaml}");
    }
}

/// A description that happens to look like a date is the same hazard, which is
/// why ARCHITECTURE §5.3 keys the guard off the scalar grammar rather than off
/// field names.
#[test]
fn a_user_supplied_date_shaped_string_is_also_emitted_unquoted() {
    let mut t = task();
    t.description = "2026-10-13".into();
    let yaml = serde_yaml_ng::to_string(&t).expect("serializes");
    assert!(yaml.contains("description: 2026-10-13\n"), "{yaml}");
}
