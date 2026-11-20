# 0018. Model fenced leases independently of the product

- Status: Proposed
- Date: 2026-11-20

 The owner requested a throwaway experiment, not adoption of its corrections.
Phase 1's owner gate remains required before product code.

## Context

Task `tsk_jrPTVUS` needs storage evidence before the engine depends on SQLite leases.
This model starts a new elapsed-clock domain on each daemon restart; system-wide
suspend-inclusive clocks can survive a process restart within one boot. Snapshot rollback removes issued
counter values. An old token must not match a newly issued run after restore.

## Decision

For this experiment only, use a standalone Rust workspace with rusqlite 0.41.1
(default features off; bundled, backup, cache), following entry step C's library audit.
Keep tests and schema outside the product workspace and all product dependencies.
Tempfile is already a maintained, approved test dependency (3.27) in the root workspace.
Run cargo-deny against this workspace using the repository policy before delivery.

Model suspend-inclusive elapsed time explicitly. UTC is audit data only. A holder's
90 s deadline starts at renewal request send time, not acknowledgement receipt time;
late acknowledgement cannot revive a stopped holder. Every resumed holder checks
current authority before work. Restart invalidates session authority but reserves
recoverable leases for 120 s. Restore selects a fresh lineage and discards old leases;
repair task counters from the restored runs/leases/counters before releasing work.
Immediate redispatch after restore requires the daemon stopped and every previous
holder proven terminated. Otherwise dispatch must stay quarantined for at least
LEASE (120 s) after restore; the model does not enforce process termination or quarantine.
Lineage is the durable fence across restarts; session is the liveness fence for reattachment.
Reusing a session label across restore must never defeat the lineage fence.

## Consequences

This tests SQLite transactions and state-machine rules, not an OS clock adapter or
worker protocol. A new lineage prevents compound fencing-token reuse, but numeric
counters can repeat across snapshot rollback. If globally increasing numeric epochs
are mandatory, restore needs a durable high-water mark outside the restored image;
that is an owner decision, not something a snapshot scan can establish.

The experiment report lists the exact rules requiring correction. Its schema is
throwaway and does not become a product migration or port contract.
