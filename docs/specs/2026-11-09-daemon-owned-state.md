# Daemon-owned state

status: approved
owner: architect; requirements approval: owner at gate 2
date: 2026-11-09
revision: 2 (round-1 challenge incorporated; approved by the owner at gate 2, 2026-11-09: spec=approved q2=stop-intent q5=regrouped)
plan: [Daemon-owned state](../plans/2026-11-09-daemon-owned-state.md)

## Source

The following are verbatim excerpts of the owner's request recorded in the goal, including the 2026-11-09 v2 amendment. Source items 1–12 retain the owner's numbering. Labels 13–23 number the remaining amendment, context, constraints, acceptance and delivery paragraphs without changing their wording. Repository checkout instructions and tracker metadata are not requirements and are not reproduced. The owner approves these requirements at **gate 2** (approved plan Q7: owner); this draft is not that approval. Gate 1 approved the v2 plan's recommendations, with Q7=owner and Q8=stack.

### Source item 1

> 1. Daemon: one process per project directory. It holds the orchestrator PID lock, owns all store access and all agent-process spawning, and listens on a Unix domain socket in the project (e.g. .orchestry/orch.sock; dir 0700, socket 0600). macOS and Linux only (Unix socket path length limit is 104 bytes on macOS: design for long temp paths — e.g. ORCH_SOCKET override and/or a short socket path under $TMPDIR keyed by a project-root hash, with the path recorded in .orchestry/).

### Source item 2

> 2. Clients: the CLI (every command except init and the daemon's own entry point), the TUI, orch web, orch mcp, and agent-run orch invocations all talk to the daemon only. No client links to the store implementations or opens .orchestry/ for state (reads included: one path, not two). Enforce this structurally — e.g. a new client crate that depends only on orch-domain/protocol, and the crate-graph xtask + ARCHITECTURE §3.1 updated so a client crate importing orch-infra storage fails the gate.

### Source item 3

> 3. No daemon, no auto-start: when no daemon answers (no socket, stale socket, connection refused, handshake failure), every client — CLI, TUI, orch web, orch mcp — fails fast with one uniform message, e.g. `No orch daemon is running for <project root>. Start it with: orch daemon start` (exact wording and command name decided in the spec), and a dedicated exit code (document it next to the existing exit codes). The TUI and orch web show the same message and exit (or, for the TUI, show it and offer to retry; never start anything). orch mcp returns it as a tool/initialize error so the MCP client surfaces it. Nothing in the client code path may spawn, fork or exec a daemon. Starting the daemon is only ever an explicit owner action: a foreground `orch daemon start` (or `orch serve` — the spec decides the names) and a detached `orch daemon start --detach` for the owner's convenience; `orch init` creates the project and prints how to start the daemon, it does not start one. Two explicit starts racing must converge on one daemon (the PID lock decides; the loser exits with "already running (PID …)").

### Source item 4

> 4. Protocol: a versioned request/response protocol over the socket with a handshake (protocol version + binary version). Reuse orch-peer's framing/codec where it fits; do not invent a second framing without a reason recorded in the ADR. Version skew: a newer client vs an older daemon (or vice versa) must not corrupt state; the client fails with the uniform no-daemon-style message that names both versions and tells the owner to restart the daemon (`orch daemon stop` then start). No automatic restart. Requests carry the caller identity (ORCH_AGENT_ID / --agent) exactly as today; the socket is the authority boundary (same-uid peer credential check).

### Source item 5

> 5. Engine inside the daemon: the existing services run unchanged in the daemon. Because dispatch, running-task state and store writes are now in one process, make these true and tested: pausing a goal is atomic with respect to dispatch (no task can start after its cancel was decided); task cancel through any client stops a live agent process (closing the R3 "store-only task_cancel" gap from #69); context expiry is a daemon-internal step, never a side effect of a client read.

### Source item 6

> 6. Dispatch is a daemon mode, not a separate process: define what orch serve, orch run --watch, orch run <id>, orch run --once mean now (recommended: the daemon hosts the loop; these commands turn dispatch on/off or run one tick via the socket and stream logs from the daemon; a freshly started daemon starts with dispatch OFF unless started with a flag, so the owner can queue work before dispatch begins). Document the mapping in the manual and CHANGELOG.

### Source item 7

> 7. Lifecycle: `orch daemon status|stop` (or equivalent); clean exit on SIGTERM/SIGINT/SIGHUP leaving running agent processes reconciled per existing state.json semantics; after a daemon crash the next client call reports "no daemon" (row 3) and the owner restarts it; on start the daemon reconciles claimed/running tasks as today and replaces a stale socket file. No idle exit, no watchdog, no auto-restart in this goal.

### Source item 8

> 8. Push: clients can subscribe to change notifications (tasks/goals/agents/messages/context) so the TUI and web stop polling the directory, and orch mcp emits resources/updated for its orch:// resources. If the architect splits this into a later PR, it must still land in this goal.

### Source item 9

> 9. orch mcp becomes a thin stdio-to-socket bridge: same tools, same results, same modes (--read-only / --allow-destructive), same identity rules. Remove the FIFO store lane if it is now redundant; remove the "concurrent writers" hazard text from manual 17 and the RUNBOOK and replace it with the daemon model.

### Source item 10

> 10. Remote peers (orch-peer TCP) keep working unchanged; mixed 0.5.x–0.14 peers still interoperate (no peer-protocol change) unless the plan says otherwise and the owner approves. Unifying local-socket and remote transports is a should, not a must.

### Source item 11

> 11. Security: socket permissions and peer-uid check; every request validated as untrusted input; no secrets in protocol logs; the daemon never inherits agent env (DEBUG/PWDEBUG etc.) into its own listener. Security review is mandatory at the gate.

### Source item 12

> 12. Docs: spec (docs/specs/2026-11-09-daemon-owned-state.md), plan, ADR 0012 (supersedes ADR 0011 decision 7 and the D10 acceptance; records "no auto-start" as an owner decision with its reason: the owner wants to know when a long-lived process starts on their machine), ARCHITECTURE §2 rewritten (single-writer model, what the daemon guarantees, what it does not), §3.1 crate diagram, RUNBOOK (daemon lifecycle, "no daemon" message, stuck/stale socket, crash recovery, version skew; delete the index-rebuild and concurrent-MCP-writers procedures or mark them legacy), manual (new daemon chapter; update every command whose semantics changed; MCP chapter, including that an MCP client's first call fails until the owner starts the daemon), README (Upgrading from v0.13.0: this is a behaviour change — no orch command works without a running daemon; how to start, see and stop it), CHANGELOG [Unreleased].

### Source item 13

> AMENDED by the owner 2026-11-09 (v2): clients NEVER start a daemon. There is no auto-start anywhere. See rows 3, 6, 7 and the tests section.

### Source item 14

> orch's state is files under .orchestry/ (one YAML per entity plus a per-store _index.json). Today N processes write them independently: the orchestrator (orch run --watch / orch serve), one-shot CLI commands, the TUI, orch web, and orch mcp servers. The PID lock serialises orchestrators only. Documented, unrepaired races (D10 / ARCHITECTURE §2.1): entity+index written as two steps; whole-record saves with no compare-and-swap (last writer wins); context expiry publishing a computed keep-list from list/get, including from --read-only MCP sessions; a per-process ensure_dir cache. PR #69/#72 accepted these as residuals and documented recovery.

### Source item 15

> Exactly one process per project owns .orchestry/ and every store. Everything else — the CLI, the TUI, orch web, orch mcp, and the orch commands agents run inside their tasks — is a client of that process over a local socket, for reads and writes alike. There is no direct-store fallback path in any client, and no client ever starts a daemon on its own: when no daemon answers, the client fails with a clear, uniform message that names the project and the exact command to start the daemon, and a dedicated exit code. The owner starts the daemon explicitly. This removes D10 as a class (single writer) instead of patching each race.
>
> Storage stays on files behind the existing orch-core store traits. Do NOT implement SQLite or change any on-disk format in this goal; that is deferred and will be a separate goal once the daemon is the only caller of the stores. Do not touch the YAML/index formats except where removing cross-process code that is now dead (and even that is optional; prefer leaving storage untouched).

### Source item 16

> Auto-start of any kind; SQLite or any storage-format change (deferred); Windows; a per-user (multi-project) daemon; changing the remote peer protocol; changing config.yml / org templates / run-event files; a web or TUI SQL console.

### Source item 17

> cargo fmt --check, clippy -D warnings, cargo test --workspace, cargo xtask crate-graph, cargo deny, and a cold ./xtask/gate.sh pass on macOS; Linux CI green on every PR.

### Source item 18

> An in-process daemon test harness so the existing ~5,600 tests keep their model; stdio MCP tests run against a test daemon; binary-level tests (yaml_contract, routing, mcp_stdio …) start an explicit test daemon on an isolated short socket path — no test may touch a real project's daemon or ~/.orchestry.

### Source item 19

> Concurrency proof: a stress test with the TUI/web-equivalent client, several CLI clients and two MCP clients mutating one project concurrently for N seconds, then asserting entity files == index, no lost field updates (versioned writes observed in order), no context key lost on renewal. This test replaces the RUNBOOK hazard and is the evidence that D10 is closed.

### Source item 20

> No-daemon tests: every client kind (CLI read, CLI write, TUI, web, MCP initialize and a tool call) against a project with no daemon, with a stale socket file, and with a daemon of a different protocol version — each produces the uniform message and exit code, and a process-table assertion proves no process was spawned. Lifecycle tests: two explicit starts racing; daemon killed mid-run then next client call reports no-daemon; explicit restart reconciles running tasks; stale socket replaced on start.

### Source item 21

> Pause atomicity test (goal paused while a tick is dispatching: no task starts after the cancel decision) and task-cancel-kills-process test.

### Source item 22

> Manual verification by qa-engineer on the owner's Mac: TUI + a Claude Code MCP session + CLI against one daemon the owner started; `pgrep -fl orch` shows one daemon; kill -9 it and confirm every client reports no-daemon and nothing restarts it until the owner does.

### Source item 23

> tier=mid, stance=0.5, create PR. This exceeds the ~3,000-line guideline, so deliver as an ordered series of stacked PRs against main, each green and leaving main fully working (recommended order: 1 protocol + daemon server behind the existing entry points, unused by clients; 2 client crate + CLI/MCP conversion with the no-daemon error path; 3 TUI/web conversion + push; 4 remove direct store access from clients, crate-graph enforcement, docs; the plan may regroup). The owner merges each PR; no release, no tag, no version bump in this goal (the release will be 0.14.0 and is separate). Approval gate: present the design questions (dispatch-mode mapping, daemon command names and exit code, socket path scheme, subscription protocol, PR split) with recommendations before building.

## Requirements

New named tests below are acceptance targets, not claims that they exist or pass. Test owners may rename targets only by updating this table to the exact implemented test at integration. Existing suites stay required. Source numbers refer to the numbered quotations above.

| Id | Requirement | Source | Priority | Acceptance check (test or command; platform) |
|---|---|---|---|---|
| R1 | One daemon per project shall own all stores and agent execution for its whole lifetime, including dispatch OFF; refresh the unchanged PID lock every ≤15 s independently of dispatch and check ownership on each refresh. Missing/foreign PID disables dispatch and reports ownership_lost without silently recreating the lock. | item 1, 2, 14, 15; SE-2 | must | `crates/orch-daemon/tests/daemon_lifecycle.rs::two_explicit_starts_one_owner`; `legacy_acquire_cannot_steal_idle_owner`; `pid_ownership_loss_disables_dispatch`; macOS + Linux CI |
| R2 | The default socket shall be .orchestry/run/orch.sock with a daemon-owned 0700 parent, bound under umask 0177 (0600); retain .orchestry mode, require owner euid and no group/world writes, validate the ≤100-byte final path including /run, override/fallback, metadata descriptors and gitignore rules below. | item 1; plan Q3; SE-3/4/5/21 | must | `crates/orch-daemon/tests/daemon_paths.rs::long_root_override_and_wrong_project`; `daemon_security.rs::uid_permissions_and_symlinks`; `existing_project_mode_preserved`; `fallback_precreation_is_rejected`; `discovery_fifo_cannot_hang`; `foreign_euid_start_refused`; `init_ignores_runtime_metadata`; macOS + Linux CI |
| R3 | All stateful CLI, TUI, web, MCP and agent operations shall use orch-client exclusively after PR 2; only init/explicit start may reach narrow daemon bootstrap APIs. Clients read discovery only. Final source-site gate scans production src/ only; tests/ may use the daemon testing API through declared dev dependencies. | item 2, 15; RT1; T0 §8.5 | must | `cargo xtask crate-graph`; `crates/orch-cli/tests/daemon_contract.rs::all_surfaces_mapped`; forbidden production import and permitted tests-only fixture checks; macOS + Linux CI |
| R4 | Every client shall report the uniform NoDaemon text and code 7 on absent/refused/stale endpoints or failed handshake; client connection code shall neither mutate discovery/socket files nor create a daemon process. | item 3, 13, 15, 20 | must | `crates/orch-cli/tests/daemon_no_daemon.rs::all_frontends_matrix`; absent/stale/refused/handshake fixtures with process-table and spawn assertions; macOS + Linux CI |
| R5 | Explicit daemon start shall default to foreground/dispatch OFF, support --detach, and publish listening <socket> only after readiness; serve/run --watch explicitly start foreground/ON. Racing starts yield one owner, loser exit 4 with PID. Init re-run is idempotent, prints the hint and never starts a daemon. | item 3, 6, 13; plan Q1; W0 Q2; T0 §8.3; RT4/5 | must | `crates/orch-daemon/tests/daemon_lifecycle.rs::two_explicit_starts_one_owner`; `foreground_readiness_line`; `crates/orch-cli/tests/daemon_dispatch.rs::mapping_and_disconnect`; `init_never_starts_daemon`; `init_rerun_is_idempotent`; macOS + Linux CI |
| R6 | The frozen unknown-tolerant hello/welcome/reject envelope shall carry protocol u32, exact package semver, dev/ino project identity and session identity, within 16 KiB. All ordinary requests require equal versions in both directions; only hello intent stop crosses skew after uid/project checks, using normal clean shutdown and a frozen completion response. | item 4, 13, 20; Q2 refinement; RT2/G1/SE-7/8 | must | `crates/orch-daemon/tests/daemon_protocol.rs::strict_skew_both_directions`; `future_hello_fields_preserve_skew`; `stop_crosses_version_skew` (two executables, both directions, old executable removed in one); `wrong_project_stop_is_rejected`; macOS + Linux CI |
| R7 | Reuse bounded 1 MiB NDJSON framing from orch-peer; typed request DTOs shall contain no untyped Value, use deny_unknown_fields and no flatten, reject duplicate fields including nested args/hello.caller, and pre-scan depth ≤64. Use ids, terminal ok/err, parts and events; invalid/oversize input cannot mutate or disappear silently. | item 4, 10; SE-7/14 | must | `crates/orch-daemon/tests/daemon_protocol.rs::frame_catalogue_and_bounds`; `request_dtos_are_typed_and_closed`; `cargo test -p orch-peer`; macOS + Linux CI |
| R8 | Both peers shall validate kernel euid (client before any byte); pid is optional attribution only. Client resolves flag-over-env identity and sender defaults once; session identity/mode cannot change. Daemon shall authorize each op from the exhaustive no-wildcard class table before admission; explicit per-tool sender overrides retain current rules. | item 4, 9, 11; SE-5/9 | must | `crates/orch-cli/tests/daemon_contract.rs::identity_modes_and_projections`; `mode_enforced_daemon_side_with_raw_client`; `session_identity_cannot_change`; `daemon_security.rs::hostile_discovery_receives_no_bytes`; macOS + Linux CI (injected foreign uid; real cross-uid separately recorded) |
| R9 | Pausing a goal shall order cancellation decisions with dispatch/spawn so a cancelled pending task cannot start afterward; unrelated goals and current goal-scoped pause behaviour shall be preserved. | item 5, 21 | must | `crates/orch-engine/tests/daemon_engine.rs::pause_prevents_post_decision_spawn`; barrier-controlled dispatch race; macOS + Linux CI |
| R10 | Task cancellation through every frontend shall reach the daemon live-run registry and stop a local running agent before success; completion races cannot resurrect cancellation or lose other fields. | item 5, 21 | must | `crates/orch-cli/tests/daemon_engine.rs::cancel_kills_live_process`; fake shell adapter through CLI/MCP/backend; macOS + Linux CI |
| R11 | Context expiry shall be daemon maintenance even with dispatch OFF; reads never purge. Renewal and expiry share ordering and recheck expiry. Expose owner admin maintenance.expire_context for explicit testing; expiry errors are reported without skipping reconciliation, failing watch startup or entering the fatal tick breaker. | item 5, 14, 19; T0 §8.2; B0 F1 lead ruling | must | `crates/orch-engine/tests/daemon_engine.rs::reads_do_not_expire_and_renewal_survives`; `expiry_error_preserves_reconciliation`; `crates/orch-daemon/tests/harness.rs::explicit_expiry_matches_timer`; macOS + Linux CI |
| R12 | Serialize complete read/modify/write transitions across RPC, dispatch, collectors, peer handlers, full-record goal edits and expiry. An overlapping title edit cannot resurrect Active after pause. Successful mutations carry daemon-instance-scoped commit_seq (the T0 write_seq ordinal); reads carry the observed sequence. No permit spans agent lifetime or socket wait. | item 5, 14, 15, 19; B0 F2; T0 §8.1 | must | `crates/orch-cli/tests/daemon_stress.rs::all_clients_share_state_order`; `crates/orch-daemon/tests/daemon_engine.rs::goal_edit_cannot_resurrect_active` (barrier-controlled, title and Paused survive); `mutation_responses_order_commit_seq`; reentrant-agent/collector tests; macOS + Linux CI |
| R13 | Dispatch RPCs shall obey the command and batch policy below: serve --once requires global OFF, suppresses seeding, ticks until the tracked workload is terminal, never restores a stale global switch, and ends on explicit dispatch on/off or disconnect with agents continuing. Show ON/OFF in start/status/TUI/web; restart defaults OFF. | item 6; plan Q1; RT5/G3/W0 Q2 | must | `crates/orch-cli/tests/daemon_dispatch.rs::mapping_and_disconnect`; `serve_once_two_pane_sequence`; `serve_against_off_daemon_has_toggle_hint`; `dispatch_state_visible_on_all_surfaces`; macOS + Linux CI (TUI in tmux) |
| R14 | Live status shall show executable path, started_at, versions, dispatch and capacity; compatible stop, version-invariant stop intent and SIGTERM/SIGINT/SIGHUP shall stop admission/dispatch, kill/reconcile agents, report cleanup errors and release ownership last. | item 7; RT2/3/4; SE-8 | must | `crates/orch-daemon/tests/daemon_lifecycle.rs::signals_reconcile_and_release`; `stop_waits_for_cleanup`; `status_shows_executable_provenance`; macOS + Linux CI |
| R15 | Only explicit start shall replace a proven stale socket and reconcile before readiness; clients never repair. Startup invokes existing index read/rebuild logic (missing/corrupt indexes only), preserving the cross-file crash limitation; legacy repair is stop, remove affected _index.json, start. | item 3, 7, 13, 20; W0 Q3 | must | `crates/orch-cli/tests/daemon_lifecycle.rs::crash_requires_explicit_start`; `stale_socket_replaced_only_on_start`; `startup_rebuilds_missing_index`; `valid_stale_index_requires_offline_repair`; macOS + Linux CI |
| R16 | Subscriptions shall push coarse tasks/goals/agents/messages/context invalidations and bus events with per-subscription sequence and daemon instance. Gaps, overflow and changed instance require resync, with no directory polling by TUI or web. | item 8 | must | `crates/orch-daemon/tests/daemon_push.rs::subscribe_snapshot_gap_resync`; `crates/orch-cli/tests/tui_daemon.rs::push_updates_without_disk_observer`; macOS + Linux CI |
| R17 | MCP shall bridge stdio to socket with the same tools, projections, --read-only/--allow-destructive and identity contracts; the redundant store FIFO shall be removed, bounded admission retained, and subscribed orch:// resources shall receive resources/updated. | item 8, 9 | must | `cargo test -p orch-cli --test mcp_contract`; `cargo test -p orch-cli --test mcp_tools`; `cargo test -p orch-cli --test mcp_stdio`; `daemon_push.rs::mcp_resource_updates`; macOS + Linux CI |
| R18 | Absent/incompatible daemon shall produce central error text and exit_code 7 in MCP initialize/tool responses, bridge alive until EOF (exit 0); owner-start then reinitialize/retry can recover. TUI uses r/q, web preflight exits 7 and live outage gives HTTP503/banner; only authenticated web sessions see project paths. | item 3, 9, 20; plan Q0/Q0b; RT4; SE-17 | must | `crates/orch-cli/tests/daemon_no_daemon.rs::all_frontends_matrix`; `mcp_initialize_fails_then_recovers`; `tui_daemon.rs::retry_and_quit`; `web_daemon.rs::startup_and_live_outage`; `web_unauthenticated_503_omits_paths`; macOS + Linux CI |
| R19 | Remote TCP Frame schema/PROTOCOL and old-peer tolerance shall stay unchanged; lift only generic framing, retaining peer-specific bounding and optional fields. Local strict semver checks never apply to TCP peers. | item 10 | must | `cargo test -p orch-peer`; `cargo test -p orch-cli --test remote_peers`; legacy 0.5.x/current fixtures in both directions; macOS + Linux CI |
| R20 | Enforce stated counts and byte budgets, separate handshake/session limits and typed validation before mutation. Logs contain only op/id/error-code/execution/timing, never err.message, serde error text or payload; detached .orchestry/daemon.log is 0600 and bounded/rotated, at every log level. | item 11; G3; SE-6/7/13/14 | must | `crates/orch-daemon/tests/daemon_protocol.rs::frame_catalogue_and_bounds`; `daemon_security.rs::logs_omit_payload_canaries` (success, service error, parse error); `overload_keeps_control_available`; `detached_log_is_private_and_capped`; macOS + Linux CI |
| R21 | Before runtime/threads start, capture sanitized env using the named denylist below; never mutate runtime process env. Spawn via env_clear plus snapshot/config env/explicit inserts; requests carry no env/PATH/cwd/argv/adapter-command execution overrides. Status exposes removed names only; document daemon-start credential lifetime and require security gate. | item 11; SE-12/13; W0 Q4 | must | `crates/orch-daemon/tests/daemon_security.rs::agent_environment_does_not_configure_listener`; `request_dtos_reject_execution_overrides`; `agent_children_receive_owner_routing`; security report at frozen commit; macOS + Linux CI |
| R22 | Publish this spec, plan, proposed ADR0012, ARCHITECTURE §2/§3.1, lifecycle/no-daemon/recovery RUNBOOK, daemon manual chapter and changed command chapters, MCP chapter, README upgrade instructions and CHANGELOG Unreleased. Supersede ADR0011 d7/D10 explicitly; no-auto-start reason is the owner wanting to know when a long-lived process starts. | item 12 | must | Fact-check report at frozen PR4 commit covering every named file and command; `git diff --name-only <base>..HEAD`; macOS; source/markdown checks on Linux CI |
| R23 | Runtime discovery/coordination files may be added but existing YAML/index/config.yml/org template/run-event schemas and orch-core storage traits remain unchanged unless separately approved. The concurrency claim shall explicitly exclude crash transactionality/external writers. | item 14, 15, 16 | must | `cargo test -p orch-cli --test yaml_contract`; storage serialization fixtures and peer tests; `daemon_contract.rs::stored_formats_unchanged`; macOS + Linux CI |
| R24 | Every PR shall pass the CI set on macOS and Linux CI; a cold gate shall pass on macOS for lifecycle/layering/security changes. No base failure is reported as a pass. | item 17, 23 | must | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo xtask crate-graph`; `cargo deny check`; cold `./xtask/gate.sh` (macOS), Linux CI each PR |
| R25 | Provide an in-process harness and explicit isolated binary test daemons; migrate MCP/binary fixtures, never reach real daemon/global state. ORCH_TEST_SOCKET_ROOT restricts every resolved endpoint to the validated test root without changing credentials/version checks; it is child/test configuration only, never cargo-wide env. | item 18; T0 §8.4/5 | must | `crates/orch-daemon/tests/harness.rs::in_process_and_socket_share_dispatcher`; `crates/orch-client/tests/isolation.rs::test_socket_root_rejects_escape`; `cargo test -p orch-cli --test yaml_contract`; routing/mcp_stdio; cold gate home hygiene; macOS + Linux CI |
| R26 | At PR 2 head and final integration, run real orch tui in isolated tmux plus 4 CLI and 2 MCP clients concurrently with dispatch/collectors/expiry, N seconds (10 default, 60 acceptance); drive actual TUI mutations and web PTY routing/preflight. Assert ordered field updates, entity/index equality and retained renewals with no swallowed errors. | item 19; RT1 | must | `ORCH_STRESS_SECONDS=60 cargo test -p orch-cli --test daemon_stress real_tui_cli_mcp_share_state_order -- --exact`; `all_clients_share_state_order`; `web_daemon.rs::pty_uses_selected_daemon`; `startup_and_live_outage`; macOS + Linux CI (tmux), record N and operations per frontend |
| R27 | No-daemon matrix covers CLI read/write, TUI, web, MCP initialize/tool × no endpoint, stale socket and incompatible protocol, with binary-version mismatch both directions too. Assert same message/code channel, no spawn and no unlink; lifecycle includes race, kill mid-run, explicit recovery and stale replacement. | item 20 | must | `cargo test -p orch-cli --test daemon_no_daemon`; `cargo test -p orch-cli --test daemon_lifecycle`; process-table records plus injected spawn sentinel; macOS + Linux CI |
| R28 | QA on owner Mac shall test one explicitly started test daemon with TUI/browser/Claude Code MCP/CLI, kill -9, fail without relaunch and explicitly recover; also begin with MCP initialize failing before owner start, then record the real client reconnect/re-enable/session-restart action needed. | item 22; RT4 | must | qa-engineer transcript on owner Mac: isolated tmux/test root; first MCP failure; explicit start; reconnect action; pgrep distinguishes daemon/clients; kill only owned test PID; each failure channel; explicit recovery; no real-project signal |
| R29 | Deliver the regrouped stack below: PR 1 developmental daemon gated by ORCH_DAEMON_DEVELOPMENTAL=1/banner, no converted clients; PR 2 all stateful frontends cut over together and gate removed; PR 3 push; PR 4 enforcement/docs. Every head green; owner gate 2 precedes daemon implementation and owner merges, no version bump. | item 23, 8, 12; RT1; SE-20; plan §8 | must | `crates/orch-cli/tests/daemon_contract.rs::developmental_gate_at_pr1_only`; `all_surfaces_mapped` and R26 at PR 2 head; lead release report with four PR/base/head/CI links; `git diff <base>..HEAD -- Cargo.toml CHANGELOG.md`; macOS + Linux CI |
| R30 | Long-running daemon shall apply configuration/default changes to subsequent operations/dispatch without rebuilding the live process registry; authored interactive edits submit a base digest and fail rather than overwriting a newer authored edit. | item 2, 5, 9, 19 | must | `crates/orch-cli/tests/daemon_contract.rs::config_refresh_and_edit_conflict`; old runtime fields preserved through edit; macOS + Linux CI |
| R31 | Explicit daemon start/serve/run --watch shall refuse with exit 2 in ORCH_TASK_ID or ORCH_AGENT_ID context unless --from-agent is explicit; all agent-context NoDaemon displays shall say Do not start it; report this to the owner and stop. An identified MCP agent uses this variant too. | item 3, 11, 13; RT4; SE-1 | must | `crates/orch-cli/tests/daemon_lifecycle.rs::agent_context_cannot_start_daemon`; `agent_no_daemon_never_instructs_start`; macOS + Linux CI |
| R32 | Developers shall stop the daemon before switching builds and explicitly start the intended executable. daemon.json/status record current_exe and started_at. Bump LOCAL_PROTOCOL for any operation wire/semantic change within the stack (PR 1 starts at 1); same semver and protocol with different build is deliberately undetectable. | item 4, 23; RT3 | must | `crates/orch-daemon/tests/daemon_protocol.rs::two_executable_stale_dev_instance` (same semver/different protocol rejected, same/same limitation explicit); `status_shows_executable_provenance`; PR contract-diff review checks LOCAL_PROTOCOL; macOS + Linux CI |
| R33 | Support one host and one canonical state directory only; compare dev/ino from O_DIRECTORY plus O_NOFOLLOW fstat. Apply the project-selection table, reject endpoint/project conflicts as wrong_project naming both paths with no delete/kill hint. Document multi-host shared state unsupported and single-host network storage unverified. | item 1, 4, 10; RT6; SE-18 | must | `crates/orch-client/tests/project_selection.rs::project_selection_vectors` (two checkouts, agent worktree, symlink alias, conflicting env, wrong override); `daemon_protocol.rs::wrong_project_is_rejected`; macOS + Linux CI |
| R34 | Attachment sources shall be absolutized client-side (CLI/TUI cwd, MCP selected project root), with relative paths rejected by daemon. Preserve current destination confinement/limits; disclose that a confined same-uid caller can use daemon file authority (M3 residual), without claiming upload isolation. | item 9, 11; SE-10/11; lead minimum option | must | `crates/orch-cli/tests/daemon_contract.rs::attach_relative_path_resolves_at_client`; `daemon_security.rs::relative_attachment_path_rejected`; `attachment_destination_confined`; macOS + Linux CI |
| R35 | Publish and obey the normative recovery table: agent stops/reports infrastructure failure, lost mutation outcome unknown, explicit MCP recovery, visible script/cron failure, foreground SIGHUP cleanup and detached session/log contract. README/manual/RUNBOOK/agent instructions shall link this section; stop old processes including MCP before upgrade. | item 3, 7, 12, 22; RT4; SE-1/2; W0 Q3 | must | `crates/orch-cli/tests/daemon_lifecycle.rs::agent_calls_across_daemon_death`; `detached_survives_tmux_kill_pane`; `foreground_sighup_stops`; `bootstrap_and_cron_fail_visibly`; R18/R28 MCP cases; docs link/checklist at PR 4; macOS + Linux CI, real MCP owner Mac |
| R36 | Pre-handshake connections shall have a separate budget and 2 s deadline; a full live-session cap returns frozen reject busy, mapped to daemon_busy exit 7 without absence/start wording, with capacity visible in status. Reserved control admission shall keep stop/cancel available. | item 3, 11; SE-6; G3 | must | `crates/orch-daemon/tests/daemon_security.rs::session_cap_reports_busy_not_absent`; `idle_handshakes_do_not_consume_sessions`; `overload_keeps_control_available`; macOS + Linux CI |
| R37 | Hold the stable .orchestry/.daemon-owner.lock guard without unlink or child inheritance; write winning PID only after locking; loser rereads ≤2 s. Never evict a live legacy PID by mtime; live non-daemon PID gives exit 4 plus ps verification hint. Non-contention lock errors fail start. | item 1, 3; G2; SE-15 | must | `crates/orch-daemon/tests/daemon_lifecycle.rs::guard_not_inherited_by_agent_children`; `two_explicit_starts_one_owner`; `live_owner_old_mtime_not_evicted`; `live_foreign_pid_requires_owner_verification`; `unsupported_lock_fails_closed`; macOS + Linux CI |
| R38 | Remove tui --observe and tui --listen; dashboard is always a client, peer listening is a daemon start option. doctor is a client, reporting NoDaemon plus discovery-only facts/exit 7 without state reads; help/version work offline. Same-uid full authority including containers and web/peer principals shall be documented. | item 2, 6, 11, 12; W0 Q2; SE-11/17/19 | must | `crates/orch-cli/tests/daemon_contract.rs::offline_help_and_doctor_contract`; `removed_tui_owner_flags`; security documentation checklist in R22 fact-check; macOS + Linux CI |
| R39 | Daemon should identify agent-originated stop/dispatch off from kernel peer pgid matched to live run pgids and require --from-agent; log kernel pid and originating run, never trust asserted caller pid. This is same-uid accident prevention, not authorization. | item 11; SE-16 lead should ruling | should | `crates/orch-daemon/tests/daemon_security.rs::agent_control_requires_explicit_override`; macOS + Linux CI; if deferred, record reason at PR 1/2 review |
| W1 | No automatic daemon start by any client, bootstrap init, retry or error recovery. | item 3, 13, 16 | won't | Explicit owner lifecycle is required; R4/R5/R27 assert absence. |
| W2 | No SQLite or changes to existing storage formats. | item 15, 16 | won't | Owner deferred backend replacement; R23 verifies preserved formats. |
| W3 | No Windows support. | item 1, 16 | won't | Target Unix sockets and existing macOS/Linux process semantics only. |
| W4 | No per-user/multi-project daemon. | item 1, 16 | won't | One owner per project; global.yml retains existing cross-project semantics. |
| W5 | No change to remote peer protocol. | item 10, 16 | won't | Preserve mixed-peer compatibility; transport unification beyond framing is deferred. |
| W6 | No config.yml/org template/run-event file format change. | item 16 | won't | These are compatibility surfaces; RPC forwards current semantics. |
| W7 | No web/TUI SQL console. | item 16 | won't | No SQL backend or console requested. |
| W8 | No idle exit, watchdog or automatic process replacement after crash/skew. | item 7, 13 | won't | Owner controls daemon lifetime; failure is observable until explicit action. |
| W9 | No release, tag, package-version bump or unapproved PR merge. | item 23 | won't | Separate owner-controlled release; this goal supplies stacked PRs only. |
| W10 | No direct-store fallback, offline state read lane or socket cleanup in clients. | item 2, 3, 15 | won't | Would recreate multiple writers and hide failed ownership; R3/R4 enforce absence. |

## Non-goals

W1–W10 above are explicit exclusions. This does not introduce distributed transactions, persisted entity revisions, exactly-once RPC retries, durable notification history or immunity to same-UID programs that bypass ownership. The new metadata is runtime discovery/coordination, not a replacement storage format. Crash between existing entity/index writes remains a recoverable limitation; deleting indexes while a live daemon runs is not supported.

## Constraints

- macOS/Linux, pinned toolchain, unchanged entity/index/config/template/run-event formats. No auto-start or client read fallback. All clients of the production cutover build use the same daemon.
- One host, one canonical `.orchestry`; two separately initialized checkouts are two projects. Concurrent multi-host/shared-volume operation is unsupported and untested; use distinct local projects with existing TCP peers. Single-host state on NFS/SMB is unverified: correct advisory locking, exclusive create, atomic rename and stable inode identity are prerequisites; unsupported lock operations fail start, never degrade to PID-only ownership.
- Exact package semver and `LOCAL_PROTOCOL` equality protect ordinary operations. The frozen same-uid/project-checked `intent: "stop"` is the only version-invariant control exception; it executes no application request or entity edit, but clean shutdown has the usual agent-cancellation/reconciliation writes. No downgrade negotiation, automatic restart or payload decoding after skew.
- Developer contract: stop before switching builds; start the intended absolute executable. `current_exe()` at start and `started_at` in discovery/status establish provenance, not a content fingerprint. Any wire or semantic change to the operation catalogue bumps `LOCAL_PROTOCOL` (PR 1 = 1), even without a package bump. Same semver + same protocol + different build is deliberately undetectable; remove the old plan's stronger assurance.
- Ordinary clients resolve project → endpoint → server UID → hello → versions → RPC. No client removes stale files, takes ownership or replays a possibly committed mutation. A lost reply after sending a mutation has unknown outcome.
- Fresh/restarted daemon defaults dispatch OFF. Explicit `serve`/`run --watch` start foreground with ON. Stop kills/reconciles agents; dispatch OFF alone leaves them running. Batch semantics and precedence are defined below.

### Uniform failure contract

Central text (frontends may style but cannot change wording):

```text
  ✕ No orch daemon is running for <project root> (<detail>)
    Start it with: orch daemon start
```

For version skew, name both daemon/client binary and protocol values in `<detail>` and replace the hint with `Start it with: orch daemon stop, then orch daemon start`. The stop intent makes that procedure executable with either binary. Agent-context rendering takes precedence over every start/restart hint:

```text
  ✕ The orch daemon for <root> is not running. Do not start it; report this to the owner and stop.
    Detail: <safe detail, including both versions on skew>
```

Agent context is a set `ORCH_TASK_ID` or `ORCH_AGENT_ID`, or an MCP bridge with a resolved agent identity. Explicit start/serve/run --watch refuse there with exit 2 unless the owner deliberately passes `--from-agent`. This prevents accidental LLM-driven startup, not a determined same-uid process unsetting env. Kernel pgid-based stop/off protection is R39 should-hardening.

Capacity is not absence: `daemon_busy (orch daemon PID <kernel peer pid or unavailable> is running; N/N sessions)` is exit 7 with no NoDaemon headline or start hint. `wrong_project` is exit 7 naming expected/actual paths, no deletion/kill instruction. For missing/stale/refused/handshake/permission/lost-connection failures use the common NoDaemon channel and safe detail. Never derive a kill hint or PID liveness from discovery alone.

CLI/web preflight exit 7; TUI q exits 7 (r reconnects only); MCP initialize/tool errors carry exit_code 7 and the bridge remains alive, exiting 0 on EOF. Web live outage is 503/banner: authenticated sessions may see project text, unauthenticated requests get generic `orch daemon unavailable`, no root/socket path. Existing exits 1 general, 2 invalid arguments, 3 uninitialized, 4 lock conflict, 5 adapter, 6 workspace remain. Help/version and syntax validation are offline. `doctor` without a daemon prints the central diagnosis plus safe discovery-file facts and exits 7, never opening stores.

### Socket discovery and project selection

Selection priority is `ORCH_SOCKET` (endpoint) > `ORCH_PROJECT_ROOT` (expected project) > cwd ancestor walk. Endpoint choice does not authorize trusting a server-selected project: resolve the expected root independently from `ORCH_PROJECT_ROOT`, otherwise the cwd ancestor walk; no initialized expected root means exit 3. An explicit project root intentionally overrides cwd (necessary for agent worktrees); incompatible endpoint/discovery/hello identities are conflicts, not grounds to switch project silently.

| Priority/input | Selection | Conflict/outcome |
|---|---|---|
| 1: absolute ORCH_SOCKET | Dial exactly this endpoint; still compare expected project dev/ino from the root selected below. | Wrong daemon identity → wrong_project with expected/actual display paths; no RPC/stop, unlink or kill hint. |
| 2: ORCH_PROJECT_ROOT | Select that canonical owning project, including from an agent task worktree or another cwd. | Invalid/uninitialized root fails, no fallback to cwd; conflicting endpoint fails wrong_project. |
| 3: cwd ancestor walk | Nearest initialized ancestor if neither root override nor higher-priority project input exists. | Two initialized checkouts remain distinct projects even for the same Git repo. |
| Symlink alias to root | Canonicalize the root alias, then open final .orchestry with O_DIRECTORY + O_NOFOLLOW and fstat. | Same dev/ino accepts aliases; a symlink replacing .orchestry itself is rejected. |
| Explicit socket and root absent | Derive expected project via cwd; never infer authority from hello or a socket filename. | No ancestor → uninitialized; wrong override → wrong_project. |

Identity is `(st_dev, st_ino)` of the directory descriptor for `<root>/.orchestry`, carried as `project_id:{dev:u64,ino:u64}`. Path bytes are for display and hashing only; instance distinguishes daemon lifetimes, not projects. Agent adapters explicitly insert both `ORCH_PROJECT_ROOT` and the selected `ORCH_SOCKET`; inherited shell routing cannot redirect children. Tests cover two checkouts, an agent worktree, aliases, root-over-cwd selection, conflicting env endpoints, and wrong override.

Default endpoint is `<root>/.orchestry/run/orch.sock` if ≤100 absolute raw path bytes (including `/run`), otherwise `$TMPDIR/orch-<sha256(canonical root bytes)[..16]>/orch.sock` (unset TMPDIR uses the platform temp directory). Validate fallback and absolute override against the same budget; reject empty/relative/NUL/too-long values and give a short ORCH_SOCKET hint, never truncate. Clients normally read the published endpoint, so a different client TMPDIR is irrelevant. Do not add XDG_RUNTIME_DIR selection or automatic endpoint repair in this goal; the lead-approved TMPDIR scheme stays bounded, and loss of a live endpoint needs owner intervention.

Create `run/` or the fallback directory with mkdir(0700); on EEXIST, open O_DIRECTORY + O_NOFOLLOW then fstat: euid owner, no group/world access. Never chmod/chown a pre-existing fallback. `.orchestry` itself must be euid-owned and not group/world-writable, but need not be 0700 and is not chmodded. Start refuses exit 2 when euid differs from its owner (including euid 0 on non-root-owned projects); root-owned projects are allowed only under that same equality. Bind under umask 0177 set before runtime/threads. Socket mode is 0600. Only ownership-holding explicit start can remove a proven stale, same-owner socket after type/owner checks and a connect probe; never delete symlinks/regular files or a live listener.

`daemon.json` schema 1 records project_root, project_id, socket, pid, binary, protocol, `executable` (current_exe), started_at, instance. Publish atomically, 0600, bounded to 16 KiB; open read-only with O_NOFOLLOW + O_NONBLOCK + O_CLOEXEC, then fstat: regular file, owner euid, no group/world permissions, size bound, bounded read even if it grows. Server UID is checked before any hello byte. Raw paths use the existing proposed lossless tagged-byte encoding; diagnostics escape controls. Runtime `.orchestry/.gitignore` entries include `run/`, `daemon.json`, `daemon.log` and its bounded rotation file; legacy `*.lock` still covers locks.

### Security checklist

- **Exposure:** Unix only; both sides require same-euid kernel credentials, with uid mandatory and pid optional. Linux SO_PEERCRED/macOS runtime peer-credential API; unavailable uid fails closed, unavailable pid is logged as unavailable. Existing peer TCP/web authentication stays separate.
- **Bounds:** frozen handshake ≤16 KiB and 2 s; session frame ≤1 MiB, pre-parse depth ≤64; no untyped Value/flatten in request DTOs; typed duplicate-key rejection at every level. Admission counts both requests (128 ordinary, 16 inflight/session) and encoded queued bytes (64 MiB/project); 16 pre-handshake slots are separate from 64 ordinary sessions. At cap accept/reject busy via bounded overflow handling; reserve bounded internal/control capacity so ordinary work cannot starve stop/cancel. Frozen stop intent does not require an ordinary session slot. Pre-handshake saturation is also explicit busy, not absence; daemon status can display the authenticated rejection counts when no ordinary slot is free. Admission tests must exercise both limits without allowing unbounded overflow readers. Connect 1 s, write 5 s, admission 10 s; 8 subscriptions/session, 256 events/subscription, 4 MiB queued output/session. Transfer chunks ≤256 KiB raw, one active/session, ≤16 MiB assembled input, 64 MiB global staging, 10 s inactivity. Service limits still apply (definition ≤1 MiB/depth64, doctor 2 jobs/120 s). No silent state truncation.
- **Permissions/ownership:** private run/fallback/temporary dirs, socket/metadata/editor/transport/log files 0600; stable guard never unlinked or inherited by children. Winner writes its pid after lock; loser waits/re-reads ≤2 s and reports lock conflict without inventing a PID if the winner died before publication. Non-contention lock errors fail closed. Refuse any live legacy PID regardless of mtime; an unverified/non-daemon holder gets exit 4 and `ps -p <pid> -o command=` hint, not a blind kill. Refresh PID lock every ≤15 s independent of dispatch; a lost/foreign lock disables dispatch and surfaces ownership_lost, never silently recreated.
- **Environment:** before runtime start, construct a sanitized immutable snapshot excluding ORCH_AGENT_ID, ORCH_AGENT_NAME, ORCH_TASK_ID, ORCH_TUI_CLIENT, ORCH_WEB_PASSWORD, ORCH_SOCKET, ORCH_PROJECT_ROOT, DEBUG, PWDEBUG, and NODE_OPTIONS when its value contains `--inspect`. Every agent spawn uses env_clear + sanitized snapshot + existing configured env + explicit identity/project/socket inserts. No runtime set_var/remove_var. Requests have no per-call env/PATH/cwd/argv/adapter-command execution overrides; existing bounded authored config/agent documents retain their intentional configuration semantics and are not a new execution-override DTO. Owner credentials come from the daemon-start snapshot; shell credential refresh requires explicit daemon restart. Status lists removed names, never values.
- **Logs/errors:** structured op/id/error code/execution class/timing only (kernel pid/run attribution for control); never err.message, serde error Display, frames, prompt/body/env/attachment bytes, at any log level. Parse diagnostics are fixed category/byte offset. Service error message goes only to requesting client with terminal escaping. `.orchestry/daemon.log` mode 0600 rotates at 10 MiB with one 0600 backup (20 MiB total); open without following symlinks, validate ownership/type before use. Logging failure is visible and does not enable payload dumps.
- **Authority:** same UID grants full owner capability, including to containers/sandboxes that expose the socket, and remote principals admitted by web or peer task submission. Do not expose `.orchestry` or ORCH_SOCKET to an environment untrusted as the owner. Daemon-side mode checking protects MCP mapping mistakes, not a hostile same-uid client opening a different session. Attachments use the minimum SE-10 contract: CLI/TUI absolutize against caller cwd, MCP against selected project root; daemon rejects relative source paths, retains limits/destination confinement. A confined caller can still ask the daemon to read a host file it cannot read itself (M3 deputy residual). General attachment upload is not selected: G3 transfers currently cover typed documents/image staging, and a general upload changes attachment DTO, staging lifetime and size semantics rather than coming at zero protocol cost.
- **Review:** security must verify these conditions on the implementation, including the restrictive test-root guard and both OS credential paths. Documentation checks here do not constitute that gate.

## Normative recovery table

This section is the common reference for README upgrading, RUNBOOK, manual chapter 18 and agent instructions (R35). No row authorizes a client to start, restart or repair a daemon.

| Trigger/caller | Required observable behavior and state consequence | Explicit owner/caller action and acceptance |
|---|---|---|
| Agent mid-task loses daemon | Exit 7 is infrastructure failure: agent stops coordinated work and reports to owner; never starts daemon. Lost mutation reply means unknown outcome. Shell set -e may exit, but orch cannot force a containing AI session to stop. | Owner restarts explicitly, inspects task/run/current state before retry; conversation is not resumed by restart. `agent_calls_across_daemon_death` uses fake shell agent, including sent-but-reply-lost mutation. |
| Explicit restart after crash | Default OFF; clear stale claims; dead local running entries become cancelled task/run and idle agent; orphan preparing runs cancelled. Live recorded local entries retain recovery reservations; no conversation/collector resurrection is promised. Orphan running records without an entry follow existing cleanup; remote registered entries stay Unknown pending sync, removed peers cancel; unverified worker mirrors stay quarantined with outcome unknown. | Preserve existing recovery.rs semantics and diagnostics, inspect before manual retry/on; deterministic fixtures cover each state and no duplicate child spawn. Startup must not advertise READY with unresolved fatal ownership/recovery errors. |
| MCP initialize first fails | Central initialize error, bridge alive; no guarantee every external MCP client retries automatically. | Owner starts daemon; client reinitializes or user reconnects/re-enables/restarts the MCP session. Automated `mcp_initialize_fails_then_recovers`; R28 records actual Claude Code action. |
| Script bootstrap or cron | init is idempotent and non-starting; state commands fail visibly if dependency unavailable. | Owner-authored bootstrap explicitly starts then checks readiness/status; cron exits nonzero preserving stderr. Examples below, `bootstrap_and_cron_fail_visibly`. No supplied launchd/KeepAlive/watchdog. |
| Foreground pane closes | SIGHUP is clean stop, same as stop/SIGINT/SIGTERM; kill agents with existing grace, reconcile, release ownership last. Other panes lose their shared service. | Owner opens a new pane and explicitly starts; `foreground_sighup_stops`, signal lifecycle tests. |
| Detached pane closes | --detach creates its own session (setsid); stdin /dev/null, stdout/stderr private capped .orchestry/daemon.log; readiness reaches parent only after publication. Survives terminal/tmux pane close, no logout/reboot survival guarantee. | Owner explicitly starts again after logout/reboot or process loss. `detached_survives_tmux_kill_pane`; no auto-restart supplied. |
| Binary/protocol skew | Ordinary calls fail before operation decode; same-uid/project-checked hello stop intent works with either version. Clean stop cancels/reconciles active work, not a transparent upgrade. | Preferred order: stop with either binary, replace executable, start explicitly. `stop_crosses_version_skew`, including removed old executable. |
| Daemon hung or endpoint removed | Deadline/error; status never claims liveness from metadata and clients do not repair. | For a hung daemon only: inspect daemon.json PID as an untrusted candidate; `ps -p <pid> -o command=` must independently show orch daemon for this root before owner sends `kill -TERM <pid>`. If root/process cannot be verified, do not signal. No automated PID fallback. |
| Crash between entity/index writes | Single writer does not make multi-file writes transactional; startup read_index rebuilds missing/corrupt indexes, cannot detect every valid-but-stale index. | Legacy offline repair only: stop, remove affected _index.json, start. Never remove while running; `startup_rebuilds_missing_index`, `valid_stale_index_requires_offline_repair`. |
| Upgrade from legacy writers | PID heartbeat protects against normal idle-age theft, not uncooperative writers or a suspended owner whose heartbeat cannot run. Old MCP servers can write without locks. | Stop every old orch process including MCP held by IDE/Claude sessions; inspect `pgrep -fl orch`, which must show no old process before first start. Restart those sessions on the new client build. |
| Re-run init/help/doctor | init does not overwrite existing state and prints start hint; help/version need no daemon; doctor is daemon client. | NoDaemon doctor shows discovery-only diagnosis and exit 7; `init_rerun_is_idempotent`, `offline_help_and_doctor_contract`. |

Owner-authored bootstrap (explicit lifecycle, not client auto-start):

```sh
set -eu
orch init
orch daemon start --detach
orch daemon status  # detach returns success only after readiness
orch task list
```

Cron command with visible failure, not a daemon launcher:

```sh
cd /absolute/project || exit 1
orch serve --once || { rc=$?; echo "orch batch failed (exit $rc); owner action required" >&2; exit "$rc"; }
```

## Verification environment

Test engineer owns automated daemon/protocol/state/concurrency tests; CLI test owner migrates binary/MCP fixtures; qa-engineer owns manual acceptance; release engineer runs the CI set and cold gate at a frozen integration commit. All tests use temp projects and explicit short isolated ORCH_SOCKET, fake shell agents and isolated ORCHESTRY_GLOBAL_CONFIG_PATH. Pure engine/domain/TUI tests retain in-process fakes. Do not use production `.orchestry` or real AI CLIs in automated tests.

Linux CI on every PR covers Unix sockets, Linux SO_PEERCRED, file modes, subprocess/signal lifecycle, existing suites and new concurrency/no-daemon matrix. macOS local verification covers the corresponding peer credential implementation and short-path boundary; cold `./xtask/gate.sh` runs on macOS. An injected different-UID case is not evidence of a real second-UID run: mark actual cross-user checks env-limited if unavailable and obtain owner disposition.

TUI acceptance uses `tmux -L <unique-socket> -f /dev/null` with temp project, pinned terminal type, bounded polling, capture-pane and cleanup guard. r/q, disconnected state, reconnect after explicit owner start and push updates require real binary tmux tests, not FakeBackend alone. Process-level SIGHUP remains portable-pty as existing tests require. Native terminal profiles, native clipboard and actual browser rendering are not covered by tmux; qa-engineer records owner-Mac results. A real Claude Code MCP session needs the owner's Mac/account and explicit test-project permission; no automated test launches it. R28 cannot be called pass from a simulated MCP transport alone.

Manual sequence: owner explicitly starts the test daemon; QA attaches CLI, TUI, web and MCP; records `pgrep -fl orch` distinguishing one daemon from multiple clients; owner/authorized QA kills that specific test daemon; verify all client channels show NoDaemon, no new daemon child appears, and explicit owner start alone restores service. No permission to kill a real project process is inferred.

## Open questions

- **Gate 2 / Q2 refinement:** owner confirms the version-invariant, same-uid/project-checked hello stop intent in R6/R14 (RT2/G1/SE-8), including its normal active-agent shutdown consequences. Other operations remain strict; no rejected ruling is outstanding.
- **Gate 2 / Q5 regrouping:** owner confirms the first production cutover moves CLI, MCP, TUI and web together in PR 2; PR 1 alone is developmental, PR 3 adds push, PR 4 enforces/cleans up. The larger PR 2 is required to avoid a supported mixed-writer head.

G2 guard, G3 bounds/batch policy and the remaining lead-accepted refinements are incorporated into this draft for the owner's overall requirements approval, not unanswered engineering alternatives. R39 remains should-hardening for the implementation review, not a new owner decision. No package version bump, release or merge is authorized by this revision.

## Verification

Not run: this is the requirements draft. The completing PR fills each mandatory row with actual commit and platform evidence; blank is not pass. W rows are scope decisions, verified by the corresponding exclusion gates and review. A1/A1b document checks are not implementation verification. R39 is a should row; a deferral needs an explicit recorded reason.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | | | |
| R2 | | | |
| R3 | | | |
| R4 | | | |
| R5 | | | |
| R6 | | | |
| R7 | | | |
| R8 | | | |
| R9 | | | |
| R10 | | | |
| R11 | | | |
| R12 | | | |
| R13 | | | |
| R14 | | | |
| R15 | | | |
| R16 | | | |
| R17 | | | |
| R18 | | | |
| R19 | | | |
| R20 | | | |
| R21 | | | |
| R22 | | | |
| R23 | | | |
| R24 | | | |
| R25 | | | |
| R26 | | | |
| R27 | | | |
| R28 | | | |
| R29 | | | |
| R30 | | | |
| R31 | | | |
| R32 | | | |
| R33 | | | |
| R34 | | | |
| R35 | | | |
| R36 | | | |
| R37 | | | |
| R38 | | | |
| R39 | | | |

Defects found outside the requirements:
- No new unrelated defect scope. The PID-lock and mismatched-stop defects are covered by R1/R6/R14/R37; B0 F1 fix/re-review remains a separate lead integration prerequisite, not verified by this revision.
- Global preference writes by multiple project daemons retain existing cross-project constraints; this goal is not a per-user ownership service.

## Protocol and service reference (proposed)

Normal CLI/TUI/web sessions retain the existing local-owner operation set. MCP normal/read_only/allow_destructive sessions retain the existing tool catalogue restrictions; do not apply MCP tool omissions to CLI commands. Same-UID identity remains attribution.

This appendix keeps the proposed wire contract in the repository, not only in the team's scratch design. It becomes normative when the owner approves the spec. Operation/argument meanings remain the existing service inputs; no generic shell or store escape is allowed.


LOCAL_PROTOCOL initially 1 (revision rule in Constraints); binary is the package version, unchanged by this goal. Frozen hello/welcome/reject tolerate unknown fields (but reject duplicate known fields/invalid types), ≤16 KiB; all other frames reject unknown fields. NDJSON UTF-8 JSON objects, ≤1,048,576 bytes excluding LF, exactly one LF terminator; empty, duplicate-field, unknown-tag, trailing/unterminated and invalid-type frames rejected. Dedicated reader owns partial-line state. Pre-parse scan enforces depth ≤64; do not rely on serde_json recursion defaults. Request DTOs use deny_unknown_fields, no flatten and no untyped Value. Request ids are nonzero u64 scoped to a connection, never reused while outstanding. Exactly one terminal `ok` or `err` per accepted id; `part` frames precede it. No arbitrary shell or storage-path RPC.

| Direction | Frame | Contract |
|---|---|---|
| C→D first | `{"hello":{"protocol":1,"binary":"<semver>","project_root":<path>,"project_id":{"dev":1,"ino":2},"intent":"session","caller":{"kind":"cli\|tui\|web\|mcp\|agent","agent_id":null,"pid":123},"mode":"normal\|read_only\|allow_destructive"}}` | Frozen envelope; intent is session (default) or stop. Client resolves `--agent` over ORCH_AGENT_ID and per-tool defaults. Identity fixed for session, ≤256-byte strings; claimed pid is never trusted. |
| D→C first | `{"welcome":{"protocol":1,"binary":"<semver>","project_root":<path>,"project_id":{"dev":1,"ino":2},"instance":"<id>","pid":123,"compatible":true}}` | Same-euid/project checked first. Ordinary skew sends compatible=false then closes before request decode; stop uses the frozen completion procedure below. |
| D→C pre-session | `{"reject":{"code":"handshake_failed","message":"<safe reason>"}}` | Frozen codes include handshake_failed, wrong_project, busy and stop_failed; bounded safe fields, no payload echo. busy includes sessions/cap; UID denial may close without bytes. |
| C→D | `{"id":1,"req":{"op":"task.get","args":{"id":"tsk_…"}}}` | Tagged typed enum with no per-request caller; effective identity/mode fixed in hello. Explicit per-tool from/agent fields retain existing override rules, resolved client-side. |
| D→C terminal | `{"id":1,"ok":<typed result>,"commit_seq":42}` | Existing domain/projection values; commit_seq is the in-memory write_seq requested by T0: monotonic per daemon instance, assigned to state commits; reads return the observed ordinal. It is not a stored revision. Null for void. |
| D→C terminal | `{"id":1,"err":{"code":"invalid_arguments","message":"…","exit_code":2,"execution":"not_executed\|completed\|unknown"}}` | Typed safe error categories preserve exits 1–6 and add 7. Failures after partial storage work use unknown; never imply rollback. |
| D→C streaming | `{"id":1,"part":{"n":0,"kind":"log\|event\|result_chunk","data":…}}` | Increasing n per request. Large JSON results serialize once as UTF-8 bytes and stream base64 chunks (≤256 KiB raw) then `ok:{"streamed":true,"encoding":"json_utf8","bytes":n,"sha256":"…"}`. Client reconstructs unchanged result; incomplete streams are errors, not success. |
| C→D subscription | `{"id":2,"req":{"op":"subscribe","args":{"stores":["tasks"],"filter":{},"bus":true}}}` | Returns `{subscription,instance,seq}` before any ev for that subscription. Filter is typed optional ids/goal/agent, not executable predicate. |
| D→C push | `{"ev":{"subscription":3,"instance":"…","seq":1,"store":"tasks","id":"tsk_…","kind":"updated","commit_seq":42}}}` | store change, kind created/updated/deleted; coarsely invalidates client cache. |
| D→C push | `{"ev":{"subscription":3,"instance":"…","seq":2,"bus":<OrchestratorEvent DTO>}}` | Domain event payload schema pinned by proto, no engine types. Large progress may use a truncation marker; state invalidations cannot silently disappear. |
| D→C push | `{"ev":{"subscription":3,"instance":"…","seq":4,"resync":{"reason":"lagged"}}}` | Explicit gap marker; refetch all subscribed views. If marker cannot be queued, close session so reconnect requires resync. |
| C→D control | `{"id":3,"req":{"op":"unsubscribe","args":{"subscription":3}}}` | Idempotent, returns null; frees queues. |
| C→D control | `{"id":4,"req":{"op":"request.cancel","args":{"id":1}}}` | Stops stream observation/queued work only; never silently cancels a running agent. Use task.cancel for that. Running state writes finish and return their result if possible. |

Large inbound arguments cannot be silently dropped by peer's oversize policy. `transfer.begin {purpose,total_bytes}` → opaque connection-local token; `transfer.chunk {token,n,data_base64}` → null; `transfer.finish {token,sha256}` → sealed token; typed request's `input_transfer` consumes it once. Chunk ≤256 KiB raw, total ≤16 MiB, one active transfer per connection, 10 s inactivity timeout, 64 MiB aggregate staged bytes, stored only in daemon-owned temporary files cleaned on disconnect/startup. Actual operation still enforces existing limits (definition document ≤1 MiB/depth64); transfer is transport, not a new generic file write. Use transfers only for envelope expansion, large strings or clipboard image bytes. Attachment source paths are absolutized at the client (CLI/TUI cwd, MCP project root); daemon rejects relative sources. The M3 deputy residual is explicit in the checklist; general upload is not added. Result streaming avoids whole-wire allocations, but existing service vectors may still allocate; do not claim unlimited query scalability. A payload beyond the explicit local aggregate bound returns an error; gate 2 records this transport bound rather than truncating results.

Connection/admission limits (G3/SE refinements; numeric defaults pinned here for owner approval): separate 16 pre-handshake slots (2 s lifetime), 64 ordinary sessions, 16 in-flight ids per connection, 128 ordinary queued state operations and 64 MiB encoded queued-request bytes per project, 10 s admission deadline, 8 subscriptions/connection, 256 queued events/subscription with total queued-byte cap 4 MiB/connection. Readers stop admitting before memory grows beyond caps; reject overloaded work as not_executed. Keep a reserved internal/control lane so stop/cancel cannot be starved. Writes time out after 5 s; handshake 2 s; connect 1 s. Long-running streams have no total run deadline but do have bounded write/inactivity handling. Doctor retains two admitted jobs and existing 120 s deadline outside the state permit.

### Frozen stop intent

`hello.intent` is `session` (default) or `stop`; the field and enum semantics are frozen. The client authenticates server uid before writing; daemon authenticates peer uid and compares project_id before considering stop. A stop hello decodes only this bounded envelope, never a request payload. It bypasses protocol/binary equality only for normal shutdown; version numbers are still returned for diagnostics. Stop grants no new same-uid authority compared with SIGTERM and avoids a manually selected PID (SE-8); RT2's upgraded/removed-old-binary failure is thereby resolved.

The daemon sends frozen `welcome` with `stop_state:"accepted"` within the hello deadline, marks STOPPING and runs R14 cleanup; after cleanup it sends the same envelope with `stop_state:"completed"` before close/release. These optional fields and their two values are frozen from LOCAL_PROTOCOL 1. Cleanup failure uses frozen `reject` code `stop_failed`; no protocol-specific result decoding is required. Acceptance is not completion: EOF/deadline after accepted without completed means unknown outcome. Client completion wait is bounded at 30 s by default (distinct from the 2 s hello deadline); timeout never triggers a kill/start/replay. Compatible `daemon.stop` RPC may use the ordinary result path but reaches the same shutdown handler. Read-only MCP has no stop tool; this owner control never broadens its published catalogue. R39 may additionally require explicit from-agent acknowledgement from recognized run pgids.

### Complete operation catalogue

This table maps today's public service/entry-point surface, including direct store helpers. Each comma-separated name is a distinct typed request; no generic service invocation. Arguments retain current service input structs/CLI meanings, translated to protocol DTOs when those structs live in engine/core/CLI. Return serialized domain values unchanged; frontend-only printing/projection stays at the edge. RPC results for mutations include the updated entity where MCP already needs it, computed within the same operation. Internal-only create/start/finish run and agent stats functions are NOT offered to arbitrary clients; they remain coordinator actions from the orchestrator.

| Requests | Existing source / callers / input → result |
|---|---|
| `status.get`, `state.get`, `ui.bootstrap` | status command and TUI LaunchContext: status summary, OrchestratorState, project/version/defaults/warnings/preferences. No store paths leak as capabilities. |
| `task.create`, `task.list`, `task.get` | TaskService create/list/get; CLI task add/list/show, MCP task_add/list/show, TUI refresh/create; CreateTaskInput/filter/id → Task(s). |
| `task.assign`, `task.cancel`, `task.approve`, `task.review_complete`, `task.reject`, `task.retry`, `task.delete`, `task.update_fields` | Service inputs id/agent, id/feedback, field patches; `task.approve` preserves CLI/MCP hold-release-or-review semantics; TUI uses `task.review_complete` for its existing direct review→done transition. Cancel uses R9–R12/R30 orchestrator path. Delete currently TUI-only; no new MCP tool. |
| `task.definition.get`, `task.definition.apply` | CLI edit/update/patch and MCP task_update/patch: authored raw document with opaque base digest; apply update/patch + optional expected digest → Task. Validate writable keys in daemon. |
| `task.attachment.read`, `attachment.stage_image` | Existing attachment viewing and TUI paste image; validated task/filename → chunks, bytes → daemon-owned temporary attachment reference. Source paths follow R34; daemon file authority differs from confined caller authority, as disclosed in the security checklist. |
| `agent.create`, `agent.list`, `agent.get`, `agent.update_fields`, `agent.remove`, `agent.enable`, `agent.disable`, `agent.autonomous`, `agent.stop` | AgentService and live force_stop_agent; CLI/MCP/TUI. list/get include qualified remote IDs as today; creation/update returns authored diagnostics for frontend to render. |
| `agent.definition.get`, `agent.definition.apply` | agent edit/update/patch raw authored fields + optional base digest; current flags, model/effort/prompt-mode validation and warnings preserved. |
| `goal.create`, `goal.list`, `goal.get`, `goal.update`, `goal.status`, `goal.delete`, `goal.tasks`, `goal.progress` | GoalService all public operations; status args id/status/force. `goal.get` projection may combine task list under the permit. Paused path uses R9–R12/R30. |
| `message.send`, `message.broadcast`, `message.inbox`, `message.list`, `message.mark_read` | MessageService and inbox marking; from/to/subject/body/reply_to, agent id/filter → message(s). Broadcast expands recipients daemon-side. Qualified remote targets use existing bridge, not a client TCP connection. |
| `context.set`, `context.get`, `context.list`, `context.delete`, `maintenance.expire_context` | ContextStore input key/value/expiry → existing ContextEntry/value projection. Client reads never invoke expire. `maintenance.expire_context` is an explicit owner admin op (no new MCP tool); timer calls the same coordinated handler. |
| `team.create`, `team.list`, `team.get`, `team.join`, `team.leave`, `team.add_task`, `team.remove_task`, `team.set_lead`, `team.disband`, `team.for_agent` | TeamService methods; remove_task/for_agent serve existing backend helpers where used; no extra MCP catalogue entries. |
| `config.get`, `config.set`, `config.definition.get`, `config.definition.apply` | CLI/MCP config operations, raw key/value/document/mutation kind → current projections, refresh R9–R12/R30. |
| `config.global.get`, `config.global.set`, `config.global.show`, `ui.preferences.patch` | config global and TUI palette/activity/notifications; field patches, never client read-whole/save-whole. Machine-global store still has existing cross-project semantics; daemon ownership is per project, not a claim of global single writer. |
| `config.prompt_mode.get`, `config.prompt_mode.set`, `config.prompt_mode.toggle`, `state.complete_onboarding` | TUI raw-config and state-store helpers; return mode/state. Toggle resolves latest value inside coordinator. |
| `run.list`, `run.get`, `run.events`, `run.events_tail`, `run.history`, `run.timings`, `logs.follow` | RunService list_all/get/list_for_task/list_for_agent/read_events/read_events_tail and CLI logs filters/since; run id or task/agent filter → existing Run/RunEvent/history/timing values. Follow is part stream; reads from daemon only. |
| `dispatch.set`, `dispatch.tick`, `dispatch.run_task`, `dispatch.run_all`, `dispatch.until_terminal` | All current run/serve/TUI control paths; the dispatch table below. Runs launch in daemon; stream output, terminal result contains exit status. |
| `daemon.status`, `daemon.stop` | Live process metadata/clean shutdown; status returns pid/socket/protocol/binary/instance/executable/started_at/dispatch/running tasks/session count and cap/ownership status/removed env names. Stop also uses hello intent across skew. |
| `remote.add`, `remote.remove`, `remote.list`, `remote.status`, `remote.listen` | Current remote command and MCP read subset; alias/address/config operations run beside peer runtime in daemon. Listener toggling only if existing command supports it; serve --listen remains explicit start option. |
| `doctor.run`, `models.catalog`, `catalog.agents`, `catalog.templates` | DoctorService, model discovery and agent shop/template shop; bounded daemon worker for probes/network fetch; TUI polls local completion slot, not files. |
| `org.list`, `org.deploy` | Org listing/deployment via existing validation/provisioning in daemon; source template selector/root/options → current deploy report. Deployment is ordered but not made transactional across files. |
| `org.template.list`, `org.template.show`, `org.template.init`, `org.template.definition.get`, `org.template.definition.apply`, `org.template.agent.list`, `org.template.agent.add`, `org.template.agent.definition.get`, `org.template.agent.definition.apply`, `org.template.agent.delete` | Existing org/template commands incl. user template sources. Preserve config.yml/template file formats, not a new store. Read/edit temp/apply roundtrip for interactive edit. |
| `update.check` | Current update checker returns install instructions/version metadata; this goal does not add binary install or release. No project state read bypass. |
| `subscribe`, `unsubscribe`, `request.cancel`, `transfer.begin`, `transfer.chunk`, `transfer.finish` | Transport controls the frame/subscription reference, not new CLI/MCP tools. |

`init` is the only project-creation exception: narrow daemon bootstrap API, no server started, print `orch daemon start`; re-run is idempotent, never overwrites existing state or mutates a live daemon, and prints the start hint. Clap help/version and pure syntax validation perform no state operation and need no connection. All other commands require a daemon for their stateful work, including commands formerly runnable without a project (global config/catalog/template helpers); make that intentional behaviour change visible in the manual. Keep filesystem reads of explicitly supplied input files outside `.orchestry` at the frontend if necessary; project-authoritative validation and all writes remain daemon-side.

Interactive edit: get authored-only bytes and base digest from daemon, launch editor on a private temporary file outside `.orchestry`, submit modified bytes plus base digest. If authored base changed, fail conflict without overwriting; runtime-owned fields always come from current state. No persisted revision field is added. Never pass a `.orchestry` filename to the editor. The digest prevents this formerly unbounded read/edit/write interval from losing a concurrent authored change.

Catalogue proof in implementation: a table-driven test enumerates clap leaf commands, all 50 MCP tools, all TuiBackend methods and resource URIs; every stateful item maps to a typed op, with explicit pure-render/bootstrap exclusions. Fail on unclassified additions. Read-only backend methods may use cached values populated only by RPC/subscription. No generic `FileRead(path)` or `StoreSave(entity)` escape is permitted.

### Authoritative operation classes and modes

orch-proto owns an exhaustive match from every typed op variant to Read, Mutate, Destructive or Control, with no wildcard/default-allow arm. The table is normative; implementation tests enumerate the complete catalogue and compare coverage. The daemon checks the fixed session kind/mode and tool-subset before admission, independently of bridge filtering. `Control` is not blanket permission: the separate control allowlist below applies.

| Class | Exact operations |
|---|---|
| Read | `status.get`, `state.get`, `ui.bootstrap`, `task.list`, `task.get`, `task.definition.get`, `task.attachment.read`, `agent.list`, `agent.get`, `agent.definition.get`, `goal.list`, `goal.get`, `goal.tasks`, `goal.progress`, `message.inbox`, `message.list`, `context.get`, `context.list`, `team.list`, `team.get`, `team.for_agent`, `config.get`, `config.definition.get`, `config.global.get`, `config.global.show`, `config.prompt_mode.get`, `run.list`, `run.get`, `run.events`, `run.events_tail`, `run.history`, `run.timings`, `logs.follow`, `daemon.status`, `remote.list`, `remote.status`, `doctor.run`, `models.catalog`, `catalog.agents`, `catalog.templates`, `org.list`, `org.template.list`, `org.template.show`, `org.template.definition.get`, `org.template.agent.list`, `org.template.agent.definition.get`, `update.check` |
| Mutate | `task.create`, `task.assign`, `task.approve`, `task.review_complete`, `task.reject`, `task.retry`, `task.update_fields`, `task.definition.apply`, `attachment.stage_image`, `agent.create`, `agent.update_fields`, `agent.enable`, `agent.disable`, `agent.autonomous`, `agent.definition.apply`, `goal.create`, `goal.update`, `goal.status`, `message.send`, `message.broadcast`, `message.mark_read`, `context.set`, `team.create`, `team.join`, `team.leave`, `team.add_task`, `team.remove_task`, `team.set_lead`, `config.set`, `config.definition.apply`, `config.global.set`, `ui.preferences.patch`, `config.prompt_mode.set`, `config.prompt_mode.toggle`, `state.complete_onboarding`, `remote.add`, `remote.listen`, `org.deploy`, `org.template.init`, `org.template.definition.apply`, `org.template.agent.add`, `org.template.agent.definition.apply` |
| Destructive | `task.cancel`, `task.delete`, `agent.remove`, `agent.stop`, `goal.delete`, `context.delete`, `team.disband`, `remote.remove`, `org.template.agent.delete`, `maintenance.expire_context` |
| Control | `dispatch.set`, `dispatch.tick`, `dispatch.run_task`, `dispatch.run_all`, `dispatch.until_terminal`, `daemon.stop`, `subscribe`, `unsubscribe`, `request.cancel`, `transfer.begin`, `transfer.chunk`, `transfer.finish` |

CLI/TUI/web/agent owner sessions admit the existing local-owner operations. MCP read_only admits only its existing Read tool/resource subset plus observation controls subscribe/unsubscribe/request.cancel. MCP normal admits its existing Read + Mutate subset; allow_destructive additionally admits the existing five destructive tools (task_cancel, agent_remove, goal_delete, context_delete, team_disband). Transport transfer controls require a purpose allowed for that session and cannot bypass the consuming op's check; read_only has no inbound mutating transfers. No MCP mode gains dispatch/daemon lifecycle/maintenance or a new tool. Argument-dependent policy also remains daemon-side: goal.status with force=true requires MCP allow_destructive even though the op is Mutate; raw-client tests cover this and other existing validation predicates. Mutation-shaped task approvals retain existing mode classifications; explicit read-only raw task.cancel must return not_executed. `message.inbox` stays Read only when it does not mark messages; marking uses the separately classified message.mark_read operation. Caller defaults are resolved client-side so CLI fallback and MCP no-silent-fallback do not drift.

### Dispatch and command mapping (Q1)

| Command | Meaning |
|---|---|
| `daemon start [--detach] [--dispatch on\|off] [--from-agent]` | Explicit owner start; foreground unless detach; OFF default; agent-context refusal unless override. |
| `serve`, `run --watch` | Explicit foreground daemon start, dispatch ON. If another daemon exists, exit 4 + `Use: orch daemon dispatch on`. |
| `daemon dispatch on\|off` | RPC toggles future dispatch. OFF does not kill existing runs, drop ownership or stop expiry/peer processing. |
| `run --once` | RPC one scheduling tick, stream that tick's events and report it; does not stop daemon or permanently toggle dispatch. Add this flag if absent at base. |
| `run <id>` | RPC explicit task launch, observe until terminal. Disconnecting observer does not kill the run. Normal eligibility/capacity/cancel checks still apply. |
| `run --all` | RPC finite current eligible-task run batch, preserve current result semantics; no second engine. |
| `serve --once` | RPC dispatch.until_terminal on existing daemon, requires global OFF; follows batch policy below and returns all_done/has_failed/interrupted, never stops daemon or agents on disconnect. |
| TUI start/stop watch | `dispatch.set on/off`; no process lifecycle operation. |
| `daemon status`, `daemon stop` | Live status; stop uses frozen hello intent for skew recovery, same R14 cleanup. |
| `init` | Creates project or idempotently reports existing project; prints explicit-start hint, never starts. |
| `tui --observe`, `tui --listen` | Removed; dashboard is always a client. Peer --listen is a daemon start option. |
| `doctor`, `--help`, `--version` | doctor is a client with discovery-only NoDaemon diagnosis; help/version stay offline. |

Batch policy (RT5/G3): reject `serve --once` while global dispatch is ON with `dispatch is ON; serve --once needs dispatch OFF` (invalid arguments, exit 2). With OFF, admit one batch at a time (a second gets an explicit conflict, exit 4); batch ticks have scheduling rights while global OFF and suppress autonomous seeding for every tick, including explicitly requested ticks during the batch. Track existing non-terminal tasks and tasks becoming eligible/created at any batch tick, including dependent handoffs; reevaluate until no non-terminal task remains, matching today's all-task wait. Blocked/unassigned work can therefore keep a batch pending; cancellation/interruption is explicit, never silent success. Concurrent explicit run/tick requests still serialize/recheck eligibility, and their resulting tasks participate in this all-task terminal check.

Batch never changes the global switch. A later explicit dispatch ON atomically ends batch mode and hands scheduling to persistent dispatch; the batch observer returns interrupted (existing interrupted exit semantics), running agents continue. A finishing/disconnected batch never restores OFF over that later command. An explicit dispatch OFF while a batch is active ends that batch too (the later explicit scheduling command wins); future batch ticks stop, running agents continue. Client disconnect or request.cancel ends batch scheduling without killing agents or changing global mode. This differs deliberately from old run_once's unconditional orchestrator stop. Test the two-pane OFF → batch → ON → batch finish/disconnect sequence and the initial-ON rejection; include tasks created during batch and blocked dependencies. Successful start/status and TUI/web headers show dispatch state prominently; `serve` against an existing OFF daemon exits 4 with the toggle hint rather than appearing to resume work.

All dispatch entrances use one coordinator; concurrent manual ticks serialize and recheck eligibility, with no duplicate launches. Dispatch OFF inhibits automatic work only; explicit tick/run commands are deliberate execution requests. Peer listener flags are start options. `serve --listen` with a live owner returns exit 4 and an explicit owner stop/serve hint. Do not silently add a listener or launch a second process. Keep log format/tick interval options at their established command boundary, forwarding execution settings to daemon operations and rendering output at the client.

### Push, gaps and snapshot ordering

Subscribe to tasks/goals/agents/messages/context and optionally teams/runs/config/state/remote plus the domain event bus. Per-subscription seq starts at 1 and increments only for delivered-or-dropped events matching that subscription, so filters do not create fake gaps. Instance identifies daemon lifetime; seq is never durable. Assign commit_seq at the shared coordinator, and enqueue change notifications only after state publication. Engine-only, peer and expiry changes participate, not just RPC writes. Coarse events contain entity identity and action, not secret-bearing entity contents.

Initial sync: register subscription under coordinator and obtain baseline commit_seq; acknowledge; buffer pushed events; fetch views via normal reads; apply/refetch invalidations after that baseline, then continue. Never fetch first and subscribe second. A gap, explicit resync marker, changed instance or lost connection invalidates every subscribed cache. Client resubscribes and refetches; no durable replay promise. Multiple read calls need not be one snapshot, but each carries commit_seq; buffered invalidations prevent a missed update. `ui.bootstrap` and status aggregates use one coordinator operation where a consistent view is needed.

Final PR 3 TUI polls only its in-memory event receiver, not disk. In PR 2 it temporarily refreshes over RPC on the existing tick, with no file observer or local stores. Web preserves existing PTY design: its `orch tui` child uses the same socket subscriptions; web parent also holds a lightweight connection to report outage before creating sessions. Web auth, Host/Origin controls and PTY limits remain. MCP resources/subscribe tracks requested `orch://` URIs and sends `notifications/resources/updated`; collection URIs invalidate on any member change, item URIs on relevant id changes, status on task/agent/run/dispatch changes, inbox on relevant message changes. Gaps invalidate all subscribed resources; callers refetch. Remove redundant MCP store FIFO; preserve bounded bridge admission and doctor limits without a second serialization lock.
