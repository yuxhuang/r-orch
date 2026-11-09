# Daemon-owned state: one orch process owns `.orchestry/`

status: active
owner: orchestrator (lead)
date: 2026-11-09
spec: [../specs/2026-11-09-daemon-owned-state.md](../specs/2026-11-09-daemon-owned-state.md) (approved by the owner at gate 2, 2026-11-09)

## Goal
Exactly one process per project, the **daemon**, owns `.orchestry/`, every store and every agent process. The CLI,
the dashboard, `orch web`, `orch mcp` and the `orch` commands agents run inside their tasks are clients of it over
a local Unix socket, for reads and writes alike. No client reads or writes `.orchestry/` state itself, no client
ever starts a daemon, and with no daemon every client fails with one uniform message and exit code 7. This closes
the cross-process store races of [ADR 0011](../decisions/0011-mcp-server.md) residual D10 and
[ARCHITECTURE §2.1](../ARCHITECTURE.md#21-a2-in-detail--the-concurrency-story-is-weaker-than-single-writer) as a
class: one writer instead of a patch per race.

## Non-goals
The spec's won't rows W1–W10: auto-start of any kind (owner decision: the owner wants to know when a long-lived
process starts on their machine); a direct-store fallback or offline read lane in any client; SQLite or any change
to the YAML, index, `config.yml`, org-template or run-event formats; Windows; one daemon per user for several
projects; changes to the remote peer protocol; an idle exit, watchdog or automatic restart; a release, tag or
version bump (0.14.0 is separate).

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
  and `LOCAL_PROTOCOL` (1) for every operation; only `hello.intent: "stop"` crosses skew. 1 MiB NDJSON frames,
  typed closed DTOs, separate pre-handshake and session budgets.
- **Engine guarantees (R9–R12).** One coordinator orders complete read/modify/write transitions; a pause decision
  is ordered with spawn; task cancel reaches the live run registry; context expiry is daemon maintenance, reads
  never purge.
- **Dispatch mapping (R13, D8).** Dispatch is a daemon mode, not a process. The `serve` / `run --watch` /
  `run --once` / `serve --once` remap is PR 2; at PR 1 head they keep their v0.13.0 behaviour.
- **Rejected** (details in ADR 0012): a direct-store fallback; auto-start; a separate `orchd` binary; gRPC or
  tarpc; a second framing next to `orch-peer`'s; protocol-number-only skew checks; an RPC-only FIFO or a lock per
  save in place of the coordinator.

## Tasks

Four stacked PRs against `main` (spec R29, regrouped at CP1 after red-team finding RT1), each green and leaving
`main` working; the owner merges each, no version bump. PR *n*+1 is built once PR *n* is frozen for review and
rebases on its merge. Builders work in separate worktrees with disjoint scopes and integrate by merge, never by
rebase of a shared branch. Owners are roles; the orch task ids are in [Status](#status).

| PR | Branch | Scope | Board (gate) |
|---|---|---|---|
| 1 | `feature/daemon-core` | `orch-proto`, `orch-daemon`, `orch-client` control plane, `orch daemon start\|status\|stop\|dispatch`, engine guarantees (B0 + B0d), in-process harness, spec, plan, ADR 0012, ARCHITECTURE §3.1, manual 18, CHANGELOG. **Developmental**: `ORCH_DAEMON_DEVELOPMENTAL=1` plus a banner that legacy writers are not excluded; no client is converted. | code, security, resilience, facts, cold gate + Linux |
| 2 | `feature/daemon-clients` | The single production cutover: every D7 operation in `orch-client`; CLI, MCP bridge, TUI backend over RPC (refresh tick, no disk observer), `orch web` preflight and PTY env; developmental gate removed; D8 command remap; binary-test migration; no-daemon matrix for all five frontends; real-TUI + 4 CLI + 2 MCP stress; README upgrade, CHANGELOG, changed command chapters. Over the 3,000-line guideline by design: splitting it would ship a head with a daemon client and a direct writer side by side (RT1). | code, security, resilience, performance (CLI round trip vs v0.13.0), cold gate + Linux |
| 3 | `feature/daemon-push` | Subscriptions; TUI and web stop RPC polling; MCP `resources/updated`; delete the unused `disk_observer`. | code, security, resilience, cold gate; QA pass 1 on the owner's Mac |
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
| S2a | Every D7 operation in `orch-client`; convert every CLI command; `init`; D8 command remap; remove the developmental gate; R39 pgid-derived stop/off acknowledgement hardening (senior-eng) | R3–R5, R13, R29, R31, R38, R39 | `crates/orch-client/**`, `crates/orch-cli/src/commands/**`, `context.rs`, `lib.rs`, `crates/orch-daemon/src/session.rs`, `crates/orch-daemon/src/ops/control.rs` | PR 1 frozen | `cargo test -p orch-client -p orch-cli`; planned `cargo test -p orch-daemon --features testing --test daemon_security agent_control_requires_explicit_override -- --exact` (R39) | 2 |
| S2b | `orch mcp` as a stdio-to-socket bridge; NoDaemon as `initialize` and tool error (senior-eng-2) | R17, R18 | `crates/orch-cli/src/mcp/**` | S2a client API | `cargo test -p orch-cli --test mcp_stdio --test mcp_contract --test mcp_tools` | 2 |
| S2c | TUI backend over RPC (refresh tick, no disk observer); `orch web` preflight, 503 and PTY env (frontend-eng) | R16, R18, R26 | `crates/orch-cli/src/{tui,web}/**` | S2a client API | `cargo test -p orch-cli`; tmux end-to-end tests | 2 |
| T2a | No-daemon matrix, lifecycle, two-executable skew (replace ignored T1b placeholder), R39 hardening vectors, stress with the real `orch tui` in tmux + 4 CLI + 2 MCP clients (test-engineer) | R4, R6, R12, R26, R27, R32, R35, R39 | `crates/orch-cli/tests/daemon_*.rs`, `crates/orch-daemon/tests/daemon_security.rs` | S2a | `cargo test -p orch-cli --test daemon_no_daemon --test daemon_lifecycle --test daemon_stress`; build two binaries with different LOCAL_PROTOCOL constants, same package semver, then run the non-ignored `daemon_lifecycle::two_executable_stale_dev_instance` in both directions (old executable removed in one case); build/run fixture must be committed by T2a | 2 |
| T2b | Move binary tests to an explicit test daemon on an isolated `ORCH_SOCKET` (test-eng-2) | R25 | `crates/orch-cli/tests/common/**`, `crates/orch-cli/tests/mcp_common/**` | S2a | `cargo test -p orch-cli --tests` | 2 |
| W2 | README "Upgrading from v0.13.0"; CHANGELOG; every changed command chapter; command reference; remove manual 18's developmental notice (tech-writer) | R22, R35 | `README.md`, `CHANGELOG.md`, `docs/manual/**` | S2a | links resolve; `docs/manual/tools/gen-command-reference.sh` output committed | 2 |
| I2, G2, CP4 | Integrate, board 2 with CLI round-trip latency, adjudicate, PR 2 | R24 | per finding | S2b, S2c, T2a, T2b, W2 | the CI set; cold gate; latency report | 2 |
| S3a, S3b | TUI push; web push (senior-eng; frontend-eng) | R16 | `crates/orch-cli/src/{tui,web}/**` | PR 2 frozen | `tui_daemon::push_updates_without_disk_observer` | 3 |
| S3c | Subscription hub; MCP `resources/updated`; delete `disk_observer` (senior-eng-2) | R16, R17 | `crates/orch-daemon/**`, `crates/orch-cli/src/mcp/resources.rs`, `crates/orch-cli/src/serve/disk_observer.rs` | PR 2 frozen | `daemon_push::subscribe_snapshot_gap_resync`; `daemon_push::mcp_resource_updates` | 3 |
| T3 | Push tests; TUI/web rows of the matrix (test-eng-2) | R16, R18 | `crates/orch-cli/tests/**` | S3a, S3b, S3c | `cargo test -p orch-cli --tests` | 3 |
| I3, G3, QA1 | Integrate, board 3, QA pass 1 on the owner's Mac (qa-engineer) | R24, R28 | per finding | T3 | reviewer verdicts; QA transcript | 3 |
| S4 | Crate and source-site allowlist; remove client infra/engine dependencies, the MCP FIFO lane and dead cross-process code (senior-eng-2; release-eng co-signs the manifest) | R3 | `xtask/**`, `crates/orch-cli/src/{serve,mcp}/**`, `crates/orch-cli/Cargo.toml` | PR 3 frozen | `cargo xtask crate-graph` passes, and fails on a planted `orch_infra` import in a client file | 4 |
| W4 | ARCHITECTURE §2; RUNBOOK; manual 17/18 sweep with captured output; spec Verification table; spec and plan `done`; Pensieve mirror (tech-writer) | R22, R35, R38 | `docs/**`, `README.md`, `CHANGELOG.md` | S4 | every example re-run at the frozen commit; fact-check | 4 |
| I4, G4, QA2 | Integrate, board 4, QA pass 2 | R22, R24, R28 | per finding | S4, W4 | reviewer verdicts; QA transcript | 4 |

## Status

Snapshot of goal `goal_zX0WXev` at 2026-11-09, taken by W1; wave-B rows updated by I1. Not updated automatically; I1 and later writers edit
it.

| Item | Task | State |
|---|---|---|
| Gate 1 (plan and design questions) | — | approved by the owner, 2026-11-09 |
| A1, SE, RT, A1b | `tsk_7bvf0xQ`, `tsk_1u_dvll`, `tsk_YVAjJFo`, `tsk_3BJ8YPF` | done; spec v2 `e1ef3b60` |
| B0, B0c, B0d, B0 review, B0d re-review | `tsk_5j4sVEl`, `tsk_I7nHT_o`, `tsk_aDaGYFl`, `tsk_ph7n2g7`, `tsk_J3ae0CX` | implemented on `feature/daemon-engine` at `f44d60b7`; review passed |
| T0 | `tsk_YXbg6QP` | implemented, failing-first (`d0da296d`) |
| W0 | `tsk_hcfxtAc` | drafted (`a2dab502`, `50409748`) |
| CP1, CP2, gate 2 (spec) | `tsk_Yvrqc9i`, `tsk_W2m3n_b` | spec approved, ADR 0012 accepted (`c14698fb`) |
| S1a-api | `tsk_K8xOIZl` | done (`f91ff25f`); integrated by I1 |
| S1a-impl | `tsk_sStUSpy` | done (`d0b657e5`, startup seams `fab0fa1a` via `tsk_x8pp05Z`); integrated by I1 |
| S1b | `tsk_8wgBbdh` | done (`2d2f5f05`); integrated by I1 |
| S1c | `tsk_fiC9flm` | done (`ba0c7037`, continuations `tsk_fssJiFW`, `tsk_MqrS4Yx`); integrated by I1 |
| S1d | `tsk__4ahant` | done (`959001ad`); integrated by I1 |
| T1 | `tsk_UtDGaCh` | done (`ca5492d9`; maintenance-timer seam `14ecefbb` via `tsk_HnLcpiw`, observation fix `2d097798` via `tsk_cVJbOAK`); integrated by I1 |
| T1b | `tsk_AWRlxx9` | done (`e5216b2f`); integrated by I1 |
| W1 | `tsk_ArQCbkH` | done (`05d55220`); written ahead of the code, so the facts board checks it at the I1 SHA |
| I1 | `tsk_vNn_rPy` | integrated on `feature/daemon-core`; the frozen SHA is the commit that changes this row (recorded in the I1 report) |
| G1: code, security, resilience, facts, cold gate | `tsk_Gc_B8Ya`, `tsk_mQszv9B`, `tsk_jeSSHfY`, `tsk_Zt3JSh7`, `tsk_JpO4CUF` | pending |
| CP3 | `tsk__g95cwZ` | pending |
| PR 2–4 | — | pending |

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
- Fix round 1, verify at I1b: `crates/orch-daemon/tests/daemon_peer_boundary.rs`,
  `daemon_endpoint_safety.rs`, `staging_cleanup.rs`; `crates/orch-cli/tests/daemon_stop_wait.rs`;
  `crates/orch-engine/tests/daemon_startup_index.rs`, `daemon_recovery_errors.rs`. The spec has one
  runnable command per target. I1b verifies the integrated function names/counts, the extensions to existing
  security/lifecycle/path targets, diagnostic env/stderr and client unknown-outcome regressions, and repeats
  the peer suites 3×. These fixes are not claimed present or passing at `b1216ce4`.
- PR 2 planned: `daemon_no_daemon::all_frontends_matrix` with process-table and spawn assertions;
  `daemon_stress::real_tui_cli_mcp_share_state_order` (60 s acceptance run); two compiled executables with
  different `LOCAL_PROTOCOL` constants under T2a (R6/R32); pgid-derived acknowledgement under S2a/T2a (R39).
  The stress test is the evidence that D10 is closed and replaces the runbook hazard.
- PR 3 planned: `daemon_push::*`, `tui_daemon::*`, `web_daemon::*`; QA pass 1 on the owner's Mac (R28).
- Hermeticity: no test touches a real project's daemon or `~/.orchestry`; sockets live under a per-test temp
  directory on a path of at most 100 bytes; fake shell agents only.
- CI set on every PR: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test --workspace`; `cargo xtask crate-graph`; `cargo deny check`; Linux CI. Cold `./xtask/gate.sh` on
  macOS for each PR (process lifecycle, layering and security change in every one).

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
Gate 2's Q2 stop intent and Q5 regrouping were approved on 2026-11-09. CP3 rulings 8.17/8.20 record:

1. **R39 deferred to PR 2 hardening, S2a implementation / T2a tests.** Kernel-pgid-derived acknowledgement
   for agent-originated stop/dispatch off is absent at PR 1. It prevents same-UID accidents rather than
   providing authorization; existing kernel-UID/project checks remain mandatory. The developmental PR 1
   converts no stateful frontend, so implementing attribution with PR 2's final control paths avoids doing
   the cutover work twice. PR 2 must test matching live-run pgids, unrelated owner pgids, explicit
   `--from-agent`, and spoofed caller-pid rejection. This is a reasoned should-row deferral, not a pass.
2. **R6/R32 two-executable skew acceptance is UNMET at PR 1.** T1b's
   `orch-cli/daemon_lifecycle::two_executable_stale_dev_instance` is ignored and has an empty body; it does
   not supply acceptance even if run with `--ignored`. Harness-advertised versions cover wire behavior only.
   T2a must build two real binaries with different `LOCAL_PROTOCOL` constants and the same package semver,
   prove ordinary calls fail before mutation and frozen stop completes in both directions, and remove the
   old executable in one case. Keep the existing binary-semver mismatch vectors. The PR 1 body must state
   this unmet acceptance explicitly; no owner waiver is implied. Same semver/protocol with a different build
   remains an intentional detection limit.

I1b/board 1b must still verify the fix round before PR 1 can advance; these dispositions do not certify it.
