//! The read-only scheduler decision (spec R55, R56): which task goes to which
//! agent, from synthetic tasks, agents and leases only. No store, no daemon, no
//! agent CLI. The vectors are the 1.0 ones: the stable sort of `find_best_in`,
//! the tick's task order and `daemon_task_eligible`.

use std::collections::BTreeSet;

use orch_core::scheduler::{
    Assignment, Decision, Held, HoldReason, LeasePhase, LeaseView, Request, RetryWait, SchedAgent,
    SchedTask, Scheduler, SchedulerView,
};
use orch_core::test_support::{make_agent, make_task};
use orch_domain::{Agent, AgentStatus, RemoteRef, Task, TaskStatus};
use orch_engine::scheduler::LegacyScheduler;

const NOW: i64 = 1_000_000;

fn task(id: &str) -> Task {
    Task {
        id: id.into(),
        ..make_task()
    }
}

fn labelled(id: &str, labels: &[&str]) -> Task {
    Task {
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        ..task(id)
    }
}

fn agent(id: &str) -> Agent {
    Agent {
        id: id.into(),
        name: format!("name-of-{id}"),
        ..make_agent()
    }
}

fn remote(id: &str, online: bool) -> Agent {
    Agent {
        remote: Some(RemoteRef {
            alias: "peer".into(),
            online,
        }),
        ..agent(id)
    }
}

/// Tasks and agents take their ordinals from their position, as a store lists them.
fn view(tasks: Vec<Task>, agents: Vec<Agent>) -> SchedulerView {
    SchedulerView {
        request: Request::Tick,
        now_ms: NOW,
        tasks: tasks
            .into_iter()
            .enumerate()
            .map(|(n, task)| SchedTask {
                ordinal: n as u64,
                task,
            })
            .collect(),
        agents: agents
            .into_iter()
            .enumerate()
            .map(|(n, agent)| SchedAgent {
                ordinal: n as u64,
                agent,
            })
            .collect(),
        leases: Vec::new(),
        retries: Vec::new(),
        paused_goals: BTreeSet::new(),
        controller: None,
        max_local_runs: 8,
    }
}

fn lease(task_id: &str, agent_id: Option<&str>, phase: LeasePhase) -> LeaseView {
    LeaseView {
        task_id: task_id.into(),
        agent_id: agent_id.map(str::to_owned),
        phase,
        remote: false,
    }
}

fn decide(view: &SchedulerView) -> Decision {
    LegacyScheduler.decide(view)
}

fn pairs(decision: &Decision) -> Vec<(&str, &str)> {
    decision
        .assignments
        .iter()
        .map(|a| (a.task_id.as_str(), a.agent_id.as_str()))
        .collect()
}

/// The agent chosen for one task, with every agent otherwise free.
fn pick(task: Task, agents: Vec<Agent>) -> Option<String> {
    let id = task.id.clone();
    decide(&view(vec![task], agents))
        .assignments
        .into_iter()
        .find(|a| a.task_id == id)
        .map(|a| a.agent_id)
}

// ── R55: it only decides ─────────────────────────────────────────────────

#[test]
fn scheduler_has_no_write_access() {
    // The decision takes the view by shared reference and is not async, so it
    // can reach no store, clock or process; the scheduler itself holds nothing.
    assert_eq!(std::mem::size_of::<LegacyScheduler>(), 0);
    let mut v = view(
        vec![task("tsk_a"), task("tsk_b")],
        vec![agent("agt_1"), agent("agt_2")],
    );
    v.leases
        .push(lease("tsk_c", Some("agt_3"), LeasePhase::Running));
    let before = v.clone();
    let first = decide(&v);
    let second = decide(&v);
    assert_eq!(v, before, "the view is unchanged by a decision");
    assert_eq!(first, second, "a decision is a function of the view");
    assert_eq!(pairs(&first), [("tsk_a", "agt_1"), ("tsk_b", "agt_2")]);
}

#[test]
fn a_decision_can_be_used_through_the_port() {
    let scheduler: Box<dyn Scheduler> = Box::new(LegacyScheduler);
    let v = view(vec![task("tsk_a")], vec![agent("agt_1")]);
    assert_eq!(
        scheduler.decide(&v).assignments,
        [Assignment {
            task_id: "tsk_a".into(),
            agent_id: "agt_1".into()
        }]
    );
}

// ── Task order ───────────────────────────────────────────────────────────

fn with(id: &str, priority: i64, goal: Option<&str>, updated: &str) -> Task {
    Task {
        priority,
        goal_id: goal.map(str::to_owned),
        updated_at: updated.into(),
        ..task(id)
    }
}

fn order_of(tasks: Vec<Task>) -> Vec<String> {
    // Agents enough for every task, so only the order shows.
    let agents = (0..tasks.len())
        .map(|n| agent(&format!("agt_{n}")))
        .collect();
    decide(&view(tasks, agents))
        .assignments
        .into_iter()
        .map(|a| a.task_id)
        .collect()
}

#[test]
fn dispatch_order_is_priority_then_goal_then_recency_then_store_order() {
    let tasks = vec![
        with("tsk_p3_free_new", 3, None, "2026-01-02"),
        with("tsk_p3_goal_old", 3, Some("goal_1"), "2026-01-01"),
        with("tsk_p1_free", 1, None, "2026-01-01"),
        with("tsk_p3_goal_new", 3, Some("goal_1"), "2026-01-02"),
        with("tsk_p3_free_old", 3, None, "2026-01-01"),
        // Equal on every key with tsk_p3_free_new: the store's order decides.
        with("tsk_p3_free_new_later", 3, None, "2026-01-02"),
        with("tsk_p2_goal", 2, Some("goal_2"), "2026-01-01"),
    ];
    assert_eq!(
        order_of(tasks),
        [
            "tsk_p1_free",
            "tsk_p2_goal",
            "tsk_p3_goal_new",
            "tsk_p3_goal_old",
            "tsk_p3_free_new",
            "tsk_p3_free_new_later",
            "tsk_p3_free_old",
        ]
    );
}

#[test]
fn the_order_follows_the_ordinal_and_not_the_position_in_the_view() {
    let mut v = view(
        vec![task("tsk_first"), task("tsk_second"), task("tsk_third")],
        vec![agent("agt_1"), agent("agt_2"), agent("agt_3")],
    );
    v.tasks.reverse();
    v.agents.reverse();
    assert_eq!(
        pairs(&decide(&v)),
        [
            ("tsk_first", "agt_1"),
            ("tsk_second", "agt_2"),
            ("tsk_third", "agt_3")
        ]
    );
}

#[test]
fn only_todo_and_retrying_tasks_are_candidates() {
    let mut tasks = Vec::new();
    for (id, status) in [
        ("tsk_todo", TaskStatus::Todo),
        ("tsk_retrying", TaskStatus::Retrying),
        ("tsk_progress", TaskStatus::InProgress),
        ("tsk_review", TaskStatus::Review),
        ("tsk_done", TaskStatus::Done),
        ("tsk_failed", TaskStatus::Failed),
        ("tsk_cancelled", TaskStatus::Cancelled),
    ] {
        tasks.push(Task { status, ..task(id) });
    }
    assert_eq!(order_of(tasks), ["tsk_todo", "tsk_retrying"]);
}

// ── Agent choice: scores and ties ────────────────────────────────────────

#[test]
fn tied_agents_rotate() {
    // The storage conformance vector: three agents of equal score, listed
    // b, a, c. Each run saves its agent, which 1.0 moved to the end of the
    // index; the ordinal says the same thing explicitly.
    let mut order: Vec<String> = ["agt_b", "agt_a", "agt_c"].map(String::from).to_vec();
    let mut chosen = Vec::new();
    for _ in 0..6 {
        let v = view(
            vec![task("tsk_work")],
            order.iter().map(|id| agent(id)).collect(),
        );
        let picked = pick_one(&v);
        order.retain(|id| *id != picked);
        order.push(picked.clone());
        chosen.push(picked);
    }
    assert_eq!(
        chosen,
        ["agt_b", "agt_a", "agt_c", "agt_b", "agt_a", "agt_c"]
    );
}

fn pick_one(v: &SchedulerView) -> String {
    let decision = decide(v);
    assert_eq!(decision.assignments.len(), 1);
    decision.assignments[0].agent_id.clone()
}

#[test]
fn ties_go_to_the_lower_ordinal_whatever_the_view_order() {
    let mut v = view(
        vec![task("tsk_work")],
        vec![agent("agt_x"), agent("agt_y"), agent("agt_z")],
    );
    v.agents.swap(0, 2);
    assert_eq!(pick_one(&v), "agt_x");
    // An agent saved again takes the next ordinal and so goes last.
    v.agents[2].ordinal = 10;
    assert_eq!(pick_one(&v), "agt_y");
}

fn skilled(id: &str, skills: &[&str]) -> Agent {
    let mut a = agent(id);
    a.config.skills = Some(skills.iter().map(|s| (*s).to_owned()).collect());
    a
}

fn with_role(id: &str, role: &str) -> Agent {
    Agent {
        role: Some(role.into()),
        ..agent(id)
    }
}

fn with_stats(id: &str, completed: u64, failed: u64) -> Agent {
    let mut a = agent(id);
    a.stats.tasks_completed = completed;
    a.stats.tasks_failed = failed;
    a.stats.total_runs = completed + failed;
    a
}

#[test]
fn a_skill_match_beats_a_role_match() {
    let first = Agent {
        role: Some("backend developer".into()),
        ..skilled("agt_1", &["frontend", "css"])
    };
    let second = skilled("agt_2", &["backend", "api"]);
    assert_eq!(
        pick(labelled("tsk_t", &["backend"]), vec![first, second]).as_deref(),
        Some("agt_2")
    );
}

#[test]
fn a_label_matches_a_role_by_substring_with_its_false_positive() {
    // "end" is inside both roles; the earlier agent wins the tie.
    let agents = vec![
        with_role("agt_1", "frontend"),
        with_role("agt_2", "backend developer"),
    ];
    assert_eq!(
        pick(labelled("tsk_t", &["end"]), agents.clone()).as_deref(),
        Some("agt_1")
    );
    assert_eq!(
        pick(labelled("tsk_t", &["backend"]), agents).as_deref(),
        Some("agt_2")
    );
}

#[test]
fn skills_match_labels_case_insensitively_and_stack_per_label() {
    let agents = vec![
        skilled("agt_1", &["TypeScript"]),
        skilled("agt_2", &["typescript", "REACT"]),
    ];
    assert_eq!(
        pick(labelled("tsk_t", &["typescript", "react"]), agents).as_deref(),
        Some("agt_2")
    );
}

#[test]
fn the_success_rate_breaks_what_labels_leave_equal() {
    // 20 idle + round(10 * rate): 1/3 -> 3, 1/2 -> 5, 2/3 -> 7, 1/1 -> 10,
    // none run -> 0 (a task with no run keeps only the idle bonus).
    let agents = vec![
        with_stats("agt_none", 0, 0),
        with_stats("agt_third", 1, 2),
        with_stats("agt_half", 1, 1),
        with_stats("agt_twothirds", 2, 1),
    ];
    assert_eq!(
        pick(task("tsk_t"), agents.clone()).as_deref(),
        Some("agt_twothirds")
    );
    let mut all = agents;
    all.push(with_stats("agt_perfect", 1, 0));
    assert_eq!(pick(task("tsk_t"), all).as_deref(), Some("agt_perfect"));
}

#[test]
fn the_success_rate_is_rounded_not_floored() {
    // 2/3 scores 20 + round(6.67) = 27, the same as 7/10; the earlier agent
    // wins that tie. A rate that is floored scores 26 and loses.
    let agents = vec![
        with_stats("agt_two_thirds", 2, 1),
        with_stats("agt_seven_tenths", 7, 3),
    ];
    assert_eq!(
        pick(task("tsk_t"), agents).as_deref(),
        Some("agt_two_thirds")
    );
}

#[test]
fn an_explicit_assignee_is_a_hard_constraint_by_id_or_name() {
    let mut named = agent("agt_1");
    named.name = "backend-dev".into();
    let other = agent("agt_2");
    for assignee in ["agt_1", "backend-dev"] {
        let t = Task {
            assignee: Some(assignee.into()),
            ..task("tsk_t")
        };
        assert_eq!(
            pick(t, vec![other.clone(), named.clone()]).as_deref(),
            Some("agt_1"),
            "{assignee}"
        );
    }
    let missing = Task {
        assignee: Some("agt_nonexistent".into()),
        ..task("tsk_t")
    };
    assert_eq!(pick(missing, vec![named.clone(), other.clone()]), None);
}

#[test]
fn a_task_waits_for_a_busy_assignee_and_is_not_handed_to_another_agent() {
    let t = Task {
        assignee: Some("agt_1".into()),
        ..task("tsk_t")
    };
    let mut v = view(vec![t], vec![agent("agt_1"), agent("agt_2")]);
    v.leases
        .push(lease("tsk_other", Some("agt_1"), LeasePhase::Running));
    assert!(decide(&v).assignments.is_empty());
    // Not idle in the directory is the same.
    let mut v = view(
        vec![Task {
            assignee: Some("agt_1".into()),
            ..task("tsk_t")
        }],
        vec![
            Agent {
                status: AgentStatus::Running,
                ..agent("agt_1")
            },
            agent("agt_2"),
        ],
    );
    assert!(decide(&v).assignments.is_empty());
    v.agents[0].agent.status = AgentStatus::Idle;
    assert_eq!(pairs(&decide(&v)), [("tsk_t", "agt_1")]);
}

#[test]
fn capability_labels_and_offline_peers_exclude_an_agent() {
    let mut capable = agent("agt_capable");
    capable.capabilities = vec!["gpu".into()];
    let plain = agent("agt_plain");
    assert_eq!(
        pick(
            labelled("tsk_t", &["cap:gpu"]),
            vec![plain.clone(), capable.clone()]
        )
        .as_deref(),
        Some("agt_capable")
    );
    assert_eq!(
        pick(
            labelled("tsk_t", &["cap:tpu"]),
            vec![plain.clone(), capable]
        ),
        None
    );
    assert_eq!(
        pick(task("tsk_t"), vec![remote("agt_peer", false), plain]).as_deref(),
        Some("agt_plain")
    );
    assert_eq!(pick(task("tsk_t"), vec![remote("agt_peer", false)]), None);
}

// ── Eligibility ──────────────────────────────────────────────────────────

fn assigned(v: &SchedulerView, id: &str) -> bool {
    decide(v).assignments.iter().any(|a| a.task_id == id)
}

#[test]
fn a_task_held_for_approval_waits() {
    let v = view(
        vec![labelled("tsk_t", &["awaiting-approval"])],
        vec![agent("agt_1")],
    );
    assert!(!assigned(&v, "tsk_t"));
}

#[test]
fn a_mirror_runs_only_for_its_controller() {
    let mut v = view(
        vec![labelled("tsk_t", &["remote-ctl:ctl_1"])],
        vec![agent("agt_1")],
    );
    assert!(!assigned(&v, "tsk_t"), "no controller attached");
    v.controller = Some("ctl_2".into());
    assert!(!assigned(&v, "tsk_t"), "another controller");
    v.controller = Some("ctl_1".into());
    assert!(assigned(&v, "tsk_t"));
}

#[test]
fn a_retry_waits_for_its_back_off_unless_overridden() {
    let retrying = Task {
        status: TaskStatus::Retrying,
        attempts: 1,
        ..task("tsk_t")
    };
    let mut v = view(vec![retrying], vec![agent("agt_1")]);
    v.retries.push(RetryWait {
        task_id: "tsk_t".into(),
        due_at_ms: NOW + 1,
    });
    assert!(!assigned(&v, "tsk_t"), "not yet due");
    v.retries[0].due_at_ms = NOW;
    assert!(assigned(&v, "tsk_t"), "due exactly now");
    v.retries[0].due_at_ms = NOW + 1;
    v.request = Request::Launch {
        task_id: "tsk_t".into(),
        override_retry_wait: true,
    };
    assert!(assigned(&v, "tsk_t"), "`orch run <id>` overrides it");
    v.request = Request::Launch {
        task_id: "tsk_t".into(),
        override_retry_wait: false,
    };
    assert!(!assigned(&v, "tsk_t"));
}

#[test]
fn a_back_off_holds_only_a_retrying_task() {
    let mut v = view(vec![task("tsk_t")], vec![agent("agt_1")]);
    v.retries.push(RetryWait {
        task_id: "tsk_t".into(),
        due_at_ms: NOW + 60_000,
    });
    assert!(assigned(&v, "tsk_t"));
}

#[test]
fn a_paused_goal_holds_its_tasks_only() {
    let mut v = view(
        vec![
            Task {
                goal_id: Some("goal_1".into()),
                ..task("tsk_in_goal")
            },
            Task {
                goal_id: Some("goal_2".into()),
                ..task("tsk_other_goal")
            },
            task("tsk_free"),
        ],
        vec![agent("agt_1"), agent("agt_2"), agent("agt_3")],
    );
    v.paused_goals.insert("goal_1".into());
    let decision = decide(&v);
    assert_eq!(
        pairs(&decision),
        [("tsk_other_goal", "agt_1"), ("tsk_free", "agt_2")]
    );
}

#[test]
fn dependencies_block_until_done_and_a_missing_one_is_resolved() {
    let dep = |id: &str, status| Task { status, ..task(id) };
    let dependent = Task {
        depends_on: vec!["tsk_dep".into()],
        ..task("tsk_t")
    };
    let agents = || vec![agent("agt_1")];
    for status in [TaskStatus::Todo, TaskStatus::InProgress, TaskStatus::Review] {
        let v = view(vec![dep("tsk_dep", status), dependent.clone()], agents());
        assert!(!assigned(&v, "tsk_t"), "{status:?}");
    }
    let v = view(
        vec![dep("tsk_dep", TaskStatus::Done), dependent.clone()],
        agents(),
    );
    assert!(assigned(&v, "tsk_t"));
    // Deleted after creation: deleted means done.
    let v = view(vec![dependent], agents());
    assert!(assigned(&v, "tsk_t"));
}

#[test]
fn a_task_that_already_holds_a_lease_is_not_started_again() {
    for phase in [
        LeasePhase::Claimed,
        LeasePhase::Preparing,
        LeasePhase::Running,
        LeasePhase::Draining,
    ] {
        let mut v = view(vec![task("tsk_t")], vec![agent("agt_1"), agent("agt_2")]);
        v.leases.push(lease("tsk_t", None, phase));
        assert!(!assigned(&v, "tsk_t"), "{phase:?}");
    }
}

#[test]
fn a_retrying_task_with_no_attempt_left_is_reported_and_not_assigned() {
    let spent = Task {
        status: TaskStatus::Retrying,
        attempts: 3,
        max_attempts: 3,
        ..task("tsk_spent")
    };
    let v = view(vec![spent, task("tsk_next")], vec![agent("agt_1")]);
    let decision = decide(&v);
    assert_eq!(pairs(&decision), [("tsk_next", "agt_1")]);
    assert_eq!(
        decision.held,
        [Held {
            task_id: "tsk_spent".into(),
            reason: HoldReason::FinalFailureUnrecorded
        }]
    );
}

// ── Scope overlap ────────────────────────────────────────────────────────

fn scoped(id: &str, scope: &[&str]) -> Task {
    Task {
        scope: Some(scope.iter().map(|s| (*s).to_owned()).collect()),
        ..task(id)
    }
}

#[test]
fn a_task_overlapping_a_leased_scope_is_held_with_the_lowest_blocker() {
    let mut v = view(
        vec![
            scoped("tsk_new", &["src/api/**"]),
            scoped("tsk_b", &["src/api/**"]),
            scoped("tsk_a", &["src/**"]),
            scoped("tsk_far", &["docs/**"]),
        ],
        vec![agent("agt_1"), agent("agt_2")],
    );
    v.leases
        .push(lease("tsk_b", Some("agt_8"), LeasePhase::Running));
    v.leases.push(lease("tsk_a", None, LeasePhase::Claimed));
    let decision = decide(&v);
    assert_eq!(
        decision.held,
        [Held {
            task_id: "tsk_new".into(),
            reason: HoldReason::ScopeOverlap {
                blocker: "tsk_a".into()
            }
        }]
    );
    assert_eq!(pairs(&decision), [("tsk_far", "agt_1")]);
}

#[test]
fn a_draining_run_no_longer_reserves_its_scope() {
    let mut v = view(
        vec![
            scoped("tsk_new", &["src/**"]),
            scoped("tsk_old", &["src/**"]),
        ],
        vec![agent("agt_1")],
    );
    v.leases.push(lease("tsk_old", None, LeasePhase::Draining));
    assert!(assigned(&v, "tsk_new"));
}

#[test]
fn an_assignment_reserves_its_scope_for_the_tasks_after_it() {
    let v = view(
        vec![
            scoped("tsk_first", &["src/**"]),
            scoped("tsk_second", &["src/lib/**"]),
        ],
        vec![agent("agt_1"), agent("agt_2")],
    );
    let decision = decide(&v);
    assert_eq!(pairs(&decision), [("tsk_first", "agt_1")]);
    assert_eq!(
        decision.held,
        [Held {
            task_id: "tsk_second".into(),
            reason: HoldReason::ScopeOverlap {
                blocker: "tsk_first".into()
            }
        }]
    );
}

#[test]
fn the_overlap_is_reported_even_when_no_agent_is_free() {
    // 1.0 judged eligibility, and so reported, before it looked for an agent.
    let mut v = view(vec![scoped("tsk_new", &["src/**"])], vec![]);
    v.leases.push(lease("tsk_old", None, LeasePhase::Claimed));
    v.tasks.push(SchedTask {
        ordinal: 9,
        task: scoped("tsk_old", &["src/**"]),
    });
    let decision = decide(&v);
    assert!(decision.assignments.is_empty());
    assert_eq!(decision.held.len(), 1);
}

// ── Agents in use and the local slot limit ───────────────────────────────

#[test]
fn agents_that_hold_a_lease_are_not_offered() {
    let mut v = view(
        vec![task("tsk_t")],
        vec![
            agent("agt_running"),
            agent("agt_preparing"),
            agent("agt_free"),
        ],
    );
    v.leases
        .push(lease("tsk_a", Some("agt_running"), LeasePhase::Running));
    v.leases
        .push(lease("tsk_b", Some("agt_preparing"), LeasePhase::Preparing));
    assert_eq!(pairs(&decide(&v)), [("tsk_t", "agt_free")]);
}

#[test]
fn one_agent_serves_one_task_of_a_decision() {
    let v = view(
        vec![task("tsk_a"), task("tsk_b"), task("tsk_c")],
        vec![agent("agt_1"), agent("agt_2")],
    );
    assert_eq!(pairs(&decide(&v)), [("tsk_a", "agt_1"), ("tsk_b", "agt_2")]);
}

#[test]
fn local_slots_cap_local_agents_and_leave_remote_peers_free() {
    let mut v = view(
        vec![task("tsk_a"), task("tsk_b")],
        vec![agent("agt_local"), remote("agt_peer", true)],
    );
    v.leases
        .push(lease("tsk_run", Some("agt_busy"), LeasePhase::Running));
    v.max_local_runs = 1;
    // The local slot is full: the only agent left is the peer's.
    assert_eq!(pairs(&decide(&v)), [("tsk_a", "agt_peer")]);
    v.max_local_runs = 2;
    assert_eq!(
        pairs(&decide(&v)),
        [("tsk_a", "agt_local"), ("tsk_b", "agt_peer")]
    );
}

#[test]
fn each_local_assignment_takes_a_slot_of_the_same_decision() {
    let mut v = view(
        vec![task("tsk_a"), task("tsk_b"), task("tsk_c")],
        vec![agent("agt_1"), agent("agt_2"), remote("agt_peer", true)],
    );
    v.max_local_runs = 1;
    assert_eq!(
        pairs(&decide(&v)),
        [("tsk_a", "agt_1"), ("tsk_b", "agt_peer")]
    );
}

#[test]
fn a_remote_run_holds_no_local_slot() {
    let mut v = view(vec![task("tsk_a")], vec![agent("agt_local")]);
    v.leases.push(LeaseView {
        remote: true,
        ..lease("tsk_run", Some("agt_peer"), LeasePhase::Running)
    });
    v.max_local_runs = 1;
    assert_eq!(pairs(&decide(&v)), [("tsk_a", "agt_local")]);
}

#[test]
fn a_zero_slot_limit_starts_nothing_locally() {
    let mut v = view(vec![task("tsk_a")], vec![agent("agt_local")]);
    v.max_local_runs = 0;
    assert!(decide(&v).assignments.is_empty());
}

// ── A launch request ─────────────────────────────────────────────────────

#[test]
fn a_launch_considers_only_the_named_task() {
    let mut v = view(
        vec![task("tsk_a"), task("tsk_b")],
        vec![agent("agt_1"), agent("agt_2")],
    );
    v.request = Request::Launch {
        task_id: "tsk_b".into(),
        override_retry_wait: false,
    };
    assert_eq!(pairs(&decide(&v)), [("tsk_b", "agt_1")]);
    v.request = Request::Launch {
        task_id: "tsk_missing".into(),
        override_retry_wait: false,
    };
    assert_eq!(decide(&v), Decision::default());
}
