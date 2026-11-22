//! Today's scheduler: the 1.0 selection rules as a read-only decision.
//!
//! [`LegacyScheduler`] implements [`Scheduler`] over a [`SchedulerView`] and
//! chooses what the daemon's tick chose. The order is the daemon's: tasks by
//! priority, goal-bound before free, most recently updated first, then the
//! store's order; for each, the checks of `daemon_task_eligible`, then the best
//! idle agent by [`choose_agent`]. Each pair counts the agent, the local slot and
//! the task it takes, as the next launch of the same tick would see them.
//!
//! It writes nothing. What 1.0 did with the outcome (the claim, the scope-overlap
//! event and its repeat policy, recording a final failure) stays in the engine.
//! The sorts are stable (AGENTS.md rule 3), and ties are decided by the explicit
//! `ordinal` of the view.

use std::collections::{BTreeSet, HashMap, HashSet};

use orch_core::scheduler::{
    Assignment, Decision, Held, HoldReason, LeasePhase, Request, SchedAgent, SchedTask, Scheduler,
    SchedulerView,
};
use orch_domain::{
    Agent, AgentStatus, Task, TaskSource, TaskStatus, is_blocked, is_dispatchable, scopes_overlap,
};

/// The 1.0 selection rules, unchanged.
#[derive(Debug, Default, Clone, Copy)]
pub struct LegacyScheduler;

/// Tasks by id, for dependency checks.
struct TaskIndex<'a>(HashMap<&'a str, &'a Task>);

impl TaskSource for TaskIndex<'_> {
    fn find_task(&self, id: &str) -> Option<&Task> {
        self.0.get(id).copied()
    }
}

impl Scheduler for LegacyScheduler {
    fn decide(&self, view: &SchedulerView) -> Decision {
        let index = TaskIndex(
            view.tasks
                .iter()
                .map(|t| (t.task.id.as_str(), &t.task))
                .collect(),
        );
        let mut agents: Vec<&SchedAgent> = view.agents.iter().collect();
        agents.sort_by_key(|a| a.ordinal);

        // What the launches before this one in the tick would have taken.
        let mut claimed: HashSet<&str> = view
            .leases
            .iter()
            .filter(|l| l.phase != LeasePhase::Draining)
            .map(|l| l.task_id.as_str())
            .collect();
        let draining: HashSet<&str> = view.leases.iter().map(|l| l.task_id.as_str()).collect();
        let mut busy_agents: HashSet<&str> = view
            .leases
            .iter()
            .filter_map(|l| l.agent_id.as_deref())
            .collect();
        let mut local_slots = view
            .leases
            .iter()
            .filter(|l| !l.remote && matches!(l.phase, LeasePhase::Running | LeasePhase::Preparing))
            .count();
        let mut taken: BTreeSet<&str> = BTreeSet::new();

        let (candidates, override_retry_wait) = candidates(view);
        let mut decision = Decision::default();
        for entry in candidates {
            let task = &entry.task;
            // Held by a claim, a run or an output still being collected.
            if draining.contains(task.id.as_str()) || taken.contains(task.id.as_str()) {
                continue;
            }
            match eligible(view, &index, task, &claimed, override_retry_wait) {
                Eligibility::Yes => {}
                Eligibility::No => continue,
                Eligibility::Hold(reason) => {
                    decision.held.push(Held {
                        task_id: task.id.clone(),
                        reason,
                    });
                    continue;
                }
            }
            let at_capacity = local_slots >= view.max_local_runs;
            let pool: Vec<Agent> = agents
                .iter()
                .filter(|a| {
                    (!at_capacity || a.agent.remote.is_some())
                        && !busy_agents.contains(a.agent.id.as_str())
                })
                .map(|a| a.agent.clone())
                .collect();
            let Some(agent) = choose_agent(task, pool) else {
                continue;
            };
            let Some(chosen) = agents.iter().find(|a| a.agent.id == agent.id) else {
                continue;
            };
            if chosen.agent.remote.is_none() {
                local_slots += 1;
            }
            busy_agents.insert(chosen.agent.id.as_str());
            claimed.insert(task.id.as_str());
            taken.insert(task.id.as_str());
            decision.assignments.push(Assignment {
                task_id: task.id.clone(),
                agent_id: agent.id,
            });
        }
        decision
    }
}

/// The tasks to consider, in dispatch order, and whether a retry's back-off is
/// overridden.
fn candidates(view: &SchedulerView) -> (Vec<&SchedTask>, bool) {
    match &view.request {
        Request::Tick => {
            let mut tasks: Vec<&SchedTask> = view
                .tasks
                .iter()
                .filter(|t| is_dispatchable(t.task.status))
                .collect();
            tasks.sort_by(|a, b| {
                a.task
                    .priority
                    .cmp(&b.task.priority)
                    .then_with(|| a.task.goal_id.is_none().cmp(&b.task.goal_id.is_none()))
                    .then_with(|| b.task.updated_at.cmp(&a.task.updated_at))
                    .then_with(|| a.ordinal.cmp(&b.ordinal))
            });
            (tasks, false)
        }
        Request::Launch {
            task_id,
            override_retry_wait,
        } => (
            view.tasks
                .iter()
                .filter(|t| &t.task.id == task_id)
                .collect(),
            *override_retry_wait,
        ),
    }
}

enum Eligibility {
    Yes,
    No,
    Hold(HoldReason),
}

/// The checks of `daemon_task_eligible`, in its order, then the unrecorded final
/// failure that the launch checks next.
fn eligible(
    view: &SchedulerView,
    index: &TaskIndex<'_>,
    task: &Task,
    claimed: &HashSet<&str>,
    override_retry_wait: bool,
) -> Eligibility {
    if !is_dispatchable(task.status) || task.awaits_approval() {
        return Eligibility::No;
    }
    if let Some(controller) = task
        .labels
        .iter()
        .find_map(|label| label.strip_prefix("remote-ctl:"))
        && view.controller.as_deref() != Some(controller)
    {
        return Eligibility::No;
    }
    if task.status == TaskStatus::Retrying
        && !override_retry_wait
        && view
            .retries
            .iter()
            .any(|retry| retry.task_id == task.id && view.now_ms < retry.due_at_ms)
    {
        return Eligibility::No;
    }
    if task
        .goal_id
        .as_ref()
        .is_some_and(|goal| view.paused_goals.contains(goal))
    {
        return Eligibility::No;
    }
    if is_blocked(task, index) {
        return Eligibility::No;
    }
    if task.scope.as_deref().is_some_and(|scope| !scope.is_empty()) {
        let blocker = view
            .tasks
            .iter()
            .filter(|other| {
                other.task.id != task.id
                    && claimed.contains(other.task.id.as_str())
                    && scopes_overlap(task.scope.as_deref(), other.task.scope.as_deref())
            })
            .map(|other| other.task.id.as_str())
            .min();
        if let Some(blocker) = blocker {
            return Eligibility::Hold(HoldReason::ScopeOverlap {
                blocker: blocker.to_owned(),
            });
        }
    }
    if task.status == TaskStatus::Retrying && task.attempts >= task.max_attempts {
        return Eligibility::Hold(HoldReason::FinalFailureUnrecorded);
    }
    Eligibility::Yes
}

/// Pick the best idle agent for a task.
///
/// - An explicit `assignee` is a **hard constraint**: it is matched by id
///   or name across *all* agents and returned only if idle. Otherwise the
///   task waits; it is never handed to a different agent.
/// - Otherwise: **+50 per** task label equal (case-insensitively) to one of
///   the agent's skills, **+30** if any label is a case-insensitive
///   substring of the role, **+20** for being idle, and **+0..10** for the
///   success rate.
///
/// The final `sort_by` is **stable**, so equal scores keep the order of
/// `agents`. Callers pass them in scheduling order, which is how tied agents
/// rotate.
pub(crate) fn choose_agent(task: &Task, agents: Vec<Agent>) -> Option<Agent> {
    let available: Vec<Agent> = agents
        .iter()
        .filter(|a| agent_eligible(task, a))
        .cloned()
        .collect();

    if available.is_empty() {
        return None;
    }

    if let Some(assignee) = task.assignee.as_deref().filter(|a| !a.is_empty()) {
        let assigned = agents
            .into_iter()
            .find(|a| a.id == assignee || a.name == assignee);
        return assigned.filter(|a| agent_eligible(task, a));
    }

    // Lower-cased once because the inner loops are O(n·m).
    let lower_labels: Vec<String> = task.labels.iter().map(|l| l.to_lowercase()).collect();
    let labels = (!lower_labels.is_empty()).then_some(lower_labels.as_slice());

    let mut scored: Vec<(i64, Agent)> = available
        .into_iter()
        .map(|agent| (score_agent(&agent, labels), agent))
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));

    scored.into_iter().next().map(|(_, agent)| agent)
}

/// Capability labels constrain explicit and automatic assignment alike.
pub fn agent_eligible(task: &Task, agent: &Agent) -> bool {
    agent.status == AgentStatus::Idle
        && agent.remote.as_ref().is_none_or(|remote| remote.online)
        && task
            .labels
            .iter()
            .filter_map(|label| label.strip_prefix("cap:"))
            .all(|cap| agent.capabilities.iter().any(|actual| actual == cap))
}

/// The agent scoring. Split out so it is unit-testable without
/// a store, and so the weights sit in one place.
pub(crate) fn score_agent(agent: &Agent, lower_labels: Option<&[String]>) -> i64 {
    let mut score = 0i64;

    if let (Some(labels), Some(skills)) = (lower_labels, agent.config.skills.as_deref())
        && !skills.is_empty()
    {
        let skill_set: HashSet<String> = skills.iter().map(|s| s.to_lowercase()).collect();
        score += 50 * labels.iter().filter(|l| skill_set.contains(*l)).count() as i64;
    }

    // `contains`, not equality: a label of "end" matches both "frontend" and
    // "backend". That is a known false-positive source that the tests assert on
    // deliberately.
    if let (Some(labels), Some(role)) = (lower_labels, agent.role.as_deref()) {
        let lower_role = role.to_lowercase();
        if labels.iter().any(|l| lower_role.contains(l.as_str())) {
            score += 30;
        }
    }

    if agent.status == AgentStatus::Idle {
        score += 20;
    }

    let total = agent.stats.tasks_completed + agent.stats.tasks_failed;
    if total > 0 {
        score += js_round(agent.stats.tasks_completed as f64 / total as f64 * 10.0);
    }

    score
}

/// `Math.round`: half rounds **up** (towards +∞), unlike Rust's `f64::round`,
/// which rounds half away from zero. The inputs here are non-negative so the
/// two agree, but spelling it out keeps the intent checkable.
fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}
