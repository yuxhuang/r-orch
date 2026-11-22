//! The scheduler port: a read-only decision over a snapshot.
//!
//! A [`Scheduler`] receives a [`SchedulerView`], plain data that holds no store
//! handle, and returns a [`Decision`]: the task-to-agent pairs it would start and
//! the tasks it holds back for a reason the engine reports. It never writes, and
//! it never starts anything. The engine's claim transaction is what stops a
//! double dispatch, so a pair that loses its claim costs one wasted attempt and
//! starts no run (spec R55, ADR 0017 decision 14).
//!
//! The view carries the order that 1.0 kept in `_index.json` as an explicit
//! `ordinal` on every task and agent. A tie between equal scores goes to the
//! lower ordinal, so the rotation of tied agents (every agent save moved the
//! agent to the end of the index) is a property of the data the adapter
//! supplies and not of the order of a slice.

use std::collections::BTreeSet;

use orch_domain::{Agent, Task};

/// A task as the scheduler sees it, with its place in the store's order.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedTask {
    /// Position in the store's order; lower comes first. Equal candidates keep
    /// this order.
    pub ordinal: u64,
    pub task: Task,
}

/// An agent as the scheduler sees it, with its place in the store's order.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedAgent {
    /// Position in the store's order; lower comes first. Agents with equal
    /// scores are chosen in this order, and 1.0 moved an agent to the end on
    /// every save, which is what rotated them.
    pub ordinal: u64,
    pub agent: Agent,
}

/// How far a task has got once something holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeasePhase {
    /// Claimed, with no run or preparation recorded (a claim that a crash left).
    Claimed,
    /// An agent is reserved and its workspace and prompt are being prepared.
    Preparing,
    /// A run is in progress.
    Running,
    /// The run has ended and its output is still being collected. The task
    /// cannot start again, but it no longer reserves its scope.
    Draining,
}

/// A task held by the engine, and the agent reserved for it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseView {
    pub task_id: String,
    pub agent_id: Option<String>,
    pub phase: LeasePhase,
    /// The agent is a remote peer's; it holds no local slot.
    pub remote: bool,
}

/// A retry that is queued and not yet due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryWait {
    pub task_id: String,
    /// When the retry comes due. An entry whose time cannot be read is left out
    /// by the adapter: 1.0 never held a task back for one.
    pub due_at_ms: i64,
}

/// What the caller asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// A tick: every dispatchable task, in dispatch order.
    Tick,
    /// One task, as `orch run <id>` asks for it. `override_retry_wait` starts a
    /// `retrying` task before its back-off is due.
    Launch {
        task_id: String,
        override_retry_wait: bool,
    },
}

/// Everything the 1.0 selection rules read, as plain data.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerView {
    pub request: Request,
    pub now_ms: i64,
    /// Every task of the project, whatever its status: dependencies and scope
    /// overlap are judged against all of them.
    pub tasks: Vec<SchedTask>,
    /// The agent directory, local and remote.
    pub agents: Vec<SchedAgent>,
    pub leases: Vec<LeaseView>,
    pub retries: Vec<RetryWait>,
    /// Goals whose tasks must wait.
    pub paused_goals: BTreeSet<String>,
    /// The controller session that remote mirrors belong to, when one is
    /// attached.
    pub controller: Option<String>,
    /// `scheduling.max_concurrent_agents`: local runs and local preparations
    /// together. A remote agent is exempt.
    pub max_local_runs: usize,
}

/// A pair the scheduler would start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub task_id: String,
    pub agent_id: String,
}

/// Why a task was held back for a reason the engine reports or acts on. A task
/// held for any other reason (not dispatchable, awaiting approval, blocked, no
/// agent free) simply waits and appears nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HoldReason {
    /// Its scope overlaps a task that holds a lease. `blocker` is the lowest
    /// id among them. 1.0 reports this once and repeats it after five minutes;
    /// that policy, and its remembered blocker, stay with the engine.
    ScopeOverlap { blocker: String },
    /// A `retrying` task with no attempt left: its last failure is counted and
    /// recording it, which writes, is the engine's job.
    FinalFailureUnrecorded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub task_id: String,
    pub reason: HoldReason,
}

/// What a scheduler decided. Pairs come in dispatch order; each already counts
/// the agent and the slot taken by the pairs before it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    pub assignments: Vec<Assignment>,
    pub held: Vec<Held>,
}

/// Decides which task goes to which agent. It has no access to a store, a
/// clock or a process: everything it may read is in the view.
pub trait Scheduler: Send + Sync {
    fn decide(&self, view: &SchedulerView) -> Decision;
}
