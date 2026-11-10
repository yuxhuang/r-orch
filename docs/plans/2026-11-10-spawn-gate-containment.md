# Spawn-gate waiter containment

status: active; builder verification pending
owner: senior engineer
date: 2026-11-10
spec: [daemon-owned state](../specs/2026-11-09-daemon-owned-state.md), R9, R10, R12, R14, R20, R21, R24, R25

## Authority and scope

R45 C1–C8 requires a coupled admission, ownership, telemetry and deterministic
control repair. R50 authorizes the seven caller/drain paths; R51 maps SG-C1 to
SG-R1 and SG-C2 to SG-R3, retaining SG-R2. R53 releases host entry only. R53a
qualifies fixture `c52527febf835d5511596756a41e27e067e433d9`; only its exact
two-path stack is integrated. Original base is
`25a0ceb29f9b1ee8889144ffecea4459770fd6b3`; the original proposed plan and
historical reports remain preserved in the task evidence.

The initial concrete revision preceded production edits and was mirrored to the
goal reports and Pensieve specs. This revision records the implemented wiring,
including the manager exit-observer and collector drainage discovered during the
lifetime audit. Acceptance remains at one committed SHA after all checks. No
frozen trait/existing callable signature, manifest, dependency, wire/storage
format, config/default, execution budget, skip or original assertion changes.

## Chosen design and alternatives

Choose yielding reservation plus synchronous handoff and owned entered work.
Offloading discovery alone leaves workspace/version callers and boundary-held
launch waiters. An observer timeout cannot revoke a kernel spawn or its permit.
An absolute PATH resolver leaves captured-pipe creation and adds new execution
semantics; it is outside the selected contract. Std spawn remains irreducible.

### Admission and lock order

`process/gate.rs` owns a single macOS Tokio semaphore. `reserve_spawn(site)`
creates a safe waiting record and awaits its owned permit without blocking a
polling worker or submitting a blocking job. Synchronous compatibility callers
use the same domain. Linux never acquires this permit.

Final dispatch order is reservation -> project operation boundary -> state mutex
-> existing task/goal/config/generation/shutdown revalidation -> actual spawn.
Cancellation uses existing task tokens and dispatch/shutdown guards, with the
existing 25 ms observation cadence. It releases preparation without attempts or
new retries. Goal pause is checked again under the final boundary. No deferred
launch bypasses that check.

The ordinary Send reservation may cross awaits for final boundary/state checks.
`SpawnReservation::run` puts it in a thread-local slot only around a synchronous
call. The real gated builder consumes it once; a restoration guard clears an
unused slot on early return/unwind. No thread/task-local reservation crosses an
await. The entered permit lasts from before pipe construction through actual
spawn return/error/unwind, and never through child output or lifetime.

Serving-route audit:

| Route | Admission / boundary relationship |
|---|---|
| Final adapter launch | Async reservation outside boundary, then revalidation and synchronous handoff |
| Workspace preparation, capture, merge/cleanup | Async reservation in capture; daemon preparation/completion external phases are outside boundary |
| Version probes / doctor | Async reservation before borrowed ProcessManager call; doctor uses track, not the operation permit |
| Model/diagnostic/review commands | Owned blocking job after yielding admission; reviews run outside completion commit |
| Hostname fallback | Reservation during container construction, before serving; full container fills peer.name and overrides only with Some, so peer startup's fallback is unreachable on this route |
| Peer worker launch | Same daemon orchestrator and injected adapters/manager |

The existing 19-construction inventory and structural guard remain required.
Snapshot clear/base/overlay/forced order, argv, cwd, stdio, process groups,
kill-on-drop and spawn error mapping are preserved. Actual agent/version calls
already entered in synchronous spawn remain uninterruptible; queued callers do
not inherit that polling-worker/boundary stall.

### Ownership, bounds and shutdown

One per-daemon SpawnWork registry is shared by snapshot diagnostics and the
snapshot manager. Diagnostic waiting observation begins before its job-slot wait.
At most 32 diagnostic slots are retained through capture or late cleanup; slot
waiters yield and inherit existing accepted-request/collector bounds. On macOS
only one job can be inside complete spawn; Linux shares the 32-slot job bound
without macOS serialization. This internal resource bound is not an execution
timeout or user-configurable concurrency change.

Register work before job submission. Registration and sealing share one mutex;
sealing closes queued slot acquisition and cancels diagnostic observation. The
blocking job owns its lease even if its oneshot receiver disappears. Failed
late delivery drops an owned child carrier, signals its group before reaping,
and retains the lease in cleanup until child.wait finishes. A delivered child
retains the lease through capture/error/cancellation. Normal successful capture
preserves existing descendant/exit semantics.

Manager exit observers also retain a registry lease for already-admitted work;
the existing accepted request/dispatch caps bound these synchronous callers.
Cancelling workspace capture kills its group while that observer still owns the
child wait. Cancelling workspace/effort/final admission releases preparation and
does not manufacture a failure attempt.

Stop seals spawn admission and terminates registered agent groups before waiting
for unresolved diagnostic work. It uses the existing 10 s drain budget, measured
from stop entry, and reports incomplete while entered work remains. Normal and
Drop lifecycle drains then join background loops, requests, owned spawn/cleanup
work and collectors before releasing ownership. Collector drainage matters even
when the bounded engine-stop observer has returned an error. Controlled holder
release permits late-child cleanup and final drainage. No timeout releases an
entered permit or advertises a clean stop with accepted work outstanding.

### Safe telemetry

A process-wide observer thread operates while records are live and exits when
empty. It depends on neither polling workers nor the project boundary. It copies
due records under its own short mutex and emits outside that lock directly to the
captured, registered subscriber, respecting its filters without ambient macro
callsite/max-level caching. Stage changes wake the observer for their first warning. At most 128
individual records and two fixed overflow-stage aggregates are retained. Fields
are generated numeric id, closed site/stage enums and monotonic elapsed time;
no command/path/argv/prompt/env/output or caller identity is accepted.

First warning is at 1 s, then at least every 5 s per persistent record. These
are warning thresholds only. RAII clears records on success/error, queued
cancellation and unwind. The additive internal observation port allows the daemon
testing harness to synchronize at actual diagnostic entry; production entrypoints
install no custom implementation and expose no environment/runtime injector.

## Findings and proof map

| Contract | Implementation / proof |
|---|---|
| C1 | One macOS permit; unchanged three real builders, complete-spawn S controls, Linux no-permit cfg |
| C2; SG-R1 / SG-C1 | Yielding caller admission; final revalidation; two-worker held-diagnostic read/cancel before release, zero attempts and no retained claim/no late executable; queued goal pause no late launch |
| C3 | Registry registration/seal, retained late carrier/manager reaper and both lifecycle drains; registered-group termination before release, incomplete stop, rival owner refused, release/drain then successor owner |
| C4; SG-R2 | Independent real private-log events with stage/id/site/elapsed, 1 s/5 s spacing, waiting cancellation, cap/overflow and error/unwind clearing |
| C5 | This design/plan, original proposal preserved, byte-identical report/Pensieve mirrors, operator docs |
| C6; SG-R3 / SG-C2 | Held diagnostic case and a distinct stop with both registered agent group and probe confirmed live; original A9 retained |
| C7 | Disposable source: containment restored to boundary-held acquisition, real manager/discovery bypass, unknown construction, telemetry emission removed; each must fail its causal assertion, then restore exact bytes |
| C8 | Full changed-crate checks below once on committed source; all failures retained |

C7's control read can fail with the existing admission Overloaded error when its
boundary is held. That is the diagnosed control failure, distinct from executable
absence or a generic harness crash. Negative-control logs and restoration hashes
are part of the builder handoff, not replacement independent review.

## Execution and verification

- [x] Read qualification/authority/full graph and record own safe host entry.
- [x] Preserve original proposal; integrate exact qualified fixture only.
- [x] Revise and mirror C5 before source; implement coupled routes/ownership/docs.
- [x] Develop deterministic admission, telemetry and four daemon scenarios.
- [ ] Complete final causal negatives and byte restoration.
- [ ] Commit explicit paths and run required commands on that exact SHA.
- [ ] Publish one full qualified SHA for the existing three independent readers.

Commands, isolated fake home/global.yml/project/short sockets and private target,
with fail-closed fake AI executables and an outer owned-workload watchdog:

```sh
cargo fmt --all --check
cargo clippy -p orch-core -p orch-infra -p orch-engine -p orch-daemon --all-targets --features orch-daemon/testing -- -D warnings
cargo test -p orch-core -p orch-infra -p orch-engine -p orch-daemon --features orch-daemon/testing
cargo xtask crate-graph
```

Build this source's orch-cli binary first solely for the existing concurrent_index
fixture prerequisite. Complete suites include the original lifecycle/pause/cancel
and ownership vectors; no original deadlines/concurrency/assertions/ignores change.
New deterministic scenarios use two workers and the existing 20 s assertion budget;
their outer cleanup watchdog is 60 s. No real AI CLI or real project daemon runs.

No performance result, SGATE, Linux/Docker/cold/workspace gate, push, release/tag
or version bump is authorized. Prior Linux 938 passed / 4 failed / 4 inherited
ignores, exit101, remains qualified RED without attribution. Native integrated CI,
escalation-2 owner disposition, owner QA and independent exact-SHA readers remain
required. Rollback is a revert of the explicit containment commit; reverting the
fixture would restore a known leak/backpressure defect and requires holding Hang
workloads for lead disposition.
