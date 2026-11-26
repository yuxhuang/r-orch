//! Isolated reservation slice of R35, on the landed entity and lease port.
//!
//! This is not a process-start operation. The integrator must supply start
//! intents, retry-row removal for the other two reservations (only
//! [`claim_due_retry`] deletes a row), admission (disk floor/run cap/unspooled
//! finishes), dependency/goal/scope checks and dispatcher authorization before
//! wiring it.
//! A `retrying` task is reserved only by [`claim_due_retry`], and only once the
//! `due_at` of its retry row has passed; [`claim`] and [`claim_explicit_rerun`]
//! refuse it. An automatic retry keeps its attempt count, so it never resets the
//! budget; only [`claim_explicit_rerun`] does, and only for a failed or
//! cancelled task.
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
    /// A due-retry reservation of a `retrying` task that has no retry row.
    /// 1.0 dispatched such a task at once; whether and how a task whose row
    /// is gone is recovered is for the integrator (T12), not for this helper.
    NoRetry,
    /// A due-retry reservation before the row's `due_at` (UTC milliseconds).
    RetryNotDue {
        due_at: i64,
    },
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
    /// A `retrying` task whose retry row is due keeps the attempts it has and
    /// loses its row.
    DueRetry,
}

impl Mode {
    fn accepts(self, status: TaskStatus) -> bool {
        match self {
            Self::Dispatch => status == TaskStatus::Todo,
            Self::ExplicitRerun => matches!(status, TaskStatus::Failed | TaskStatus::Cancelled),
            Self::DueRetry => status == TaskStatus::Retrying,
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

/// The reservation of an automatic retry that has come due. The task must be
/// `retrying` and its retry row must be due (`due_at <= now_ms`, the retry
/// port's rule and 1.0's `now >= due_at`; a row that 1.0 could not parse has no
/// counterpart, since `due_at` is a number here). The row is read inside the
/// transaction, so a row replaced after the scheduler chose the task is judged
/// by its stored deadline. The budget is kept, not reset: the run is attempt
/// `attempts + 1` of the task, as for [`claim`]; the row's own `attempt` is
/// not interpreted (retry port). The row is deleted with the lease, the run row
/// and the task update, so a refusal (approval hold, no row, not yet due,
/// unavailable agent, held lease) or a storage error leaves the task, its row
/// and its attempts as they were. A `todo`, failed or cancelled task is refused
/// whatever rows it has, and any other row of the table is untouched. Like
/// [`claim`] it checks no admission, dependency or goal rule and starts
/// nothing.
pub async fn claim_due_retry(
    storage: &(impl Storage + ?Sized),
    caller: Caller,
    request: ClaimRequest,
) -> Result<ClaimResult> {
    storage
        .transaction(caller, Lane::Claims, move |unit| {
            reserve(unit, request, Mode::DueRetry)
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
    if mode == Mode::DueRetry {
        let Some(retry) = unit.get_retry(&request.task_id)? else {
            return Ok(ClaimResult::Refused(Refusal::NoRetry));
        };
        if retry.due_at > request.now_ms {
            return Ok(ClaimResult::Refused(Refusal::RetryNotDue {
                due_at: retry.due_at,
            }));
        }
    }
    if crate::agent_service::AgentService::find_best_in(&task_data, vec![*agent_data]).is_none() {
        return Ok(ClaimResult::Refused(Refusal::AgentUnavailable));
    }
    // The reset is only a local value until the task update below, so every
    // refusal and error before it leaves the stored budget as it was.
    let spent = match mode {
        Mode::Dispatch | Mode::DueRetry => task_data.attempts,
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
    if mode == Mode::DueRetry {
        unit.delete_retry(&request.task_id)?;
    }
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
