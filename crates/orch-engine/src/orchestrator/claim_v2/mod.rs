//! Isolated reservation slice of R35, on the landed entity and lease port.
//!
//! This is not a process-start operation. The integrator must supply retry
//! removal, start intents, admission (disk floor/run cap/unspooled finishes),
//! dependency/goal/scope checks and dispatcher authorization before wiring it.
//! Retrying tasks are deliberately refused until their queue can participate.
//! An automatic retry keeps its attempt count, so it never resets the budget;
//! only [`claim_explicit_rerun`] does, and only for a failed or cancelled task.
//! Prompt blobs (#134), workspace preparation and peer submission stay outside
//! this helper. A preparing run stores empty prompt/workspace placeholders;
//! nothing here emits a started event or launches a process.
//!
//! A reservation sets the task in progress and assigns its selected agent, but
//! leaves the agent entity idle: the lease, rather than agent status, excludes
//! another reservation. It is only a partial R35 transaction, not authorization
//! or a durable intent to start work. No production dispatcher calls it.

use orch_core::clock::iso_from_epoch_ms;
use orch_core::store::leases::{ClaimOutcome, Lease, NewLease};
use orch_core::store::{
    Caller, Entity, EntityKind, Lane, Record, Result, Revision, Storage, StorageExt, StoreError,
    UnitOfWork,
};
use orch_domain::{Run, RunStatus, TaskStatus};

/// Scheduler decision plus the revisions of the records it observed. This
/// helper validates that selection; it never chooses a different task or agent.
#[derive(Debug, Clone)]
pub struct ClaimRequest {
    pub task_id: String,
    pub task_revision: Revision,
    pub agent_id: String,
    pub agent_revision: Revision,
    pub run_id: String,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    TaskUnavailable,
    AgentUnavailable,
    Held(Lease),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimResult {
    Reserved {
        task: Box<Record>,
        run: Box<Record>,
        lease: Lease,
    },
    Refused(Refusal),
}

/// Which task states a reservation accepts, and what it does to the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A `todo` task keeps the attempts it has, as before.
    Dispatch,
    /// A `failed` or `cancelled` task starts again with a fresh budget, which
    /// is what 1.0's `retry` followed by the next dispatch did in two saves.
    ExplicitRerun,
}

impl Mode {
    fn accepts(self, status: TaskStatus) -> bool {
        match self {
            Self::Dispatch => status == TaskStatus::Todo,
            Self::ExplicitRerun => matches!(status, TaskStatus::Failed | TaskStatus::Cancelled),
        }
    }
}

/// Returns a reservation only after commit. Any storage error aborts the entire
/// transaction, including a newly issued lease and its epoch. A repeated claim
/// is refused; it is not a replay key or permission to launch again.
pub async fn claim(
    storage: &(impl Storage + ?Sized),
    caller: Caller,
    request: ClaimRequest,
) -> Result<ClaimResult> {
    storage
        .transaction(caller, Lane::Claims, move |unit| {
            reserve(unit, request, Mode::Dispatch)
        })
        .await
}

/// The reservation of an explicit rerun of a `failed` or `cancelled` task (1.0's
/// `orch task retry` and the dispatch that follows it). The attempt count is
/// reset to zero and then spent by this reservation, so the run is attempt 1 of
/// a fresh budget, in the same transaction as the lease, the run row and the
/// task update: a refusal (approval hold, unavailable agent, held lease) or a
/// storage error after the reset leaves the task failed or cancelled with the
/// attempts it had. Every other status is refused, `retrying` included: an
/// automatic retry never resets the budget, and `todo` goes through [`claim`].
/// Like [`claim`] it checks no admission, dependency or goal rule and starts
/// nothing.
pub async fn claim_explicit_rerun(
    storage: &(impl Storage + ?Sized),
    caller: Caller,
    request: ClaimRequest,
) -> Result<ClaimResult> {
    storage
        .transaction(caller, Lane::Claims, move |unit| {
            reserve(unit, request, Mode::ExplicitRerun)
        })
        .await
}

fn selected(
    unit: &dyn UnitOfWork,
    kind: EntityKind,
    id: &str,
    expected: Revision,
) -> Result<Record> {
    match unit.get(kind, id)? {
        Some(record) if record.revision == expected => Ok(record),
        actual => Err(StoreError::StaleRevision {
            kind,
            id: id.into(),
            expected,
            actual: actual.map(|record| record.revision),
        }),
    }
}

fn reserve(unit: &mut dyn UnitOfWork, request: ClaimRequest, mode: Mode) -> Result<ClaimResult> {
    let task = selected(
        unit,
        EntityKind::Task,
        &request.task_id,
        request.task_revision,
    )?;
    let agent = selected(
        unit,
        EntityKind::Agent,
        &request.agent_id,
        request.agent_revision,
    )?;
    let Entity::Task(mut task_data) = task.entity else {
        return Ok(ClaimResult::Refused(Refusal::TaskUnavailable));
    };
    let Entity::Agent(agent_data) = agent.entity else {
        return Ok(ClaimResult::Refused(Refusal::AgentUnavailable));
    };
    if !mode.accepts(task_data.status) || task_data.awaits_approval() {
        return Ok(ClaimResult::Refused(Refusal::TaskUnavailable));
    }
    if crate::agent_service::AgentService::find_best_in(&task_data, vec![*agent_data]).is_none() {
        return Ok(ClaimResult::Refused(Refusal::AgentUnavailable));
    }
    // The reset is only a local value until the task update below, so every
    // refusal and error before it leaves the stored budget as it was.
    let spent = match mode {
        Mode::Dispatch => task_data.attempts,
        Mode::ExplicitRerun => 0,
    };
    let attempt = spent.checked_add(1).ok_or(StoreError::CounterExhausted)?;
    let lease = match unit.claim_lease(
        &NewLease {
            task: request.task_id.clone(),
            agent: request.agent_id.clone(),
            run: request.run_id.clone(),
        },
        request.now_ms,
    )? {
        ClaimOutcome::Granted(lease) => lease,
        ClaimOutcome::Held(lease) => return Ok(ClaimResult::Refused(Refusal::Held(lease))),
    };
    let now = iso_from_epoch_ms(request.now_ms);
    let run = unit.create(Entity::Run(Box::new(Run {
        id: request.run_id,
        task_id: request.task_id,
        agent_id: request.agent_id.clone(),
        attempt,
        status: RunStatus::Preparing,
        started_at: now.clone(),
        finished_at: None,
        workspace_path: String::new(),
        prompt: String::new(),
        pid: None,
        error: None,
        error_kind: None,
        tokens: None,
    })))?;
    task_data.status = TaskStatus::InProgress;
    task_data.assignee = Some(request.agent_id);
    task_data.attempts = attempt;
    task_data.updated_at = now;
    let task = unit.update(Entity::Task(task_data), request.task_revision)?;
    Ok(ClaimResult::Reserved {
        task: Box::new(task),
        run: Box::new(run),
        lease,
    })
}

#[cfg(test)]
mod tests;
