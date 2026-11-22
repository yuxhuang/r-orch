//! Isolated reservation slice of R35, on the landed entity and lease port.
//!
//! This is not a process-start operation. The integrator must supply retry
//! removal, start intents, admission (disk floor/run cap/unspooled finishes),
//! dependency/goal/scope checks and dispatcher authorization before wiring it.
//! Retrying tasks are deliberately refused until their queue can participate.
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

/// Returns a reservation only after commit. Any storage error aborts the entire
/// transaction, including a newly issued lease and its epoch. A repeated claim
/// is refused; it is not a replay key or permission to launch again.
pub async fn claim(
    storage: &(impl Storage + ?Sized),
    caller: Caller,
    request: ClaimRequest,
) -> Result<ClaimResult> {
    storage
        .transaction(caller, Lane::Claims, move |unit| reserve(unit, request))
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

fn reserve(unit: &mut dyn UnitOfWork, request: ClaimRequest) -> Result<ClaimResult> {
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
    if task_data.status != TaskStatus::Todo || task_data.awaits_approval() {
        return Ok(ClaimResult::Refused(Refusal::TaskUnavailable));
    }
    if crate::agent_service::AgentService::find_best_in(&task_data, vec![*agent_data]).is_none() {
        return Ok(ClaimResult::Refused(Refusal::AgentUnavailable));
    }
    let attempt = task_data
        .attempts
        .checked_add(1)
        .ok_or(StoreError::CounterExhausted)?;
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
