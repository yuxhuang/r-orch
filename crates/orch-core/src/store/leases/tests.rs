//! Implementation-specific checks that cannot be reached through the port.
#![allow(
    clippy::unwrap_used,
    reason = "fixture helper outside #[test]; failures terminate the scenario"
)]

use super::*;
use crate::store::{Caller, Principal, StoreError};
fn owner(project: &str) -> Caller {
    Caller {
        project: project.into(),
        principal: Principal::Owner {
            identity: "synthetic".into(),
        },
    }
}
fn new(task: &str, agent: &str, run: &str) -> NewLease {
    NewLease {
        task: task.into(),
        agent: agent.into(),
        run: run.into(),
    }
}
const P: &str = "proj";
/// The two lineage checks each refuse a case the other lets through: a token
/// of an earlier lineage against the daemon's current one, and a token of the
/// current lineage against a lease a restore left behind. Only the state can show
/// the first, because the storage's restore ends every lease.
mod lineage_checks {
    use super::*;
    use crate::store::leases::fake::{By, LeaseState};

    fn state() -> (LeaseState, Lease) {
        let mut state = LeaseState::default();
        let ClaimOutcome::Granted(lease) = state.claim("old", &new("t1", "a1", "r1"), 0).unwrap()
        else {
            panic!("claim refused");
        };
        (state, lease)
    }

    #[test]
    fn a_lease_left_behind_by_a_restore_is_refused_under_its_own_lineage() {
        let (state, lease) = state();
        let c = owner(P);
        assert!(state.check(&c, "old", By::Task("t1"), "old", 1).is_ok());
        assert!(matches!(
            state.check(&c, "new", By::Task("t1"), "old", 1),
            Err(StoreError::StaleEpoch)
        ));
        assert!(matches!(
            state.check(&c, "new", By::Run("r1"), "old", lease.epoch),
            Err(StoreError::StaleEpoch)
        ));
    }

    #[test]
    fn a_token_of_the_current_lineage_is_refused_by_a_lease_of_an_earlier_one() {
        // Only the lease's own lineage can refuse this: the token matches the daemon's.
        let (state, _) = state();
        assert!(matches!(
            state.check(&owner(P), "new", By::Task("t1"), "new", 1),
            Err(StoreError::StaleEpoch)
        ));
    }

    #[test]
    fn a_lease_is_found_by_its_run_as_well_as_by_its_task() {
        let (state, lease) = state();
        let found = state
            .check(&owner(P), "old", By::Run("r1"), "old", 1)
            .unwrap();
        assert_eq!(found, &lease);
        assert!(
            state
                .check(&owner(P), "old", By::Run("r2"), "old", 1)
                .is_err()
        );
    }
}
