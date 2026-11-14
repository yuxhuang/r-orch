# Daemon-owned state: one orch process owns `.orchestry/`

status: active
progress: implementation PRs #74 to #81 and documentation/test PR82 are merged (PR82 merge c863494e). Subsequent record, 2026-11-14: the fifth selected check passed on the documentation candidate 984711b0 (five direct macOS commands, 6,974 passed / 0 failed / 7 ignored); original PR82 Linux CI passed (6,944 / 0 / 7), and original merged-main CI passed. The four earlier targeted measurements remain single macOS runs at 760c270b, with session-only identity and representative-storage limits. Twelve proof gaps plus the real-second-user connection remain unproven, owner-waived 2026-11-14; repeated MCP initialize after daemon death remains an accepted known limitation. This document is not `done`: 21 rows remain `in part`, one deferred and 18 pass; the release commit's final facts/report and fact/link review are recorded in its pull request and release record. Dated snapshot, 2026-11-14: the preparation commit 8c82aa4d passed five direct macOS commands once (6,974 passed / 0 failed / 7 ignored), and no other commit inherits that result. The four direct macOS commands omitted at #81 and original main CI43561235069 attempt-1 failure are historical facts, not overwritten by later results
owner: orchestrator (lead)
date: 2026-11-09
spec: [../specs/2026-11-09-daemon-owned-state.md](../specs/2026-11-09-daemon-owned-state.md) (approved by the owner at gate 2, 2026-11-09)

## Goal
Exactly one process per project, the **daemon**, owns `.orchestry/`, every store and every agent process. The CLI,
the dashboard, `orch web`, `orch mcp` and the `orch` commands agents run inside their tasks are clients of it over
a local Unix socket, for reads and writes alike. No client reads or writes `.orchestry/` state itself, no client
ever starts a daemon, and with no daemon every client fails with one uniform message and exit code 7. This closes
the cross-process store races of [ADR 0011](../decisions/0011-mcp-server.md) residual D10 and
[ARCHITECTURE §2.1](../ARCHITECTURE.md#21-daemon-ownership-guarantees-and-limits) as a
class: one writer instead of a patch per race.

## Non-goals
The spec's won't rows W1–W10: auto-start of any kind (owner decision: the owner wants to know when a long-lived
process starts on their machine); a direct-store fallback or offline read lane in any client; SQLite or any change
to the YAML, index, `config.yml`, org-template or run-event formats; Windows; one daemon per user for several
projects; changes to the remote peer protocol; an idle exit, watchdog or automatic restart.

The owner amended the original release exclusion on 2026-11-14: a major release and tag follow the final
documentation and acceptance decisions. The subsequent release is prepared as 1.0.0 from actual PR82 merge `c863494e`, replacing the historical 0.14.0 plan; its publication facts are recorded in its release record. The release cold gate is
not run, skipped by the owner's word of 2026-11-14; one green Linux CI execution per PR remains required.
The owner controls merges, tagging and publishing. See the spec's release amendment; versioning follows the project's
declared SemVer policy, and the package version is not evidence of compatibility or reliability beyond the recorded checks.

## Context
- Base: `main` at `8bab8d1c` (v0.13.0). Owner's request: the goal text, quoted in the spec's Source.
- Today `orch_engine::build_light` / `build_full_with_peer` are called from 19 files in `orch-cli` (every command,
  `mcp/tools/*`, `tui/backend.rs`, `web/mod.rs`, `serve/`; counted with `git grep -l -E 'build_light|build_full_with_peer' b1216ce4 -- crates/orch-cli/src`). The PID lock (`crates/orch-engine/src/lock.rs`)
  serialises orchestrators only; each store has its own `.index.lock`.
- Documented races: entity and index written as two steps; whole-record saves with no compare-and-swap; context
  expiry publishing a keep-list from `list`/`get`, also from `--read-only` MCP sessions; a per-process
  `ensure_dir` cache. Recovery is documented in the runbook ("Rebuilding an index after a lost update",
  "Recovering after concurrent MCP writers") and manual 17 ("Concurrent writers").
- Decision record: [ADR 0012](../decisions/0012-daemon-owned-state.md) (accepted), superseding ADR 0011
  decision 7 and the D10 acceptance once the client conversion is complete. User manual:
  [chapter 18](../manual/18-daemon.md).

## Design
The [spec](../specs/2026-11-09-daemon-owned-state.md) is normative: requirements R1–R39, the uniform failure
contract, socket discovery and project selection, the security checklist, the
[normative recovery table](../specs/2026-11-09-daemon-owned-state.md#normative-recovery-table) and the protocol
and operation catalogue. [ADR 0012](../decisions/0012-daemon-owned-state.md) records the decision and the
rejected alternatives. This section is the summary a builder needs to find the right requirement.

- **Crates.** `orch-proto` (local wire types, version constants, bounded NDJSON framing, discovery record;
  workspace dependency `orch-domain` only); `orch-daemon` (ownership, lifecycle, discovery publication, listener,
  state-operation coordinator, operation handlers, hosted dispatch loop and peer runtime, in-process test harness);
  `orch-client` (discovery read, connect, peer-uid check, handshake, typed operations, uniform NoDaemon error;
  `orch-domain` and `orch-proto` only; no process API). `orch-cli` names `orch_daemon` only in
  `commands/daemon.rs` and `commands/init.rs` (allowlist enforced in PR 4). See
  [ARCHITECTURE §3.1](../ARCHITECTURE.md#31-dependency-direction).
- **Lifecycle (R1, R5, R14, R15, R31, R37).** Only an explicit `orch daemon start` (foreground, or `--detach`)
  starts a daemon; dispatch is OFF unless `--dispatch on`. A stable owner guard plus the existing PID lock pick one
  winner of racing starts; the loser exits 4. Signals and `orch daemon stop` stop dispatch, kill and reconcile
  agents, then release ownership last. Agent context refuses start with exit 2 unless `--from-agent`.
- **Discovery (R2, R33).** `ORCH_SOCKET` > `ORCH_PROJECT_ROOT` > cwd walk; default
  `.orchestry/run/orch.sock` within the 100-byte budget, else `$TMPDIR/orch-<hash16>/orch.sock`;
  `.orchestry/daemon.json` schema 1; project identity is the dev/ino of `.orchestry`.
- **Protocol (R6, R7, R20, R32, R36).** Frozen hello/welcome/reject envelope; strict equality of package semver
  and `LOCAL_PROTOCOL` (1; 2 from PR 3, for the subscription semantics) for every operation; only `hello.intent: "stop"` crosses skew. 1 MiB NDJSON frames,
  typed closed DTOs, separate pre-handshake and session budgets.
- **Engine guarantees (R9–R12).** One coordinator orders complete read/modify/write transitions; a pause decision
  is ordered with spawn; task cancel reaches the live run registry; context expiry is daemon maintenance, reads
  never purge.
- **Dispatch mapping (R13, D8).** Dispatch is a daemon mode, not a process. The `serve` / `run --watch` /
  `run --once` / `serve --once` remap landed in PR 2.
- **Push and dispatch wake (R16–R18; PR 3).** Clients subscribe before they read; a subscription has a cursor
  (origin 0, first event 1) and a baseline (the subscribe call's `commit_seq`). A change without an entity id
  invalidates its whole store; resync (`lagged`, or an uncertain store set) is rare. The dashboard and the web
  dashboard's sessions refresh only from push; `orch mcp` maps store events to at most 256 subscribed `orch://`
  URIs over one observation session. A commit schedules a dispatch wake within a 500 ms scheduling bound,
  and a tick never wakes itself. This is not an execution or task-start guarantee: a slow tick, the operation
  permit or the OS can delay execution. See the spec's
  [Push, gaps and snapshot ordering](../specs/2026-11-09-daemon-owned-state.md#push-gaps-and-snapshot-ordering) and
  [Commit-triggered dispatch wake](../specs/2026-11-09-daemon-owned-state.md#commit-triggered-dispatch-wake).
- **PR 3 known limitations.** Commit metadata (256 entries / 64 KiB), 8 subscriptions/session,
  256 events/subscription and 4 MiB staged output/session are bounded. The engine's per-operation domain-event
  buffer is not capped: it holds one operation's events until release and grows with the number emitted.
  PR 4 must supply a bounded lossless overflow design for in-process listeners or an explicit owner-approved
  supported envelope; this obligation is not deferred beyond the goal. An idle worker's heartbeats emit no changes
  or wakes; an idle controller still publishes Remote status about once per second and may wake dispatch.
- **Rejected** (details in ADR 0012): a direct-store fallback; auto-start; a separate `orchd` binary; gRPC or
  tarpc; a second framing next to `orch-peer`'s; protocol-number-only skew checks; an RPC-only FIFO or a lock per
  save in place of the coordinator.

## Tasks

Four stacked PRs against `main` (spec R29, regrouped after a red-team review found that a split cutover would
ship a daemon client and a direct writer side by side), each green and leaving `main` working; the owner merges each; implementation PRs make no version bump, and the subsequently ordered 1.0.0 release follows them. PR *n*+1 is built once PR *n* is frozen for review and
rebases on its merge. Builders work in separate worktrees with disjoint scopes and integrate by merge, never by
rebase of a shared branch. Owners are roles; task ids live in the tracker, not in this plan.

| PR | Branch | Scope | Reviews and gate |
|---|---|---|---|
| 1 | `feature/daemon-core` | `orch-proto`, `orch-daemon`, `orch-client` control plane, `orch daemon start\|status\|stop\|dispatch`, engine guarantees (B0 + B0d), in-process harness, spec, plan, ADR 0012, ARCHITECTURE §3.1, manual 18, CHANGELOG. **Developmental**: `ORCH_DAEMON_DEVELOPMENTAL=1` plus a banner that legacy writers are not excluded; no client is converted. | code, security, resilience, facts, cold gate + Linux |
| 2 | `feature/daemon-clients` | The single production cutover: every D7 operation in `orch-client`; CLI, MCP bridge, TUI backend over RPC (refresh tick, no disk observer), `orch web` preflight and PTY env; developmental gate removed; D8 command remap; binary-test migration; no-daemon matrix for all five frontends; real-TUI + 4 CLI + 2 MCP stress; README upgrade, CHANGELOG, changed command chapters. Over the 3,000-line guideline by design: splitting it would ship a head with a daemon client and a direct writer side by side. | code, security, resilience, performance (CLI round trip vs v0.13.0), cold gate + Linux |
| 3 | `feature/daemon-push` | Subscriptions (cursor, baseline, store-wide change, resync); TUI and web stop RPC polling; MCP `resources/subscribe` and `resources/updated`; commit-triggered dispatch wake (restores prompt dispatch); foreground start-up diagnostics; delete the unused `disk_observer`. | code, security, resilience, facts, performance (wake latency, slow subscribers), cold gate + Linux; QA pass 1 on the owner's Mac after those reviews |
| 4 | `feature/daemon-enforce` | Crate and source-site allowlist, client infra/engine dependencies removed, MCP FIFO and dead code removed; ARCHITECTURE §2, RUNBOOK, manual 17/18 sweep; spec Verification table; plan and spec `done`. | code, security, resilience, facts, cold gate; QA pass 2 |

### PR 1 (wave B)

| Id | Task (owner) | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| A1, A1b | Design, spec, ADR 0012; rebuttal v2 (architect) | R1–R39 | `docs/specs/2026-11-09-*`, `docs/decisions/0012-*` | — | owner approves the spec at gate 2 | 1 |
| SE, RT | Threat model; red team of the decision set (sec-eng; red-team-guy) | R6, R20, R21, R29, R31 | reader | A1 | lead adjudicates at CP1 | 1 |
| B0, B0c, B0d | Engine guarantees: pause ordered with spawn, cancel stops the live process, explicit context expiry; expiry errors never skip reconciliation (senior-eng-2) | R9–R11 | `crates/orch-engine/src/**`, `crates/orch-infra/src/storage/{context,index}.rs` | — | `cargo test -p orch-engine --test daemon_guarantees`; `cargo test -p orch-engine --lib orchestrator::tests::resilience::expiry_write_failure_does_not_skip_reconciliation -- --exact`; B0 code review at `f44d60b7` | 1 |
| T0 | Failing-first tests for B0; harness design (test-engineer) | R9–R11, R25 | `crates/orch-engine/tests/**` | — | `cargo test -p orch-engine --test daemon_guarantees`; failing-first evidence retained in T0/B0 reports; R9–R11 exact functions in spec | 1 |
| W0 | Plan, manual 18 skeleton, doc stubs (tech-writer) | R22 | `docs/plans/**`, `docs/manual/**` | — | links resolve | 1 |
| S1a-api | `orch-proto` and `orch-daemon` skeleton with public seams; workspace members; crate-graph table (senior-eng) | R6, R7 | `crates/orch-proto/**`, `crates/orch-daemon/**`, `Cargo.toml`, `Cargo.lock`, `xtask/src/crate_graph.rs` | gate 2 | `cargo test -p orch-proto --test daemon_protocol --test envelope`; R6/R7 exact functions in spec; `cargo xtask crate-graph`; `cargo deny check` | 1 |
| S1a-impl | Lifecycle, discovery, listener, handshake, sessions, env snapshot, harness (senior-eng) | R1, R2, R4–R8, R14, R15, R20, R21, R25, R31–R33, R35–R37 | `crates/orch-daemon/src/{lifecycle,server,discovery,testing}/**`, `crates/orch-daemon/src/lib.rs`, `crates/orch-daemon/Cargo.toml`, `crates/orch-proto/**` | S1a-api | `cargo test -p orch-daemon --features testing --lib --test daemon_lifecycle --test daemon_paths --test daemon_security --test daemon_wire`; R1/R2/R6/R8/R14/R25/R36/R37 exact functions in spec | 1 |
| S1b | State-operation coordinator (D2) and the D7 operation handlers (senior-eng-2) | R9–R13, R20, R30, R34 | `crates/orch-daemon/src/{ops/**,coordinator.rs}`, `crates/orch-daemon/tests/ops_*.rs`, `crates/orch-engine/**` | S1a-api, B0d | `cargo test -p orch-daemon --features testing --test daemon_engine --test ops_control --test ops_coordinator`; `cargo test -p orch-engine --test ops_daemon`; includes `orch-daemon/daemon_engine::goal_edit_cannot_resurrect_active` and `orch-daemon/ops_coordinator::mutations_and_reads_observe_one_commit_order` | 1 |
| S1c | `orch-client` control plane; `orch daemon start\|status\|stop\|dispatch` behind the developmental gate; `init` start hint (senior-eng-2) | R3–R5, R8, R14, R29, R31 | `crates/orch-client/**`, `crates/orch-cli/src/commands/{daemon,mod,init}.rs`, `crates/orch-cli/src/lib.rs`, `crates/orch-cli/Cargo.toml`, `crates/orch-cli/tests/daemon_cmd_*.rs` | S1a-api | `cargo test -p orch-client`; `cargo test -p orch-cli --test daemon_cmd_control`; includes `orch-cli/daemon_cmd_control::status_dispatch_and_stop_use_the_explicit_harness_socket` and `protocol_skew_names_both_versions_and_frozen_stop_still_works` | 1 |
| S1d | Hosted dispatch loop and peer runtime in the daemon; lift the NDJSON framing from `orch-peer` (senior-eng) | R1, R13, R19, R30 | `crates/orch-daemon/src/{dispatch,peer}/**`, `crates/orch-peer/src/{codec,lib}.rs`, `crates/orch-proto/**` | S1a-impl | `cargo test -p orch-daemon --features testing --test daemon_dispatch`; `cargo test -p orch-daemon --features testing --lib peer::tests::listen_is_a_start_option_and_speaks_the_unchanged_peer_protocol -- --exact`; `cargo test -p orch-peer`; peer byte fixture `orch-peer/lib::codec::tests::wire_bytes_are_unchanged_by_the_framing_lift` | 1 |
| T1 | Integration vectors against the harness and protocol, failing first (test-engineer) | R2, R6, R7, R12, R20, R25, R32, R33, R36, R37 | `crates/orch-daemon/tests/daemon_*.rs`, `crates/orch-daemon/tests/common/**`, `crates/orch-proto/tests/**` | S1a-api | `cargo test -p orch-daemon --features testing --tests`; `cargo test -p orch-proto --tests`; spec command index and R-row functions identify the exact targets; two-executable R6/R32 acceptance remains UNMET | 1 |
| T1b | Binary lifecycle tests: racing starts, signals, crash then NoDaemon, stale socket, no-spawn proof (test-engineer) | R4, R5, R14, R15, R27, R31, R37 | `crates/orch-cli/tests/daemon_lifecycle*.rs`, `crates/orch-cli/tests/daemon_common/**` | S1a-impl, S1c | `cargo test -p orch-cli --test daemon_lifecycle --test daemon_lifecycle_nospawn --test daemon_paths --test daemon_security`; includes `orch-cli/daemon_lifecycle::two_explicit_starts_one_owner`, `foreground_sighup_stops`, `crash_requires_explicit_start`; ignored empty `two_executable_stale_dev_instance` is not evidence (T2a owns replacement) | 1 |
| W1 | This plan's task table and status; ARCHITECTURE §3.1; manual 18 command reference; manual 17 expiry wording; CHANGELOG `[Unreleased]` (tech-writer) | R22, R29 | `docs/plans/2026-11-09-daemon-owned-state.md`, `docs/ARCHITECTURE.md`, `docs/manual/{17-mcp-server,18-daemon}.md`, `CHANGELOG.md` | gate 2 | links resolve; every documented command, flag, exit code and path matches the spec and D8 | 1 |
| I1 | Integrate PR 1 into `feature/daemon-core`; full CI set; freeze the commit (senior-eng) | R24, R29 | `crates/**`, `xtask/**`, `Cargo.toml`, `Cargo.lock`, `docs/**`, `CHANGELOG.md` | S1b, S1c, S1d, T1, T1b, W1 | the CI set; cold `./xtask/gate.sh` | 1 |
| G1 | Board 1 at the frozen commit: code (code-reviewer), security (sec-reviewer-codex), resilience (sre), facts (fact-checker-2), cold gate and Linux (release-eng) | R20, R21, R22, R24 | reader | I1 | reviewer verdicts | 1 |
| CP3 | Adjudicate board 1, fixes and re-review, push PR 1 (lead; release-eng pushes) | — | per finding | G1 | PR 1 open, CI green | 1 |

### PR 2 (wave C), PR 3 (wave D), PR 4 (wave E)

**All targets and commands in this PR 2–4 table are planned acceptance, not PR 1 coverage.** Existing suite names will gain cutover assertions. Task ids are created when the previous PR freezes; scopes are disjoint within a wave.

| Id | Task (owner) | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| S2a | Every D7 operation in `orch-client`; convert every CLI command; `init`; D8 command remap; remove the developmental gate (senior-eng); R39 is deferred by the owner and not built | R3–R5, R13, R29, R31, R38 | `crates/orch-client/**`, `crates/orch-cli/src/commands/**`, `context.rs`, `lib.rs`, `crates/orch-daemon/src/session.rs`, `crates/orch-daemon/src/ops/control.rs` | PR 1 frozen | `cargo test -p orch-client -p orch-cli` | 2 |
| S2b | `orch mcp` as a stdio-to-socket bridge; NoDaemon as `initialize` and tool error (senior-eng-2) | R17, R18 | `crates/orch-cli/src/mcp/**` | S2a client API | `cargo test -p orch-cli --test mcp_stdio --test mcp_contract --test mcp_tools` | 2 |
| S2c | TUI backend over RPC (refresh tick, no disk observer); `orch web` preflight, 503 and PTY env (frontend-eng) | R16, R18, R26 | `crates/orch-cli/src/{tui,web}/**` | S2a client API | `cargo test -p orch-cli`; tmux end-to-end tests | 2 |
| T2a | No-daemon matrix, lifecycle, two-executable skew (replace ignored T1b placeholder), stress with the real `orch tui` in tmux + 4 CLI + 2 MCP clients (test-engineer); no R39 vectors, R39 being deferred by the owner | R4, R6, R12, R26, R27, R32, R35 | `crates/orch-cli/tests/daemon_*.rs` | S2a | `cargo test -p orch-cli --test daemon_no_daemon --test daemon_lifecycle --test daemon_stress`; build two binaries with different LOCAL_PROTOCOL constants, same package semver, then run the non-ignored `daemon_lifecycle::two_executable_stale_dev_instance` in both directions (old executable removed in one case); build/run fixture must be committed by T2a | 2 |
| T2b | Move binary tests to an explicit test daemon on an isolated `ORCH_SOCKET` (test-eng-2) | R25 | `crates/orch-cli/tests/common/**`, `crates/orch-cli/tests/mcp_common/**` | S2a | `cargo test -p orch-cli --tests` | 2 |
| W2 | README "Upgrading from v0.13.0"; CHANGELOG; every changed command chapter; command reference; remove manual 18's developmental notice (tech-writer) | R22, R35 | `README.md`, `CHANGELOG.md`, `docs/manual/**` | S2a | links resolve; `docs/manual/tools/gen-command-reference.sh` output committed | 2 |
| I2, G2, CP4 | Integrate, board 2 with CLI round-trip latency, adjudicate, PR 2 | R24 | per finding | S2b, S2c, T2a, T2b, W2 | the CI set; cold gate; latency report | 2 |
| S3-api | Freeze the subscription API in `orch-proto` and `orch-client` (cursor origin 0, baseline from `commit_seq`, optional change id, second resync reason, `LOCAL_PROTOCOL` 2); one buildable API-first SHA that every PR 3 consumer merges (senior-eng) | R16, R20, R32 | `crates/orch-proto/{src,tests}/**`, `crates/orch-client/{src,tests}/**` | PR 2 frozen | `cargo test -p orch-proto -p orch-client`; `cargo xtask crate-graph` | 3 |
| IGN1 | Print start-up orchestrator errors once on the foreground daemon's stderr; un-ignore the two opencode legacy-ignore tests (senior-eng-2) | R20 | `crates/orch-daemon/src/**`, `crates/orch-cli/src/commands/daemon.rs`, `crates/orch-cli/tests/token_eco_opencode_wire_gate.rs` | PR 2 frozen | `cargo test -p orch-cli --test token_eco_opencode_wire_gate` | 3 |
| S3c-1 | Subscription hub and typed commit metadata: register before read, ack before events, filters, store-wide changes, bounded queues, resync or close (senior-eng) | R16, R20 | `crates/orch-daemon/src/**`, `crates/orch-engine/src/**` | S3-api, IGN1 | `cargo test -p orch-daemon --features testing`; `cargo test -p orch-engine` | 3 |
| S3c-2 | Commit-triggered dispatch wake: 500 ms scheduling bound, coalescing, no self-wake, OFF/pause/back-off rechecked (senior-eng) | R9, R13 | as S3c-1 | S3c-1 | `cargo test -p orch-daemon --features testing dispatch` | 3 |
| S3a | Dashboard consumes push: subscribe before bootstrap, no periodic state read; delete `disk_observer` (frontend-eng) | R16, R18 | `crates/orch-cli/src/tui/**`, `crates/orch-tui/{src,tests}/**`, `crates/orch-cli/src/serve/{mod,disk_observer}.rs` | S3-api | `cargo test -p orch-tui`; `cargo test -p orch-cli --lib tui::` | 3 |
| S3b | `orch web` parent holds an observation and liveness connection; sessions keep the PTY dashboard (frontend-eng-2) | R16, R18 | `crates/orch-cli/src/web/**` | S3-api | `cargo test -p orch-cli --lib web::`; `cargo test -p orch-cli --test web_bridge` | 3 |
| S3m | `orch mcp` resource subscriptions: 256 URIs, one observation session, local URI mapping, coalesced `resources/updated` (senior-eng-2) | R17, R18 | `crates/orch-cli/src/mcp/**` | S3-api | `cargo test -p orch-cli --lib mcp::`; `cargo test -p orch-cli --test mcp_contract --test mcp_tools --test mcp_stdio` | 3 |
| PR 3 scoped repairs | Writer activates after acknowledgement flush; dirty prompt-mode view rearms on asynchronous success/refusal; MCP owns one scheduled recovery, bounds retained URI strings, cancels/joins tasks and bounds output even at EOF | R16–R18, R20 | `crates/orch-daemon/src/{session,hub}.rs`, `crates/orch-tui/{src/app.rs,tests/push_refresh.rs}`, `crates/orch-cli/src/mcp/server{.rs,/subscriptions.rs}` | existing push API | Scoped exact-SHA reviews are qualified; integration must retain the causal controls and obtain independent code/security/resilience delta reviews at the new full SHA | 3 |
| T3 | Independent push and wake acceptance tests, authored against the API and finished on the integrated candidate (test-engineer) | R16–R18, R9, R13 | `crates/orch-daemon/tests/{daemon_push,daemon_dispatch_wake}.rs`, `crates/orch-cli/tests/{daemon_push,tui_daemon,web_daemon}.rs`, `crates/orch-cli/tests/push_support/**` | S3-api | the spec's [PR 3 acceptance targets](../specs/2026-11-09-daemon-owned-state.md#pr-3-acceptance-targets) | 3 |
| T3-port | Replace the ignored PR 2 cases (`yaml_bomb` health assertion, three registry tests) with active daemon-owned equivalents (test-eng-2) | R15, R19, R20 | `crates/orch-cli/tests/{yaml_bomb,remote_peers}.rs`, `crates/orch-daemon/tests/registry_store.rs` | PR 2 frozen | `cargo test -p orch-cli --test yaml_bomb --test remote_peers`; `cargo test -p orch-daemon --features testing --test registry_store` | 3 |
| W3 | Spec wording, this plan, ADR 0012 clarification note, manual 02/07/08/09/12/15/17/18, RUNBOOK, CHANGELOG; drafted ahead of the code, verified on the integrated candidate (tech-writer) | R16–R18, R22, R23, R29 | `README.md`, `CHANGELOG.md`, `docs/**` | PR 2 frozen | `git diff --check`; examples re-run on an isolated integrated binary | 3 |
| I3a, I3b | Assemble the candidate on `feature/daemon-push` by merge; then merge final tests and docs, gate and freeze (senior-eng-2) | R24, R29 | per merge | all PR 3 slices | the CI set; cold `./xtask/gate.sh`; Linux | 3 |
| G3, QA1 | Board 3 at the frozen SHA: gate (release-eng), code (code-reviewer), security (sec-reviewer-codex), resilience (sre), facts (fact-checker), performance (perf-eng); then QA pass 1 on the owner's Mac (qa-engineer) | R24, R28 | per finding | I3b | reviewer verdicts; QA transcript | 3 |
| S4 | Crate and source-site allowlist; remove client infra/engine dependencies, the MCP FIFO lane and dead cross-process code (senior-eng-2; release-eng co-signs the manifest) | R3 | `xtask/**`, `crates/orch-cli/src/{serve,mcp}/**`, `crates/orch-cli/Cargo.toml`, `crates/orch-daemon/src/session.rs`, `crates/orch-daemon/src/ops/control.rs`, `crates/orch-daemon/tests/daemon_security.rs` | PR 3 frozen | `cargo xtask crate-graph` passes, and fails on a planted `orch_infra` import in a client file | 4 |
| PR 4 retained hardening | Resolve uncapped per-operation events; adjudicate no-op peer commit/fan-out work; make the daemon harness hermetic by default; trace and independently measure no-op and collector `record_event` held-permit p50/p95 | R20, R21, R25 | scopes fixed at the qualified PR 4 base | PR 3 integration and explicit PR 4 handoff | Bounded lossless event-buffer design or owner-approved envelope; unchanged peer interoperability and causal controls; exact-base reviews and both held-permit distributions | 4 |
| Scope-overlap event and lost-ownership notice | `orch serve --once` reports a scope hold when it is first observed and when its blocker changes, then at most once per five minutes while dispatch evaluates the task, and names a real blocker; the daemon writes one fixed, best-effort line to its standard error when it finds its ownership lost. The owner chose both | R40 | `crates/orch-engine/**`, `crates/orch-daemon/**`, `crates/orch-cli/**` | PR 4 retained hardening | **Historical implementation, now merged in #81.** Current subsequent evidence is recorded in the Status table and the spec's Verification table at the final commit | 4 |
| W4 | ARCHITECTURE §2; RUNBOOK; manual 17/18 sweep with captured output; spec Verification table; spec and plan `done`; Pensieve mirror (tech-writer) | R22, R35, R38 | `docs/**`, `README.md`, `CHANGELOG.md` | S4 | every example re-run at the frozen commit; fact-check | 4 |
| I4, G4, QA2 | Integrate, board 4, QA pass 2 | R22, R24, R28 | per finding | S4, W4 | reviewer verdicts; QA transcript | 4 |

W4 above remains a task plan, not proof that its done-condition is met: this preparation does not rerun every
example, finish the spec/plan, or synchronize mirrors. The final release fact/link review and release facts/report
are recorded with the release commit, not here.

### Bounded synthetic spawn-admission repair

This subsection preserves its historical checkpoint record, including the holds then pending. See the current
Status table below for subsequent integration, CI and owner dispositions; the earlier holds are not current claims.

Build a new repair branch from `e5b46febad614baa645e2bdeee297d7cd306e464`;
first merge final PR 2 head `c49d47830091a812613dd86764343a58d1dad7bb`,
which includes the required ETXTBSY base
`33ebc8839fbb6ff439b214b8af74f6ce9c0b038b`. Preserve all existing public
signatures, Process defaults and real macOS complete-spawn exclusion. No new
features, dependencies, protocol, config or on-disk changes.

| Step | Work | Prerequisite | Result / evidence |
|---|---|---|---|
| T1 | Demonstrate the deterministic foreign pre-body reservation regression at exact baseline, with Collector/Retry/Pause cause records and post-release controls | adopted design and Linux falsifier | Done. Red at the baseline (`7eb7933f`, the regression on top of the pre-repair candidate): `daemon_spawn_gate_isolation` fails with "fake daemon completed its wake while the foreign pre-spawn reservation remained held". Landed as `b83dc12`. |
| B | Explicit per-harness synthetic domain, provenance rejection, additive plumbing through launch/workspace/OpenCode; document before code | required baseline red; final PR 2 merge | Done: spec and plan `9e3f9cf`, code `6cf17f0`. |
| T2 | Bind the two fake fixtures; add domain lifecycle/route controls, fail-closed negatives and exact opt-in source allow-list | B interfaces | Done: `fbe9e75`, `3dd0831`. The record-ID controls were then made independent of the 128-record observation registry: `a46b015`. |
| Q | Freeze SHA/tree and lineage; regression green/revert red/restored green; original ten-vector wake target N=30, default threading, first red stands; exclusion negative and standard checks | T2 | Passed at `3dd0831e`, macOS: regression green at the repair, red with the repair reverted, green again once restored; real-spawn exclusion negative red when bypassed; `daemon_dispatch_wake` 30 of 30 consecutive default-threaded invocations `10 passed; 0 failed`; `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` (6427 passed / 0 failed / 7 ignored), `cargo xtask crate-graph` and `cargo deny check` green. N=30 is acceptance at the stated N, **not a reliability rate**. |
| QL | Native Linux regression companion, original wake and infra controls, one invocation each | Q; quiet host | Passed at `3dd0831e`: companion 1/0, contract 1/0, `daemon_dispatch_wake` 10/0, `cargo test -p orch-infra` 953 passed / 0 failed / 4 ignored. One local Docker sequence (aarch64), **not Linux CI**. |
| Reviews / checkpoint | Independent exact-SHA code, resilience and security review, then lead integration decision | QL | Done at `3dd0831e`: security pass, resilience pass, code pass with conditions. Test-only condition fixed at `a46b015f` and re-reviewed pass (20 of 20 runs of `cargo test -p orch-infra --lib process::gate::`, 27 tests each). The repair is accepted at `a46b015f`; the Q and QL results carry to it because it changes only a test module that is empty on Linux. The second condition, the harness route list, is documentation only and is recorded in the spec (R25) and ARCHITECTURE. |

The unchanged-candidate Linux falsifier is done: one invocation, green 10/0,
platform support only. It is not repair acceptance. Only Q and QL ran the original
wake oracle against the repair; no serialization, deadline change, drain change or
rerun to green was allowed.

Still pending, and not claimed: the integration candidate's macOS cold gate, Linux
CI and owner QA. The existing integration, CI, owner QA and reserved-consumer
holds remain.

## Status

Status as of local 1.0.0 release preparation after actual PR82 merge (2026-11-14). The spec's [Verification](../specs/2026-11-09-daemon-owned-state.md#verification)
table gives the evidence per row and what is not proven.

| Item | State |
|---|---|
| Gate 1 (plan and design questions) | approved by the owner, 2026-11-09 |
| Gate 2 (spec) | spec approved, ADR 0012 accepted, 2026-11-09 |
| PR 1, `feature/daemon-core` | merged: [#74](https://github.com/yuxhuang/r-orch/pull/74), merge commit `d1951d29` |
| PR 2, `feature/daemon-clients-i2d` | merged: [#75](https://github.com/yuxhuang/r-orch/pull/75), merge commit `d3c5e3f1` |
| PR 3, `feature/daemon-push-i3e` | merged: [#76](https://github.com/yuxhuang/r-orch/pull/76), merge commit `47b3c5d9` |
| PR 4, `feature/daemon-enforce` | merged in three pull requests: [#77](https://github.com/yuxhuang/r-orch/pull/77) (4a, `eae4f13d`), [#79](https://github.com/yuxhuang/r-orch/pull/79) (a test repair, `ab538dec`) and [#78](https://github.com/yuxhuang/r-orch/pull/78) (4b, `12b3bd28`) |
| Documentation as built, and one registry-lock test repair | merged: [#80](https://github.com/yuxhuang/r-orch/pull/80), merge commit `2b91c674`, head `d002013f`; the cold gate passed on macOS at that head (9 of 9 steps, 6,935 tests) |
| Scope-overlap event and lost-ownership notice (R40) | [#81](https://github.com/yuxhuang/r-orch/pull/81) merged at head `bed2b433`, merge commit `fc8d8bf8`: Linux CI run 43552080077 green three times; the push run on `main` at `fc8d8bf8` (run 43561235069, same tree) failed one R40 test (`orch-cli/daemon_ownership_notice::full_stderr_two_workers_status_request_and_stop`), and that failed run remains recorded; the corrected test subsequently passed original PR82 Linux CI43597261918 attempt 1 (notice 15/0/0); the 60-second stress run and an automated end-to-end run passed on macOS; the cold gate was not run, by the owner's decision of 2026-11-13 |
| Documentation/test PR82 | merged: [#82](https://github.com/yuxhuang/r-orch/pull/82); head `984711b003e474c8e059d4150f811ee0c5dc2d0f`, API base and actual merge first parent `fc8d8bf8189a4c157b783d24304ca7791cd94978`, actual merge `c863494e25dc3a3b898eda18d8c19b5319552b59`, tree `8ccd3decd521d672c5cee94d2095a8cc34d177aa`. Original PR CI43597261918 attempt 1 used synthetic `8020cbe3bff04595f21d922ee04ba7b705f4520c`, parents equal to actual merge, tree equal to head; both jobs passed. Original [main CI43604091956](https://github.com/yuxhuang/r-orch/actions/runs/43604091956), push at actual merge, attempt 1, both jobs passed. |
| 1.0.0 release | local metadata/documentation preparation from `c863494e`, a separate commit. Dated snapshot, 2026-11-14, preparation commit `8c82aa4d9f14b63e117a269d9f88512f831205e9`: the five direct macOS commands passed once (6,974/0/7, notice 15/0/0); no other commit inherits that result. The release commit's checks, original Linux CI, independent final fact/link review, release facts/report and publication are recorded in its pull request, CI runs and release record. No code/test/protocol/storage change. Release cold gate **not run, skipped by the owner's word of 2026-11-14**, not passed; R3 accepted-not-cleared, R20 waiver and R39 deferral retained. |
| Verification table, status lines, gate and CI statement | written in a change that follows #81; `in part` rows remain, so the spec and this plan are **not** `done` |
| Owner disposition of the remaining unproven parts, 2026-11-14 | the owner answered "use your mix": five selected parts checked on the documentation candidate, with the release commit's own checks recorded in its pull request, twelve waived and a real second-user connection waived as environment-limited (all unproven, none passed, listed in the spec under "Owner dispositions of 2026-11-14 and what stays unproven" and in the CHANGELOG), and one known limitation accepted for a later fix (an MCP bridge answers a repeated `initialize` after the daemon died). Merging, tagging and publishing stay with the owner |
| Targeted checks run for that disposition (R5, R8, R10, R23) | each run once on macOS at `760c270b`, with no defect found: dispatch on at the start of `serve` and `run --watch`; cancel of a live agent through the MCP bridge and the dashboard; a connected session's identity and mode (session only); a v0.13.0 project read and written both ways (representative, with listed exclusions and differences). Single measurements, not permanent tests, none on Linux. R5 and R10 are now `pass`; R8 and R23 stay `in part`. Details are in the spec's rows |
| The four direct macOS commands (R24) | Subsequent result, 2026-11-14: at documentation candidate `984711b0`, fmt, clippy, crate-graph and workspace tests plus installed cargo-deny passed once on macOS (6,974/0/7, notice 15/0/0). Original PR82 Linux CI43597261918 attempt 1 passed (6,944/0/7, notice 15/0/0). At the 2026-11-14 preparation commit `8c82aa4d` the five direct macOS commands also passed once (6,974/0/7, notice 15/0/0); no other commit inherits that result. The release commit's own checks are recorded in its pull request. The four direct macOS commands omitted at #81 and the original main failure remain recorded. |
| Test repair for the R40 notice test | the owner ordered the failing test fixed in the same pull request as the documentation: one test-only commit (`9ca23c3e`, `crates/orch-cli/tests/daemon_ownership_notice.rs`, no product change) with historical local macOS evidence; subsequent original PR82 Linux CI43597261918 attempt 1 passed the notice target 15/0/0 at `984711b0` (same tested tree as actual merge `c863494e`), and the red run on `main` stays recorded as red |
| QA on the owner's Mac (R28, R35) | replaced by the owner, on 2026-11-13, by an automated end-to-end run in tmux; see R28 |

### Owner-amended performance acceptance and storage follow-up (2026-11-10)

PR 3 performance is accepted **7/7 under the amended no-regression burst criterion** at
`9277f832281d1c9734ce9ef03e8096389ae4ddc9`, comparator
`b97fc59a5b1f32ec3f8d3fb1685a5cccb9bf9533`. Raw results remain 38/100 acknowledgements within 500 ms,
100 eventually, last at 1318.192250 ms; comparator diagnostics reached 39–40/100. The owner accepts the spread
as run-to-run noise. The original fixed-throughput failure remains unchanged: no remeasurement, statistical
equivalence, faster storage or successful 100-in-500-ms/coalescing test is claimed. See the
[spec's full acceptance qualification](../specs/2026-11-09-daemon-owned-state.md#pr-3-performance-acceptance-owner-amended-2026-11-10).

Keep the other six contracts: launch 30 per side, continuous 60-second traffic, paired fan-out intervals
containing zero, qualified RSS occupancy, live peer heartbeat and collector delivery/reconstruction (including
its original failure and qualified offline reconstruction). Only no-op and `record_event` held-permit p50/p95
are deferred to PR 4. The qualified containment source `67ee5017eee36af8dc665ecb9c00c92a51b7a51a` is a
separate lineage; measurement and scoped repair acceptance do not certify integration or shipping.

Defer the 100-successful-commits-in-500-ms storage objective to separate work **after this goal**. It is not
a PR 3, PR 4 or push gate. That work must first design amortized/batched persistence and specify durability,
error visibility, ordering, publication and crash recovery; attribution supports inherited sequential
persistence latency but does not establish a safe minimal repair. No early acknowledgements, dropped syncs,
changed formats or widened measurement window are authorized. The PR 4 hardening row remains required.

The integration lead must preserve the PR 2 board/draft handoff, qualify the actual production and test deltas,
integrate and re-freeze PR 3, and obtain independent exact-SHA delta reviews before releasing its direct gate.
QA follows that qualified candidate. Draft shipping requires three complete green GitHub CI executions on the
identified head/tree and the other applicable gates; the lead reports readiness and the owner merges. (Owner amendment, 2026-11-13: one green Linux CI execution per pull request is enough.)

## Test plan
- The spec's [requirements table](../specs/2026-11-09-daemon-owned-state.md#requirements) names the acceptance
  check and platform of every R-row; its [Verification](../specs/2026-11-09-daemon-owned-state.md#verification)
  table is filled at PR 4 with commit and platform. Blank is not pass.
- PR 1: exact package/target/function pointers and runnable commands are in the spec's
  [acceptance command index](../specs/2026-11-09-daemon-owned-state.md#pr-1-acceptance-command-index).
  Protocol: `orch-proto/daemon_protocol` + `envelope`; wire: `orch-daemon/daemon_wire`;
  lifecycle/path/security: both crate-specific targets listed there; engine: `orch-engine/daemon_guarantees`
  + `ops_daemon`, `orch-daemon/daemon_engine` + `ops_control` + `ops_coordinator`.
  **Every standalone orch-daemon run uses `--features testing`** (including `--tests` and `--lib`).
  `cargo test -p orch-peer` remains required. The ignored empty two-executable placeholder does not count.
- First fix round, verified when the fix branches are integrated:
  `crates/orch-daemon/tests/daemon_peer_boundary.rs`,
  `daemon_endpoint_safety.rs`, `staging_cleanup.rs`; `crates/orch-cli/tests/daemon_stop_wait.rs`;
  `crates/orch-engine/tests/daemon_startup_index.rs`, `daemon_recovery_errors.rs`. The spec has one
  runnable command per target. That integration verifies the integrated function names/counts, the extensions
  to existing security/lifecycle/path targets, diagnostic env/stderr and client unknown-outcome regressions,
  and repeats the peer suites 3×. These fixes are not claimed present or passing at `b1216ce4`.
- PR 2 planned: `daemon_no_daemon::all_frontends_matrix` with process-table and spawn assertions;
  `daemon_stress::real_tui_cli_mcp_share_state_order` (60 s acceptance run); two compiled executables with
  different `LOCAL_PROTOCOL` constants under T2a (R6/R32). The pgid-derived acknowledgement (R39) is deferred
  by the owner and not built.
  The stress test is the evidence that D10 is closed and replaces the runbook hazard.
- PR 3 planned: the spec's [PR 3 acceptance targets](../specs/2026-11-09-daemon-owned-state.md#pr-3-acceptance-targets)
  (`daemon_push::*`, `daemon_dispatch_wake::*`, `tui_daemon::*`, `web_daemon::*`, `daemon_push::mcp_resource_updates`),
  each shown failing once against a deliberate break; QA pass 1 on the owner's Mac (R28) after the PR 3 reviews.
- Hermeticity: no test touches a real project's daemon or `~/.orchestry`; sockets live under a per-test temp
  directory on a path of at most 100 bytes; fake shell agents only.
- Owner-amended macOS gate: run `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask crate-graph` and
  `cargo test --workspace --no-fail-fast` directly, with tmux on PATH and plain command timeouts.
  Process snapshots are observations, not vetoes; custom test runners are retired from these gates.
  Linux coverage comes from GitHub CI after a qualified draft push; no local Linux/Docker prerequisite.
  CI retains its lockfile and cargo-deny checks. Each identified head/tree needs three complete green CI
  executions; this supersedes the older cold-gate wording in the historical task tables above. Owner amendment,
  2026-11-13: one green Linux CI execution per pull request is enough, and the cold gate was skipped for #81's code;
  see the spec's Verification.

## Rollback / compatibility
No on-disk format changes (R23): entity, index, `config.yml`, org-template and run-event files keep their format.
The daemon adds runtime files only: `.orchestry/run/` (or the `$TMPDIR` fallback directory),
`.orchestry/daemon.json`, `.orchestry/daemon.log` and its one rotation file, and the stable
`.orchestry/.daemon-owner.lock` guard. Remote peers keep the same protocol, so 0.5.x peers still interoperate.

- **PR 1** changes no client: with `ORCH_DAEMON_DEVELOPMENTAL` unset nothing new runs. A developmental daemon
  is not excluded from legacy writers (the start banner says so); use it on test projects only.
- **From PR 2**, no `orch` command but `init`, `--help`, `--version` and the explicit starts works without a
  running daemon.
- **To back out** (design D12): the owner stops the current daemon (`orch daemon stop`), checks that its agent
  processes were reconciled, then installs the previous complete binary. Existing files stay readable, and the old
  multi-writer hazards return with it. Do not run old and new writers together. There is no transparent
  downgrade path and no background process replacement.

## Docs to update
Spec; ADR 0012 and the ADR index; ARCHITECTURE §3.1 (PR 1) and §2 (PR 4); RUNBOOK (PR 4: daemon lifecycle, no
daemon, stale socket, crash recovery, version skew; the index-rebuild and concurrent-MCP-writers procedures
removed or marked legacy); manual chapter 18 (PR 1, swept in PR 2 and PR 4), chapter 17 (PR 1 expiry wording,
PR 2 bridge, PR 4 hazard sections), every changed command chapter and the command reference (PR 2),
troubleshooting; README "Upgrading from v0.13.0" (PR 2); CHANGELOG `[Unreleased]` (every PR). Spec and plan are
mirrored to Pensieve's `specs/` directory.

## Open questions
Gate 2's Q2 stop intent and Q5 regrouping were approved on 2026-11-09. Recorded dispositions:

1. **R39 is deferred by the owner (2026-11-11) and not built in this goal.** The kernel-pgid-derived acknowledgement
   for agent-originated stop/dispatch off does not exist: an agent-context `orch daemon stop` or `orch daemon
   dispatch off` succeeds without `--from-agent`. Reasons: the owner's, that an agent with a shell can always run
   the command, so an acknowledgement cannot stop it; and the design review's finding that, with the
   evidence the design was permitted to use about a caller (sampled process-group ids), no design refuses an
   agent's accidental stop without sometimes refusing the owner's own: when a run's process group ends and its
   number is reused for the owner's shell, a stop sent by an older `orch`, whose frozen stop hello cannot carry
   an acknowledgement, would be refused, and R6 and R14 require that stop to work. The finding is limited to
   that evidence; it is not a claim about every possible design. The accepted consequence is that the environment-based refusal of an agent-context
   start is the only protection: accident prevention, not a barrier against a same-uid process. Existing
   kernel-UID/project checks remain mandatory. R39 is a should row, recorded as deferred, not as a pass. The
   decision is ADR 0012, decision 15.
2. **R6/R32 two-executable skew acceptance is UNMET at PR 1.** T1b's
   `orch-cli/daemon_lifecycle::two_executable_stale_dev_instance` is ignored and has an empty body; it does
   not supply acceptance even if run with `--ignored`. Harness-advertised versions cover wire behavior only.
   T2a must build two real binaries with different `LOCAL_PROTOCOL` constants and the same package semver,
   prove ordinary calls fail before mutation and frozen stop completes in both directions, and remove the
   old executable in one case. Keep the existing binary-semver mismatch vectors. The PR 1 body must state
   this unmet acceptance explicitly; no owner waiver is implied. Same semver/protocol with a different build
   remains an intentional detection limit.

The integration of the fix branches and the review that follows it must still verify the first fix round before PR 1 can advance; these dispositions do not certify it.
