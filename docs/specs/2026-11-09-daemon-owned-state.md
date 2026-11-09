# Daemon-owned state

status: draft
owner: architect; requirements approval: owner at gate 2
date: 2026-11-09
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
| R1 | One daemon per canonical project shall own all store access and agent execution, holding project ownership for its entire lifetime including dispatch OFF. | item 1, 2, 14, 15 | must | `crates/orch-daemon/tests/daemon_lifecycle.rs::two_explicit_starts_one_owner`; `live_owner_old_mtime_not_evicted`; macOS + Linux CI |
| R2 | The daemon shall use a Unix socket with same-owner parent 0700/socket 0600, `.orchestry/orch.sock` at ≤100 absolute path bytes, otherwise the approved TMPDIR/hash path, plus explicit ORCH_SOCKET override and daemon.json 0600. Reject unsafe/too-long final paths instead of truncating. | item 1; plan Q3 | must | `crates/orch-daemon/tests/daemon_paths.rs::long_root_override_and_wrong_project`; `daemon_security.rs::uid_permissions_and_symlinks`; macOS + Linux CI |
| R3 | All stateful CLI, TUI, web, MCP and agent-invoked operations shall use orch-client exclusively; only init and explicit daemon-start entry points may reach the narrow orch-daemon bootstrap API. Clients may read discovery metadata, never entity/index/config/state/run files. | item 2, 15 | must | `cargo xtask crate-graph`; `crates/orch-cli/tests/daemon_contract.rs::all_surfaces_mapped`; forbidden dependency/source fixtures; macOS + Linux CI |
| R4 | Every client shall report the uniform NoDaemon text and code 7 on absent/refused/stale endpoints or failed handshake; client connection code shall neither mutate discovery/socket files nor create a daemon process. | item 3, 13, 15, 20 | must | `crates/orch-cli/tests/daemon_no_daemon.rs::all_frontends_matrix`; absent/stale/refused/handshake fixtures with process-table and spawn assertions; macOS + Linux CI |
| R5 | Explicit `daemon start` shall be foreground by default, support --detach and dispatch OFF by default; `serve` and `run --watch` explicitly start foreground with dispatch ON. Racing starts shall produce one owner and exit 4 for the loser with winner PID. Init shall create a project and print the explicit-start command without starting it. | item 3, 6, 13; plan Q1 | must | `daemon_lifecycle.rs::two_explicit_starts_one_owner`; `crates/orch-cli/tests/daemon_dispatch.rs::mapping_and_disconnect`; `init_never_starts_daemon`; macOS + Linux CI |
| R6 | Handshake shall carry protocol u32, exact package semver, canonical project identity and caller identity. Both ends reject either-direction protocol or binary mismatch before requests, with both versions in NoDaemon and explicit owner stop/start instructions. | item 4, 13, 20; plan Q2 | must | `crates/orch-daemon/tests/daemon_protocol.rs::strict_skew_both_directions`; `wrong_project_is_rejected`; macOS + Linux CI; skew-stop procedure remains gate-2 question G1 |
| R7 | Reuse the bounded 1 MiB NDJSON framing from orch-peer through orch-proto; local requests use typed DTOs, request ids, terminal ok/err, part streams and ev notifications. Invalid/oversize frames shall not mutate state or silently disappear. | item 4, 10 | must | `crates/orch-daemon/tests/daemon_protocol.rs::frame_catalogue_and_bounds`; `cargo test -p orch-peer`; macOS + Linux CI |
| R8 | Caller identity shall preserve explicit flag over ORCH_AGENT_ID and existing per-call sender/inbox rules; kernel same-euid peer credentials, not asserted identity, shall be the authority boundary. | item 4, 9, 11 | must | `crates/orch-cli/tests/daemon_contract.rs::identity_modes_and_projections`; `daemon_security.rs::uid_permissions_and_symlinks`; macOS + Linux CI |
| R9 | Pausing a goal shall order cancellation decisions with dispatch/spawn so a cancelled pending task cannot start afterward; unrelated goals and current goal-scoped pause behaviour shall be preserved. | item 5, 21 | must | `crates/orch-engine/tests/daemon_engine.rs::pause_prevents_post_decision_spawn`; barrier-controlled dispatch race; macOS + Linux CI |
| R10 | Task cancellation through every frontend shall reach the daemon live-run registry and stop a local running agent before success; completion races cannot resurrect cancellation or lose other fields. | item 5, 21 | must | `crates/orch-cli/tests/daemon_engine.rs::cancel_kills_live_process`; fake shell adapter through CLI/MCP/backend; macOS + Linux CI |
| R11 | Context expiry shall be an explicit daemon maintenance operation even with dispatch OFF. Reads never purge. Renewals and expiry shall share an operation boundary and recheck expiration before deletion. | item 5, 14, 19 | must | `crates/orch-engine/tests/daemon_engine.rs::reads_do_not_expire_and_renewal_survives`; fake clock plus file-backed test; macOS + Linux CI |
| R12 | The daemon shall serialize complete state read/modify/write transitions across RPC, dispatch, process completions, peer handlers and expiry, preserving disjoint field updates; no lock may span an agent lifetime or socket consumer wait. | item 5, 14, 15, 19 | must | `crates/orch-cli/tests/daemon_stress.rs::all_clients_share_state_order`; reentrant fake agent calls and cancelled collector tests; macOS + Linux CI |
| R13 | Dispatch on/off, one tick, run task and run all shall execute in the existing daemon via RPC and stream output. OFF retains running children, ownership, maintenance and peer handling. Existing serve --once shall retain until-terminal result semantics using the daemon; document distinction from run --once. | item 6; plan Q1 | must | `crates/orch-cli/tests/daemon_dispatch.rs::mapping_and_disconnect`; every command in dispatch table; macOS + Linux CI; serve --once refinement G3 at gate 2 |
| R14 | Status shall query the live daemon; stop over a compatible socket and SIGTERM/SIGINT/SIGHUP shall stop admission/dispatch, kill/reconcile children using existing state.json semantics, finish cleanup and release ownership last. | item 7 | must | `crates/orch-daemon/tests/daemon_lifecycle.rs::signals_reconcile_and_release`; `stop_waits_for_cleanup`; macOS + Linux CI |
| R15 | After crash, the next client shall report NoDaemon with no recovery side effects. Only explicit start may replace a proven stale socket and reconcile claimed/preparing/running state before readiness. | item 3, 7, 13, 20 | must | `crates/orch-cli/tests/daemon_lifecycle.rs::crash_requires_explicit_start`; `stale_socket_replaced_only_on_start`; macOS + Linux CI |
| R16 | Subscriptions shall push coarse tasks/goals/agents/messages/context invalidations and bus events with per-subscription sequence and daemon instance. Gaps, overflow and changed instance require resync, with no directory polling by TUI or web. | item 8 | must | `crates/orch-daemon/tests/daemon_push.rs::subscribe_snapshot_gap_resync`; `crates/orch-cli/tests/tui_daemon.rs::push_updates_without_disk_observer`; macOS + Linux CI |
| R17 | MCP shall bridge stdio to socket with the same tools, projections, --read-only/--allow-destructive and identity contracts; the redundant store FIFO shall be removed, bounded admission retained, and subscribed orch:// resources shall receive resources/updated. | item 8, 9 | must | `cargo test -p orch-cli --test mcp_contract`; `cargo test -p orch-cli --test mcp_tools`; `cargo test -p orch-cli --test mcp_stdio`; `daemon_push.rs::mcp_resource_updates`; macOS + Linux CI |
| R18 | MCP initialize and a tool call without a compatible daemon shall return the central message with structured exit_code 7; stdio process remains available and exits 0 on EOF. TUI displays central message with r retry/q quit (exit7); web preflight exits7, later outage returns HTTP503 and a WebSocket banner. | item 3, 9, 20; plan Q0/Q0b | must | `crates/orch-cli/tests/daemon_no_daemon.rs::all_frontends_matrix`; `tui_daemon.rs::retry_and_quit` in tmux; `web_daemon.rs::startup_and_live_outage`; macOS + Linux CI |
| R19 | Remote TCP Frame schema/PROTOCOL and old-peer tolerance shall stay unchanged; lift only generic framing, retaining peer-specific bounding and optional fields. Local strict semver checks never apply to TCP peers. | item 10 | must | `cargo test -p orch-peer`; `cargo test -p orch-cli --test remote_peers`; legacy 0.5.x/current fixtures in both directions; macOS + Linux CI |
| R20 | Every input shall be validated, with bounded frame/depth/connection/admission/output buffering; unknown operations, malformed ids and unsafe paths cannot mutate state. No request values/secrets shall enter protocol/error logs. | item 11 | must | `daemon_protocol.rs::frame_catalogue_and_bounds`; `daemon_security.rs::logs_omit_payload_canaries`; `daemon_security.rs::overload_keeps_control_available`; macOS + Linux CI |
| R21 | The listener shall never adopt per-agent/request environment. Child adapters receive deliberately constructed existing env plus daemon project/socket/identity routing; DEBUG/PWDEBUG and agent markers cannot alter listener behaviour. Socket/metadata/credential design and implementation require security review. | item 11 | must | `crates/orch-daemon/tests/daemon_security.rs::agent_environment_does_not_configure_listener`; security gate report at frozen commit; macOS + Linux CI |
| R22 | Publish this spec, plan, proposed ADR0012, ARCHITECTURE §2/§3.1, lifecycle/no-daemon/recovery RUNBOOK, daemon manual chapter and changed command chapters, MCP chapter, README upgrade instructions and CHANGELOG Unreleased. Supersede ADR0011 d7/D10 explicitly; no-auto-start reason is the owner wanting to know when a long-lived process starts. | item 12 | must | Fact-check report at frozen PR4 commit covering every named file and command; `git diff --name-only <base>..HEAD`; macOS; source/markdown checks on Linux CI |
| R23 | Runtime discovery/coordination files may be added but existing YAML/index/config.yml/org template/run-event schemas and orch-core storage traits remain unchanged unless separately approved. The concurrency claim shall explicitly exclude crash transactionality/external writers. | item 14, 15, 16 | must | `cargo test -p orch-cli --test yaml_contract`; storage serialization fixtures and peer tests; `daemon_contract.rs::stored_formats_unchanged`; macOS + Linux CI |
| R24 | Every PR shall pass the CI set on macOS and Linux CI; a cold gate shall pass on macOS for lifecycle/layering/security changes. No base failure is reported as a pass. | item 17, 23 | must | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo xtask crate-graph`; `cargo deny check`; cold `./xtask/gate.sh` (macOS), Linux CI each PR |
| R25 | Provide an in-process daemon harness retaining existing engine/domain/fake-TUI unit-test models. Every binary state test and stdio MCP fixture shall explicitly start a test daemon on an isolated short ORCH_SOCKET, with an isolated global config; never touch a real daemon or real ~/.orchestry. | item 18 | must | `crates/orch-daemon/tests/harness.rs::in_process_and_socket_share_dispatcher`; `cargo test -p orch-cli --test yaml_contract`; routing/mcp_stdio suites; cold gate home hygiene; macOS + Linux CI |
| R26 | Run concurrency proof with 4 CLI clients, two MCP clients and TUI/web-equivalent subscriber/client plus dispatch/collector/expiry for N seconds (10 default, 60 acceptance soak). Assert entity/index equality, ordered version markers/disjoint fields, retained renewals, and explicit errors. | item 19 | must | `ORCH_STRESS_SECONDS=60 cargo test -p orch-cli --test daemon_stress all_clients_share_state_order -- --exact`; macOS + Linux CI; test asserts N and operation count in evidence |
| R27 | No-daemon matrix covers CLI read/write, TUI, web, MCP initialize/tool × no endpoint, stale socket and incompatible protocol, with binary-version mismatch both directions too. Assert same message/code channel, no spawn and no unlink; lifecycle includes race, kill mid-run, explicit recovery and stale replacement. | item 20 | must | `cargo test -p orch-cli --test daemon_no_daemon`; `cargo test -p orch-cli --test daemon_lifecycle`; process-table records plus injected spawn sentinel; macOS + Linux CI |
| R28 | QA shall exercise TUI, browser web, Claude Code MCP and CLI against one owner-started test-project daemon on the owner Mac, then kill -9 and verify every client fails and nothing starts until owner action. | item 22 | must | qa-engineer transcript: isolated `tmux -L <unique> -f /dev/null`, `pgrep -fl orch` distinguished by argv, explicit start, `kill -9 <owned-test-daemon-pid>`, per-client failure, explicit recovery; owner Mac only |
| R29 | Deliver four ordered stacked PRs, each green, with final enforcement/no fallback and push landing within this goal. Owner merges; requirements gate 2 precedes daemon implementation. Spec/plan mirrored under the established convention. | item 23, 8, 12 | must | Release/lead handoff with four PR links, base/head chain, macOS checks/Linux CI and owner approvals; `git diff <base>..HEAD -- Cargo.toml CHANGELOG.md`; macOS + GitHub CI |
| R30 | Long-running daemon shall apply configuration/default changes to subsequent operations/dispatch without rebuilding the live process registry; authored interactive edits submit a base digest and fail rather than overwriting a newer authored edit. | item 2, 5, 9, 19 | must | `crates/orch-cli/tests/daemon_contract.rs::config_refresh_and_edit_conflict`; old runtime fields preserved through edit; macOS + Linux CI |
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

- macOS and Linux, pinned repository toolchain; no unsafe hand-written peer credential syscall glue when the existing runtime provides it.
- Existing domain JSON/YAML values, tool names/projections and modes remain; connection/lifecycle semantics change intentionally. Local protocol starts at 1 independently of peer PROTOCOL=1. Binary equality is exact package semver equality, not a content hash.
- Ordinary clients use metadata → connect → UID/hello → version check → RPC. No client ever removes stale files or attempts ownership. A lost response after sending a mutation has unknown outcome; do not replay automatically.
- New daemon defaults to dispatch OFF. Stop and signals perform existing kill/reconcile semantics; switching dispatch OFF does not stop existing agents. `serve`/`run --watch` are explicit foreground starts, not socket-client shortcuts.
- The strict mismatch rule currently makes `daemon stop` from a mismatched binary fail too. G1 must produce a viable owner recovery instruction before approval. The draft does not authorize a pre-handshake stop request.

### Uniform failure contract

Uncolored, centralized text (frontends may style but do not change wording):

```text
  ✕ No orch daemon is running for <project root> (<detail>)
    Start it with: orch daemon start
```

For skew:

```text
  ✕ No orch daemon is running for <project root> (version skew: daemon orch <d> / protocol <dp>, this orch <c> / protocol <cp>)
    Start it with: orch daemon stop, then orch daemon start
```

CLI/web startup exit 7; TUI q exits7 after the same disconnected screen (r reconnects); MCP returns the same text with structured exit_code7 and remains alive, then exits0 on EOF. Web outage after startup uses HTTP503 and an existing-session banner. Existing exits 1 general, 2 invalid arguments, 3 uninitialized, 4 lock conflict, 5 adapter, 6 workspace remain unchanged. Invalid CLI syntax/help are parsed before connecting. Invalid/missing endpoint for a resolved project uses 7, not 3.

### Socket discovery

Canonical raw project-root bytes select `.orchestry/orch.sock`, or `$TMPDIR/orch-<sha256(root)[..16]>/orch.sock` when the default exceeds 100 bytes. Validate the final TMPDIR or explicit absolute ORCH_SOCKET path also fits; if not, explicit start fails with a short-override instruction. `daemon.json` 0600 records schema, root, socket, pid, binary, protocol, started_at and instance. Default clients read it once; an explicit override chooses the endpoint but does not bypass UID/project/version checks. Lossless tagged-byte encoding handles non-UTF8 paths only in new metadata/transport. Existing file formats remain unchanged.

Security checklist:
- **Default bind and exposure:** Unix socket only, 0700 same-euid parent, 0600 socket, same-euid kernel peer checks in both directions (SO_PEERCRED on Linux; getpeereid/LOCAL_PEERCRED-equivalent on macOS through Tokio). Existing web and peer TCP exposure/auth controls remain independent; local RPC never accepts TCP.
- **Bounds on input size, count and rate:** 1 MiB encoded NDJSON frame, depth64, 16 KiB discovery, connect1s/hello2s/write5s; proposed 64 connections, 16 inflight ids/connection, 128 ordinary queued operations, 10s admission timeout, reserved internal/control capacity. Eight subscriptions/connection; 256 events/subscription and 4 MiB queued bytes/connection. Chunk transfer proposal: 256 KiB raw chunks, 16 MiB assembled input, one transfer/connection, 64 MiB global staging, 10s inactivity. Existing definition document1MiB/depth64 and doctor2 jobs/120s bounds remain. Reject overload explicitly; no silent truncation of state. Gate 2 approves these bounds.
- **Permissions of files it writes:** daemon.json and temporary transport/editor/discovery files0600; directories0700; never follow/unlink foreign symlink or regular file at socket endpoint. Only explicit start under ownership replaces a stale socket; shutdown removes only its own generation. Supplemental stable guard is proposed in G2; never unlink its inode. Stored entity format stays unchanged; no broad recursive chmod of an existing project.
- **Secrets and environment passed to children:** no request env import into listener, no protocol payload logging, errors name field/type without supplied values. Agent children get existing deliberate adapter env plus project/socket/identity routing; bootstrap removes agent debug/identity markers from listener configuration. Preserve required owner credentials only at their intended adapter boundary. Existing explicit shell adapter remains; no shell/argv RPC.
- **Untrusted text rendered to a terminal or passed to a shell:** identifiers and paths validated before access; render control bytes safely in diagnostics, do not log prompt/body/env. Terminal rendering remains frontend-owned. Editors receive private temporary authored documents, not `.orchestry` files. Server-side apply revalidates writable keys and expected base digest. Attachment-source authority remains the local caller as in ADR0011 D12; destination/resource path confinement remains mandatory.

## Verification environment

Test engineer owns automated daemon/protocol/state/concurrency tests; CLI test owner migrates binary/MCP fixtures; qa-engineer owns manual acceptance; release engineer runs the CI set and cold gate at a frozen integration commit. All tests use temp projects and explicit short isolated ORCH_SOCKET, fake shell agents and isolated ORCHESTRY_GLOBAL_CONFIG_PATH. Pure engine/domain/TUI tests retain in-process fakes. Do not use production `.orchestry` or real AI CLIs in automated tests.

Linux CI on every PR covers Unix sockets, Linux SO_PEERCRED, file modes, subprocess/signal lifecycle, existing suites and new concurrency/no-daemon matrix. macOS local verification covers the corresponding peer credential implementation and short-path boundary; cold `./xtask/gate.sh` runs on macOS. An injected different-UID case is not evidence of a real second-UID run: mark actual cross-user checks env-limited if unavailable and obtain owner disposition.

TUI acceptance uses `tmux -L <unique-socket> -f /dev/null` with temp project, pinned terminal type, bounded polling, capture-pane and cleanup guard. r/q, disconnected state, reconnect after explicit owner start and push updates require real binary tmux tests, not FakeBackend alone. Process-level SIGHUP remains portable-pty as existing tests require. Native terminal profiles, native clipboard and actual browser rendering are not covered by tmux; qa-engineer records owner-Mac results. A real Claude Code MCP session needs the owner's Mac/account and explicit test-project permission; no automated test launches it. R28 cannot be called pass from a simulated MCP transport alone.

Manual sequence: owner explicitly starts the test daemon; QA attaches CLI, TUI, web and MCP; records `pgrep -fl orch` distinguishing one daemon from multiple clients; owner/authorized QA kills that specific test daemon; verify all client channels show NoDaemon, no new daemon child appears, and explicit owner start alone restores service. No permission to kill a real project process is inferred.

## Open questions

- **G1 — owner/lead at gate 2:** strict fail-on-skew also prevents an upgraded binary from issuing `daemon stop`. Recommended: keep rejection and instruct explicit stop using a retained matching binary, or independently verified owner SIGTERM, then explicit start. Alternative: approve a version-independent, same-UID stop control contract. No exception is implemented or assumed here.
- **G2 — owner/lead at gate 2:** accept a stable advisory `.daemon-owner.lock` guard plus unchanged PID text, and refusal to evict a live PID by mtime? The existing 60-second stale rule and stale-unlink race cannot prove one owner. This is a coordination refinement, not a storage schema migration.
- **G3 — owner at gate 2:** approve bounded aggregate transport input (16 MiB) and existing `serve --once` until-terminal semantics via RPC, distinct from the newly specified one-tick `run --once`. These refine surfaces not settled in Q1; accepted inputs must never silently truncate. Alternatives are a different explicit cap/finite-batch mapping, with matching tests/docs.

## Verification

Not run: this is the requirements draft. The completing PR fills each mandatory row with actual commit and platform evidence; blank is not pass. W rows are scope decisions, verified by the corresponding exclusion gates and review. A1's document check is not implementation verification.

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

Defects found outside the requirements:
- No new unrelated defect scope. The existing PID-lock age/cleanup behaviour and mismatched stop are directly relevant blockers to the proposed guarantees (G1/G2).
- Global preference writes by multiple project daemons retain existing cross-project constraints; this goal is not a per-user ownership service.

## Protocol and service reference (proposed)

Normal CLI/TUI/web sessions retain the existing local-owner operation set. MCP normal/read_only/allow_destructive sessions retain the existing tool catalogue restrictions; do not apply MCP tool omissions to CLI commands. Same-UID identity remains attribution.

This appendix keeps the proposed wire contract in the repository, not only in the team's scratch design. It becomes normative when the owner approves the spec. Operation/argument meanings remain the existing service inputs; no generic shell or store escape is allowed.


Local protocol initial version 1; binary is the current package version (this goal does not bump it). NDJSON UTF-8 JSON objects, ≤1,048,576 bytes excluding LF, exactly one LF terminator; empty, duplicate-field, unknown-tag, trailing/unterminated and invalid-type frames rejected. Dedicated reader owns partial-line state. Parse depth ≤64. Request ids are nonzero u64 scoped to a connection, never reused while outstanding. Exactly one terminal `ok` or `err` per accepted id; `part` frames precede it. No arbitrary shell or storage-path RPC.

| Direction | Frame | Contract |
|---|---|---|
| C→D first | `{"hello":{"protocol":1,"binary":"<semver>","project_root":<path>,"caller":{"kind":"cli\|tui\|web\|mcp\|agent","agent_id":null,"pid":123},"mode":"normal\|read_only\|allow_destructive"}}` | Agent default is resolved once: explicit `--agent` over ORCH_AGENT_ID. Kind/pid/agent are attribution only; bound identity strings to 256 bytes. |
| D→C first | `{"welcome":{"protocol":1,"binary":"<semver>","project_root":<path>,"instance":"<id>","pid":123,"compatible":true}}` | Same-euid checked first. On skew send compatible=false with own versions then close without decoding requests. |
| D→C pre-session | `{"reject":{"code":"handshake_failed","message":"<safe reason>"}}` | No id exists yet. Bounded message, no payload echo. UID denial can close without any bytes. |
| C→D | `{"id":1,"caller":{"agent_id":null},"req":{"op":"task.get","args":{"id":"tsk_…"}}}` | Tagged typed request enum; every request repeats effective identity. Explicit `from` fields retain current override rules. It cannot alter session mode. |
| D→C terminal | `{"id":1,"ok":<typed result>,"commit_seq":42}` | Existing domain/projection values; seq is an in-memory state-operation ordinal, not a stored entity revision. Null for void. |
| D→C terminal | `{"id":1,"err":{"code":"invalid_arguments","message":"…","exit_code":2,"execution":"not_executed\|completed\|unknown"}}` | Typed safe error categories preserve exits 1–6 and add 7. Failures after partial storage work use unknown; never imply rollback. |
| D→C streaming | `{"id":1,"part":{"n":0,"kind":"log\|event\|result_chunk","data":…}}` | Increasing n per request. Large JSON results serialize once as UTF-8 bytes and stream base64 chunks (≤256 KiB raw) then `ok:{"streamed":true,"encoding":"json_utf8","bytes":n,"sha256":"…"}`. Client reconstructs unchanged result; incomplete streams are errors, not success. |
| C→D subscription | `{"id":2,"req":{"op":"subscribe","args":{"stores":["tasks"],"filter":{},"bus":true}}}` | Returns `{subscription,instance,seq}` before any ev for that subscription. Filter is typed optional ids/goal/agent, not executable predicate. |
| D→C push | `{"ev":{"subscription":3,"instance":"…","seq":1,"store":"tasks","id":"tsk_…","kind":"updated","commit_seq":42}}}` | store change, kind created/updated/deleted; coarsely invalidates client cache. |
| D→C push | `{"ev":{"subscription":3,"instance":"…","seq":2,"bus":<OrchestratorEvent DTO>}}` | Domain event payload schema pinned by proto, no engine types. Large progress may use a truncation marker; state invalidations cannot silently disappear. |
| D→C push | `{"ev":{"subscription":3,"instance":"…","seq":4,"resync":{"reason":"lagged"}}}` | Explicit gap marker; refetch all subscribed views. If marker cannot be queued, close session so reconnect requires resync. |
| C→D control | `{"id":3,"req":{"op":"unsubscribe","args":{"subscription":3}}}` | Idempotent, returns null; frees queues. |
| C→D control | `{"id":4,"req":{"op":"request.cancel","args":{"id":1}}}` | Stops stream observation/queued work only; never silently cancels a running agent. Use task.cancel for that. Running state writes finish and return their result if possible. |

Large inbound arguments cannot be silently dropped by peer's oversize policy. `transfer.begin {purpose,total_bytes}` → opaque connection-local token; `transfer.chunk {token,n,data_base64}` → null; `transfer.finish {token,sha256}` → sealed token; typed request's `input_transfer` consumes it once. Chunk ≤256 KiB raw, total ≤16 MiB, one active transfer per connection, 10 s inactivity timeout, 64 MiB aggregate staged bytes, stored only in daemon-owned temporary files cleaned on disconnect/startup. Actual operation still enforces existing limits (definition document ≤1 MiB/depth64); transfer is transport, not a new generic file write. Use transfers only for envelope expansion, large strings or clipboard image bytes. Existing task attachments are daemon-read source paths with current relative-root semantics; no upload needed. Result streaming avoids whole-wire allocations, but existing service vectors may still allocate; do not claim unlimited query scalability. A payload beyond the explicit local aggregate bound returns an error; gate 2 records this transport bound rather than truncating results.

Connection/admission defaults (architect proposals for gate 2): 64 connections, 16 in-flight ids per connection, 128 ordinary queued state operations per project, 10 s admission deadline, 8 subscriptions/connection, 256 queued events/subscription with total queued-byte cap 4 MiB/connection. Readers stop admitting before memory grows beyond caps; reject overloaded work as not_executed. Keep a reserved internal/control lane so stop/cancel cannot be starved. Writes time out after 5 s; handshake 2 s; connect 1 s. Long-running streams have no total run deadline but do have bounded write/inactivity handling. Doctor retains two admitted jobs and existing 120 s deadline outside the state permit.

### Complete operation catalogue

This table maps today's public service/entry-point surface, including direct store helpers. Each comma-separated name is a distinct typed request; no generic service invocation. Arguments retain current service input structs/CLI meanings, translated to protocol DTOs when those structs live in engine/core/CLI. Return serialized domain values unchanged; frontend-only printing/projection stays at the edge. RPC results for mutations include the updated entity where MCP already needs it, computed within the same operation. Internal-only create/start/finish run and agent stats functions are NOT offered to arbitrary clients; they remain coordinator actions from the orchestrator.

| Requests | Existing source / callers / input → result |
|---|---|
| `status.get`, `state.get`, `ui.bootstrap` | status command and TUI LaunchContext: status summary, OrchestratorState, project/version/defaults/warnings/preferences. No store paths leak as capabilities. |
| `task.create`, `task.list`, `task.get` | TaskService create/list/get; CLI task add/list/show, MCP task_add/list/show, TUI refresh/create; CreateTaskInput/filter/id → Task(s). |
| `task.assign`, `task.cancel`, `task.approve`, `task.review_complete`, `task.reject`, `task.retry`, `task.delete`, `task.update_fields` | Service inputs id/agent, id/feedback, field patches; `task.approve` preserves CLI/MCP hold-release-or-review semantics; TUI uses `task.review_complete` for its existing direct review→done transition. Cancel uses R9–R12/R30 orchestrator path. Delete currently TUI-only; no new MCP tool. |
| `task.definition.get`, `task.definition.apply` | CLI edit/update/patch and MCP task_update/patch: authored raw document with opaque base digest; apply update/patch + optional expected digest → Task. Validate writable keys in daemon. |
| `task.attachment.read`, `attachment.stage_image` | Existing attachment viewing and TUI paste image; validated task/filename → chunks, bytes → daemon-owned temporary attachment reference. Source paths for task.create retain ADR0011 D12 trust semantics. |
| `agent.create`, `agent.list`, `agent.get`, `agent.update_fields`, `agent.remove`, `agent.enable`, `agent.disable`, `agent.autonomous`, `agent.stop` | AgentService and live force_stop_agent; CLI/MCP/TUI. list/get include qualified remote IDs as today; creation/update returns authored diagnostics for frontend to render. |
| `agent.definition.get`, `agent.definition.apply` | agent edit/update/patch raw authored fields + optional base digest; current flags, model/effort/prompt-mode validation and warnings preserved. |
| `goal.create`, `goal.list`, `goal.get`, `goal.update`, `goal.status`, `goal.delete`, `goal.tasks`, `goal.progress` | GoalService all public operations; status args id/status/force. `goal.get` projection may combine task list under the permit. Paused path uses R9–R12/R30. |
| `message.send`, `message.broadcast`, `message.inbox`, `message.list`, `message.mark_read` | MessageService and inbox marking; from/to/subject/body/reply_to, agent id/filter → message(s). Broadcast expands recipients daemon-side. Qualified remote targets use existing bridge, not a client TCP connection. |
| `context.set`, `context.get`, `context.list`, `context.delete` | ContextStore input key/value/expiry → existing ContextEntry/value projection. Client reads never invoke expire. `context.expire` is INTERNAL ONLY. |
| `team.create`, `team.list`, `team.get`, `team.join`, `team.leave`, `team.add_task`, `team.remove_task`, `team.set_lead`, `team.disband`, `team.for_agent` | TeamService methods; remove_task/for_agent serve existing backend helpers where used; no extra MCP catalogue entries. |
| `config.get`, `config.set`, `config.definition.get`, `config.definition.apply` | CLI/MCP config operations, raw key/value/document/mutation kind → current projections, refresh R9–R12/R30. |
| `config.global.get`, `config.global.set`, `config.global.show`, `ui.preferences.patch` | config global and TUI palette/activity/notifications; field patches, never client read-whole/save-whole. Machine-global store still has existing cross-project semantics; daemon ownership is per project, not a claim of global single writer. |
| `config.prompt_mode.get`, `config.prompt_mode.set`, `config.prompt_mode.toggle`, `state.complete_onboarding` | TUI raw-config and state-store helpers; return mode/state. Toggle resolves latest value inside coordinator. |
| `run.list`, `run.get`, `run.events`, `run.events_tail`, `run.history`, `run.timings`, `logs.follow` | RunService list_all/get/list_for_task/list_for_agent/read_events/read_events_tail and CLI logs filters/since; run id or task/agent filter → existing Run/RunEvent/history/timing values. Follow is part stream; reads from daemon only. |
| `dispatch.set`, `dispatch.tick`, `dispatch.run_task`, `dispatch.run_all`, `dispatch.until_terminal` | All current run/serve/TUI control paths; the dispatch table below. Runs launch in daemon; stream output, terminal result contains exit status. |
| `daemon.status`, `daemon.stop` | Live process metadata/clean shutdown; status returns pid/socket/protocol/binary/instance/start time/dispatch/running tasks/client count. |
| `remote.add`, `remote.remove`, `remote.list`, `remote.status`, `remote.listen` | Current remote command and MCP read subset; alias/address/config operations run beside peer runtime in daemon. Listener toggling only if existing command supports it; serve --listen remains explicit start option. |
| `doctor.run`, `models.catalog`, `catalog.agents`, `catalog.templates` | DoctorService, model discovery and agent shop/template shop; bounded daemon worker for probes/network fetch; TUI polls local completion slot, not files. |
| `org.list`, `org.deploy` | Org listing/deployment via existing validation/provisioning in daemon; source template selector/root/options → current deploy report. Deployment is ordered but not made transactional across files. |
| `org.template.list`, `org.template.show`, `org.template.init`, `org.template.definition.get`, `org.template.definition.apply`, `org.template.agent.list`, `org.template.agent.add`, `org.template.agent.definition.get`, `org.template.agent.definition.apply`, `org.template.agent.delete` | Existing org/template commands incl. user template sources. Preserve config.yml/template file formats, not a new store. Read/edit temp/apply roundtrip for interactive edit. |
| `update.check` | Current update checker returns install instructions/version metadata; this goal does not add binary install or release. No project state read bypass. |
| `subscribe`, `unsubscribe`, `request.cancel`, `transfer.begin`, `transfer.chunk`, `transfer.finish` | Transport controls the frame/subscription reference, not new CLI/MCP tools. |

`init` is the only project-creation exception: narrow daemon bootstrap API, no server started, print `orch daemon start`; refuse to overwrite an existing project or mutate a live daemon's state. Clap help/version and pure syntax validation perform no state operation and need no connection. All other commands require a daemon for their stateful work, including commands formerly runnable without a project (global config/catalog/template helpers); make that intentional behaviour change visible in the manual. Keep filesystem reads of explicitly supplied input files outside `.orchestry` at the frontend if necessary; project-authoritative validation and all writes remain daemon-side.

Interactive edit: get authored-only bytes and base digest from daemon, launch editor on a private temporary file outside `.orchestry`, submit modified bytes plus base digest. If authored base changed, fail conflict without overwriting; runtime-owned fields always come from current state. No persisted revision field is added. Never pass a `.orchestry` filename to the editor. The digest prevents this formerly unbounded read/edit/write interval from losing a concurrent authored change.

Catalogue proof in implementation: a table-driven test enumerates clap leaf commands, all 50 MCP tools, all TuiBackend methods and resource URIs; every stateful item maps to a typed op, with explicit pure-render/bootstrap exclusions. Fail on unclassified additions. Read-only backend methods may use cached values populated only by RPC/subscription. No generic `FileRead(path)` or `StoreSave(entity)` escape is permitted.

### Dispatch and command mapping (Q1)

| Command | Meaning |
|---|---|
| `daemon start [--detach] [--dispatch on\|off]` | Explicit start; foreground unless detach; OFF default. |
| `serve`, `run --watch` | Explicit foreground daemon start, dispatch ON. If another daemon exists, exit 4 + `Use: orch daemon dispatch on`. |
| `daemon dispatch on\|off` | RPC toggles future dispatch. OFF does not kill existing runs, drop ownership or stop expiry/peer processing. |
| `run --once` | RPC one scheduling tick, stream that tick's events and report it; does not stop daemon or permanently toggle dispatch. Add this flag if absent at base. |
| `run <id>` | RPC explicit task launch, observe until terminal. Disconnecting observer does not kill the run. Normal eligibility/capacity/cancel checks still apply. |
| `run --all` | RPC finite current eligible-task run batch, preserve current result semantics; no second engine. |
| `serve --once` | Existing distinct until-terminal behaviour via `dispatch.until_terminal`, skip autonomous seeding; it requires an existing daemon, observes the admitted workload and returns existing all_done/has_failed/interrupted status. Does not stop the daemon or unrelated work. Document distinction from the new run --once. |
| TUI start/stop watch | `dispatch.set on/off`; no process lifecycle operation. |
| `daemon status`, `daemon stop` | Live RPC per R14–R15. |
| `init` | Creates project and prints explicit-start hint; no daemon. |

All dispatch entrances use one coordinator; concurrent manual ticks serialize and recheck eligibility, with no duplicate launches. Dispatch OFF inhibits automatic work only; explicit tick/run commands are deliberate execution requests. Peer listener flags are start options. `serve --listen` with a live owner returns exit 4 and an explicit owner stop/serve hint. Do not silently add a listener or launch a second process. Keep log format/tick interval options at their established command boundary, forwarding execution settings to daemon operations and rendering output at the client.

### Push, gaps and snapshot ordering

Subscribe to tasks/goals/agents/messages/context and optionally teams/runs/config/state/remote plus the domain event bus. Per-subscription seq starts at 1 and increments only for delivered-or-dropped events matching that subscription, so filters do not create fake gaps. Instance identifies daemon lifetime; seq is never durable. Assign commit_seq at the shared coordinator, and enqueue change notifications only after state publication. Engine-only, peer and expiry changes participate, not just RPC writes. Coarse events contain entity identity and action, not secret-bearing entity contents.

Initial sync: register subscription under coordinator and obtain baseline commit_seq; acknowledge; buffer pushed events; fetch views via normal reads; apply/refetch invalidations after that baseline, then continue. Never fetch first and subscribe second. A gap, explicit resync marker, changed instance or lost connection invalidates every subscribed cache. Client resubscribes and refetches; no durable replay promise. Multiple read calls need not be one snapshot, but each carries commit_seq; buffered invalidations prevent a missed update. `ui.bootstrap` and status aggregates use one coordinator operation where a consistent view is needed.

TUI polls only its in-memory event receiver, not disk. Web preserves existing PTY design: its `orch tui` child uses the same socket subscriptions; web parent also holds a lightweight connection to report outage before creating sessions. Web auth, Host/Origin controls and PTY limits remain. MCP resources/subscribe tracks requested `orch://` URIs and sends `notifications/resources/updated`; collection URIs invalidate on any member change, item URIs on relevant id changes, status on task/agent/run/dispatch changes, inbox on relevant message changes. Gaps invalidate all subscribed resources; callers refetch. Remove redundant MCP store FIFO; preserve bounded bridge admission and doctor limits without a second serialization lock.
