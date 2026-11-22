//! Test-only lease state: the reference for the fenced rules, held per project.
//! Pure and synchronous; the daemon's lineage is passed in by the storage that
//! owns it, because it belongs to the daemon and not to a project.

use std::collections::BTreeMap;

use super::*;
use crate::store::{Caller, Principal, StoreError};

#[derive(Clone, Default)]
pub struct LeaseState {
    /// By task id: a task holds at most one lease.
    leases: BTreeMap<String, Lease>,
    /// Highest epoch ever issued per task. Never decreased, never removed.
    epochs: BTreeMap<String, u64>,
}

/// What a lease-bound call names the lease by.
#[derive(Clone, Copy)]
pub enum By<'a> {
    Task(&'a str),
    Run(&'a str),
}

impl LeaseState {
    pub fn claim(&mut self, lineage: &str, new: &NewLease, now_ms: i64) -> Result<ClaimOutcome> {
        let in_the_way = self
            .leases
            .get(&new.task)
            .or_else(|| self.leases.values().find(|l| l.agent == new.agent))
            .or_else(|| self.leases.values().find(|l| l.run == new.run));
        if let Some(lease) = in_the_way {
            return Ok(ClaimOutcome::Held(lease.clone()));
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
        let lease = Lease {
            task: new.task.clone(),
            agent: new.agent.clone(),
            run: new.run.clone(),
            lineage: lineage.into(),
            epoch,
            expires_at,
        };
        self.leases.insert(new.task.clone(), lease.clone());
        Ok(ClaimOutcome::Granted(lease))
    }

    pub fn renew(
        &mut self,
        caller: &Caller,
        current: &str,
        task: &str,
        lineage: &str,
        epoch: u64,
        now_ms: i64,
    ) -> Result<Lease> {
        self.check(caller, current, By::Task(task), lineage, epoch)?;
        let expires_at = expiry(now_ms)?;
        let lease = self.leases.get_mut(task).ok_or(StoreError::StaleEpoch)?;
        lease.expires_at = expires_at;
        Ok(lease.clone())
    }

    pub fn release(
        &mut self,
        caller: &Caller,
        current: &str,
        task: &str,
        lineage: &str,
        epoch: u64,
    ) -> Result<Lease> {
        self.check(caller, current, By::Task(task), lineage, epoch)?;
        self.leases.remove(task).ok_or(StoreError::StaleEpoch)
    }

    /// The one fence every lease-bound call shares, event append included.
    /// A missing lease, another lineage or epoch, or a task principal that is
    /// not this lease's holder are all the same refusal: stale.
    pub fn check(
        &self,
        caller: &Caller,
        current: &str,
        by: By<'_>,
        lineage: &str,
        epoch: u64,
    ) -> Result<&Lease> {
        let lease = match by {
            By::Task(task) => self.leases.get(task),
            By::Run(run) => self.leases.values().find(|l| l.run == run),
        }
        .ok_or(StoreError::StaleEpoch)?;
        if lineage != current || lease.lineage != lineage || lease.epoch != epoch {
            return Err(StoreError::StaleEpoch);
        }
        if let Principal::Task {
            task,
            run,
            lineage,
            epoch,
            ..
        } = &caller.principal
            && (task != &lease.task
                || run != &lease.run
                || lineage != &lease.lineage
                || *epoch != lease.epoch)
        {
            return Err(StoreError::StaleEpoch);
        }
        Ok(lease)
    }

    pub fn by_task(&self, task: &str) -> Option<Lease> {
        self.leases.get(task).cloned()
    }

    pub fn by_agent(&self, agent: &str) -> Option<Lease> {
        self.leases.values().find(|l| l.agent == agent).cloned()
    }

    pub fn all(&self) -> Vec<Lease> {
        self.leases.values().cloned().collect()
    }

    /// A restore ends every lease and keeps every counter (R138).
    pub fn end_all(&mut self) {
        self.leases.clear();
    }
}

fn expiry(now_ms: i64) -> Result<i64> {
    now_ms
        .checked_add(LEASE_MS)
        .ok_or(StoreError::CounterExhausted)
}
