//! Lease state of the file reference, written from the port's rules
//! (`orch_core::store::leases`) and not from the test double. A task holds at
//! most one lease and an agent at most one; a refused claim consumes no epoch;
//! the epoch counter outlives the lease; one fence serves every lease-bound call.

use std::collections::BTreeMap;

use orch_core::store::leases::{ClaimOutcome, LEASE_MS, Lease, NewLease};
use orch_core::store::{Caller, Principal, StoreError};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, StoreError>;

/// What a lease-bound call names the lease by.
#[derive(Clone, Copy)]
pub(super) enum By<'a> {
    Task(&'a str),
    Run(&'a str),
}

#[derive(Clone, Serialize, Deserialize)]
struct LeaseRow {
    agent: String,
    run: String,
    lineage: String,
    epoch: u64,
    expires_at: i64,
}

/// Leases of one project, and the epoch counters that outlive them.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Leases {
    /// By task id.
    leases: BTreeMap<String, LeaseRow>,
    /// Highest epoch ever issued per task. Never lowered, never removed.
    epochs: BTreeMap<String, u64>,
}

fn lease(task: &str, row: &LeaseRow) -> Lease {
    Lease {
        task: task.to_owned(),
        agent: row.agent.clone(),
        run: row.run.clone(),
        lineage: row.lineage.clone(),
        epoch: row.epoch,
        expires_at: row.expires_at,
    }
}

fn expiry(now_ms: i64) -> Result<i64> {
    now_ms
        .checked_add(LEASE_MS)
        .ok_or(StoreError::CounterExhausted)
}

impl Leases {
    /// A stored lease must be below its task's counter and must not share an
    /// agent or run with another; anything else is a document nobody wrote.
    pub(super) fn validate(&self) -> std::result::Result<(), String> {
        let mut agents = std::collections::BTreeSet::new();
        let mut runs = std::collections::BTreeSet::new();
        for (task, row) in &self.leases {
            if row.epoch > self.epochs.get(task).copied().unwrap_or(0) {
                return Err(format!("the lease of {task} is past its epoch counter"));
            }
            if !agents.insert(&row.agent) || !runs.insert(&row.run) {
                return Err(format!("the lease of {task} shares an agent or a run"));
            }
        }
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.leases.is_empty()
    }

    pub(super) fn claim(
        &mut self,
        lineage: &str,
        new: &NewLease,
        now_ms: i64,
    ) -> Result<ClaimOutcome> {
        let by_task = self.leases.get_key_value(&new.task);
        let by_agent = || self.leases.iter().find(|(_, l)| l.agent == new.agent);
        let by_run = || self.leases.iter().find(|(_, l)| l.run == new.run);
        if let Some((task, row)) = by_task.or_else(by_agent).or_else(by_run) {
            return Ok(ClaimOutcome::Held(lease(task, row)));
        }
        let expires_at = expiry(now_ms)?;
        let epoch = self
            .epochs
            .get(&new.task)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(StoreError::CounterExhausted)?;
        self.epochs.insert(new.task.clone(), epoch);
        let row = LeaseRow {
            agent: new.agent.clone(),
            run: new.run.clone(),
            lineage: lineage.to_owned(),
            epoch,
            expires_at,
        };
        let granted = lease(&new.task, &row);
        self.leases.insert(new.task.clone(), row);
        Ok(ClaimOutcome::Granted(granted))
    }

    pub(super) fn renew(
        &mut self,
        caller: &Caller,
        current: &str,
        task: &str,
        (lineage, epoch): (&str, u64),
        now_ms: i64,
    ) -> Result<Lease> {
        self.check(caller, current, By::Task(task), lineage, epoch)?;
        let expires_at = expiry(now_ms)?;
        let row = self.leases.get_mut(task).ok_or(StoreError::StaleEpoch)?;
        row.expires_at = expires_at;
        Ok(lease(task, row))
    }

    pub(super) fn release(
        &mut self,
        caller: &Caller,
        current: &str,
        task: &str,
        (lineage, epoch): (&str, u64),
    ) -> Result<Lease> {
        self.check(caller, current, By::Task(task), lineage, epoch)?;
        let row = self.leases.remove(task).ok_or(StoreError::StaleEpoch)?;
        Ok(lease(task, &row))
    }

    /// The one fence every lease-bound call shares, event append included. A
    /// missing lease, another lineage or epoch, or a task principal that is not
    /// this lease's holder are all the same refusal: stale.
    pub(super) fn check(
        &self,
        caller: &Caller,
        current: &str,
        by: By<'_>,
        lineage: &str,
        epoch: u64,
    ) -> Result<()> {
        let (task, row) = match by {
            By::Task(task) => self.leases.get_key_value(task),
            By::Run(run) => self.leases.iter().find(|(_, l)| l.run == run),
        }
        .ok_or(StoreError::StaleEpoch)?;
        if lineage != current || row.lineage != lineage || row.epoch != epoch {
            return Err(StoreError::StaleEpoch);
        }
        if let Principal::Task {
            task: held,
            run,
            lineage,
            epoch,
            ..
        } = &caller.principal
            && (held != task || run != &row.run || lineage != &row.lineage || *epoch != row.epoch)
        {
            return Err(StoreError::StaleEpoch);
        }
        Ok(())
    }

    pub(super) fn by_task(&self, task: &str) -> Option<Lease> {
        self.leases.get_key_value(task).map(|(t, l)| lease(t, l))
    }

    pub(super) fn by_agent(&self, agent: &str) -> Option<Lease> {
        self.leases
            .iter()
            .find(|(_, l)| l.agent == agent)
            .map(|(t, l)| lease(t, l))
    }

    /// In task order.
    pub(super) fn all(&self) -> Vec<Lease> {
        self.leases.iter().map(|(t, l)| lease(t, l)).collect()
    }

    /// A restore ends every lease and keeps every counter (R138).
    pub(super) fn end_all(&mut self) {
        self.leases.clear();
    }
}
