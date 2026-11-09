# Daemon-owned state: one orch process owns `.orchestry/`

status: proposed
owner: orchestrator (lead)
date: 2026-11-09
spec: [../specs/2026-11-09-daemon-owned-state.md](../specs/2026-11-09-daemon-owned-state.md) <!-- TBD: after A1 (the architect's spec lands in PR 1; requirement ids below are R-TBD until it does) -->

## Goal
Exactly one process per project, the **daemon**, owns `.orchestry/`, every store and every agent process. The CLI,
the dashboard, `orch web`, `orch mcp` and the `orch` commands agents run inside their tasks are clients of it over
a local Unix socket, for reads and writes alike. No client reads or writes `.orchestry/` state itself, no client
ever starts a daemon, and with no daemon every client fails with one uniform message and exit code 7. This closes
the cross-process store races of [ADR 0011](../decisions/0011-mcp-server.md) residual D10 and
[ARCHITECTURE §2.1](../ARCHITECTURE.md#21-a2-in-detail--the-concurrency-story-is-weaker-than-single-writer) as a
class: one writer instead of a patch per race.

## Non-goals
Auto-start of any kind (owner decision: the owner wants to know when a long-lived process starts on their
machine); a direct-store fallback in any client; SQLite or any change to the YAML, index, `state.json`,
`config.yml`, org-template or run-event formats; Windows; one daemon per user for several projects; changes to
the remote peer protocol; an idle exit, watchdog or automatic restart; a release, tag or version bump (0.14.0 is
separate).

## Context
- Base: `main` at `8bab8d1c` (v0.13.0). Owner's request: the goal text, quoted in the spec's Source.
- Today `orch_engine::build_light` / `build_full_with_peer` are called from 22 files in `orch-cli` (every command,
  `mcp/tools/*`, `tui/backend.rs`, `web/mod.rs`, `serve/`). The PID lock (`crates/orch-engine/src/lock.rs`)
  serialises orchestrators only; each store has its own `.index.lock`.
- Documented races: entity and index written as two steps; whole-record saves with no compare-and-swap; context
  expiry publishing a keep-list from `list`/`get`, also from `--read-only` MCP sessions; a per-process
  `ensure_dir` cache. Recovery is documented in the runbook ("Rebuilding an index after a lost update",
  "Recovering after concurrent MCP writers") and manual 17 ("Concurrent writers").
- Decision record: ADR 0012 <!-- TBD: after A1 (file name and link) -->, superseding ADR 0011 decision 7 and the
  D10 acceptance. User manual: [chapter 18](../manual/18-daemon.md).

## Design
Owner-approved at gate 1 (2026-11-09); the architect's spec and ADR refine it, and the owner approves the spec at
gate 2 before the daemon is built.

**Daemon.** `orch daemon start` runs the daemon in the foreground (`--detach` to background it) with **dispatch
off** unless started with `--dispatch on`, so work can be queued first. It takes the PID lock first: of two
explicit starts the lock picks one, and the loser exits 4 with `✕ orch daemon already running (PID …)`. It holds
the lock for its lifetime, owns every store and agent process, runs the existing engine services unchanged, and
exits cleanly on SIGTERM, SIGINT and SIGHUP, reconciling running agent processes per the existing `state.json`
rules. On start it reconciles claimed and running tasks as today and replaces a stale socket file; it is the only
code that ever removes one.

**Commands.** `orch serve` and `orch run --watch` start the daemon in the foreground with dispatch on (they keep
their v0.13.0 feel: they *are* the orchestrator process). `orch daemon status`, `orch daemon stop` and
`orch daemon dispatch on|off` act on the running daemon over the socket. One tick and `orch run <id>` go through
the socket and need a running daemon. <!-- TBD: after A1 (the plan says `orch run --once`; today's flag is `orch serve --once`; also `orch run --all`, `orch tui --observe`, `--listen` on `tui`) -->

**No daemon.** No socket, a stale socket, a refused connection, a failed handshake or a version mismatch all
produce `DomainError::NoDaemon { project_root, detail }`, printed by every client as
`✕ No orch daemon is running for <project root> (<detail>)` / `Start it with: orch daemon start`, exit code 7
(1–6 are taken). `orch mcp` returns it as the JSON-RPC error of `initialize` and of every tool call and stays up;
the dashboard shows it full-screen with `r` retry and `q` quit; `orch web` exits 7 at start, or answers HTTP 503
with the same text and a banner if the daemon goes away while it serves. `orch init` creates the project and
prints how to start the daemon. No client code path spawns, forks or execs anything.

**Version skew.** The handshake carries the protocol number and the binary version. Any mismatch, in either
direction, fails the client with the uniform message naming both versions and `orch daemon stop, then orch daemon
start`; the daemon never processes a request from a skewed client. No drain, no restart.

**Socket.** `.orchestry/orch.sock` (directory 0700, socket 0600); if that path is over 100 bytes,
`$TMPDIR/orch-<first 16 hex of sha256(project root)>/orch.sock`. `.orchestry/daemon.json` (0600) records the
path, pid, versions and start time; `ORCH_SOCKET` overrides it (tests always use a short path). Clients only
*report* a stale socket and never unlink it. The daemon checks the peer's uid; every frame is untrusted input.

**Protocol.** NDJSON frames over the socket, reusing `orch-peer`'s bounded (1 MiB) `read_frame`, lifted into a
shared crate; the peer wire format is unchanged. `{"id":n,"req":…}` → `ok` | `err{code,message}`, streaming
responses as `part` frames ending in `ok`. Subscriptions push coarse change events
(`{"ev":{"seq":k,"store":…,"id":…,"kind":…}}`, the client refetches; a `seq` gap means resync) plus orchestrator
bus events, so the dashboard and web stop polling and `orch mcp` emits `resources/updated`. Requests carry the
caller identity (`ORCH_AGENT_ID` / `--agent`) as today.

**Engine guarantees** (in the daemon, tested): pausing a goal is atomic with respect to dispatch; cancelling a
task from any client stops a live agent process; context expiry is a daemon step, never a side effect of a read.

**Crates.** `orch-proto` (wire types and framing; `orch-domain`, serde, tokio io), `orch-client` (`orch-domain`,
`orch-proto`, tokio), `orch-daemon` (`orch-engine`, `orch-infra`, `orch-peer`, `orch-proto`). `orch-cli` names
`orch-daemon` only at the daemon entry points and `init`; `cargo xtask crate-graph` gets a bidirectional
allowlist (like `ENGINE_INFRA_SITES`) that fails any other `orch-cli` file naming `orch_daemon`, `orch_engine` or
`orch_infra`, and fails `orch-client` on process-spawning tokens (`std::process::Command`, `tokio::process`,
`fork`, `exec`).

**Rejected** (details in ADR 0012): a direct-store fallback (two paths, races stay); auto-start (owner decision);
a separate `orchd` binary (one `orch` binary, Homebrew recipe unchanged); gRPC or tarpc (new load-bearing
dependency, no gain on one local socket); a second framing next to `orch-peer`'s; protocol-number-only skew
checks (a rebuilt dev binary could talk to a stale daemon).

## Tasks

Four stacked PRs against `main` (owner, gate 1), each green and leaving `main` working; the owner merges each.
PR *n*+1 is built once PR *n* is frozen for review and rebases on its merge. Builders work in separate worktrees
with disjoint scopes. Requirement ids are `R-TBD` until the spec lands; the goal's must-row numbers are given in
brackets to make the mapping checkable. PR 2 is likely over the ~3,000-line guideline; the lead records why in
the PR and reviews in slices under 800 lines. Orch task ids are in [Status](#status).

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| A1 | Design, spec, ADR 0012, ARCHITECTURE §3.1 diagram text | R-TBD [1–12] | `docs/specs/2026-11-09-*`, `docs/decisions/0012-*` | — | lead review; owner approves the spec at gate 2 | 1 |
| B0 | Engine guarantees: atomic pause, cancel stops a live process, explicit context expiry | R-TBD [5] | `crates/orch-engine/src/**` | — | `cargo test -p orch-engine` | 1 |
| T0 | Failing-first tests for B0; harness design note for the in-process daemon | R-TBD [5, tests] | `crates/orch-engine/tests/**` | — | `cargo test -p orch-engine --tests` (fails before B0, passes after) <!-- TBD: after A1 (test file names) --> | 1 |
| W0 | This plan; manual chapter 18 skeleton; RUNBOOK, README, CHANGELOG and manual 17 drafts in the team's working files | R-TBD [12] | `docs/plans/**`, `docs/manual/**` | — | links resolve; `grep -rnE '/(Users|home)/[a-z]' docs/plans/2026-11-09-daemon-owned-state.md docs/manual/18-daemon.md` prints nothing | 1 |
| SE | Threat model of A1: socket permissions, uid check, explicit-start race, stale socket, `daemon.json` trust, environment inheritance, frame validation, stop and dispatch abuse | R-TBD [11] | reader | A1 | report; lead adjudicates at CP1 | 1 |
| RT | Red team the decision set: one daemon, no fallback, no auto-start, dispatch as a daemon mode, fail on skew | R-TBD [2–4, 6] | reader | A1 | report; lead adjudicates at CP1 | 1 |
| CP1 | Adjudicate wave A; architect rebuttal into spec v2; gate 2 | — | spec, this plan | A1, B0, T0, W0, SE, RT | owner approves the spec | 1 |
| S1a | `orch-proto` and `orch-daemon`: listener, handshake, uid check, dispatcher over the existing services, subscription hub, dispatch switch, signals, socket path, `daemon.json`; lift the framing out of `orch-peer` | R-TBD [1, 4, 7, 10, 11] | `crates/orch-proto/**`, `crates/orch-daemon/**`, `crates/orch-peer/src/codec.rs`, `Cargo.toml`, `Cargo.lock` | CP1 | `cargo test -p orch-proto -p orch-daemon -p orch-peer`; `cargo xtask crate-graph`; `cargo deny check` | 1 |
| S1b | `orch daemon start\|status\|stop\|dispatch` subcommand; merge B0 and T0 | R-TBD [3, 5, 6, 7] | `crates/orch-cli/src/commands/daemon.rs`, `crates/orch-cli/src/lib.rs`, `crates/orch-cli/src/tree.rs` <!-- TBD: after A1 --> | CP1, B0, T0 | `cargo test -p orch-cli daemon` <!-- TBD: after A1 --> | 1 |
| T1 | In-process daemon harness; daemon lifecycle unit tests | R-TBD [7, tests] | `crates/orch-daemon/tests/**` | S1a (API commit) | `cargo test -p orch-daemon --tests` | 1 |
| W1 | Plan task table and status; ARCHITECTURE §3.1 diagram; CHANGELOG stub | R-TBD [12] | `docs/plans/2026-11-09-daemon-owned-state.md`, `docs/ARCHITECTURE.md`, `CHANGELOG.md` | CP1 | links resolve; diagram matches `cargo xtask crate-graph` | 1 |
| I1 | Integrate PR 1; freeze the commit | — | PR 1 files | S1a, S1b, T1, W1 | the CI set; cold `./xtask/gate.sh` | 1 |
| G1 | Board 1: code, security, resilience, facts, cold gate; fixes | R-TBD [11] | per finding | I1 | reviewer verdicts | 1 |
| S2a | `orch-client` (connect, handshake, no-daemon error, skew); `orch init` prints how to start the daemon; convert every CLI command but `init`, `daemon` and `serve` | R-TBD [2, 3, 4] | `crates/orch-client/**`, `crates/orch-cli/src/commands/**`, `crates/orch-cli/src/context.rs`, `crates/orch-cli/src/lib.rs` | PR 1 frozen | `cargo test -p orch-client -p orch-cli` | 2 |
| S2b | `orch mcp` as a stdio-to-socket bridge; store lane removed; no-daemon as a JSON-RPC error | R-TBD [3, 9] | `crates/orch-cli/src/mcp/**` | S2a (client API commit) | `cargo test -p orch-cli --test mcp_stdio --test mcp_contract --test mcp_tools` | 2 |
| T2a | Stress test (dashboard-equivalent client, several CLI clients, two MCP clients for N seconds: files equal index, no lost field update, no lost context key); lifecycle tests; no-daemon matrix with a process-table assertion | R-TBD [3, 7, tests] | `crates/orch-cli/tests/daemon_*.rs` | S2a | `cargo test -p orch-cli --test daemon_stress --test daemon_lifecycle --test daemon_no_daemon` <!-- TBD: after A1 (test names) --> | 2 |
| T2b | Move binary tests (`yaml_contract`, `routing`, `mcp_stdio`, `remote_*`, `web_bridge`, `tui_*`) to an explicit test daemon on an isolated `ORCH_SOCKET` | R-TBD [tests] | `crates/orch-cli/tests/common/**`, `crates/orch-cli/tests/mcp_common/**` | S2a | `cargo test -p orch-cli --tests` | 2 |
| W2 | README "Upgrading from v0.13.0"; CHANGELOG; manual pages of every command whose meaning changed; regenerated command reference | R-TBD [6, 12] | `README.md`, `CHANGELOG.md`, `docs/manual/**` | S2a | links resolve; `docs/manual/tools/gen-command-reference.sh` output committed | 2 |
| I2 | Integrate PR 2; freeze | — | PR 2 files | S2b, T2a, T2b, W2 | the CI set; cold `./xtask/gate.sh` | 2 |
| G2 | Board 2 (+ CLI round-trip latency against v0.13.0); fixes | R-TBD [11] | per finding | I2 | reviewer verdicts; latency report | 2 |
| S3a | Dashboard backend over the client with subscriptions | R-TBD [2, 3, 8] | `crates/orch-cli/src/tui/**` | PR 2 frozen | `cargo test -p orch-cli tui::`; tmux end-to-end tests <!-- TBD: after A1 (names) --> | 3 |
| S3b | `orch web` over the client with subscriptions; 503 and banner when the daemon goes away | R-TBD [2, 3, 8] | `crates/orch-cli/src/web/**` | PR 2 frozen | `cargo test -p orch-cli --test web_bridge` | 3 |
| S3c | Subscription hub hardening; MCP `resources/updated` | R-TBD [8, 9] | `crates/orch-daemon/**`, `crates/orch-cli/src/mcp/resources.rs` | PR 2 frozen | `cargo test -p orch-daemon`; `cargo test -p orch-cli --test mcp_stdio` | 3 |
| T3 | Push tests; dashboard and web rows of the no-daemon matrix (dashboard retry, web 503) | R-TBD [3, 8, tests] | `crates/orch-cli/tests/**` <!-- TBD: after A1 --> | S3a, S3b, S3c | `cargo test -p orch-cli --tests` | 3 |
| I3 | Integrate PR 3; freeze | — | PR 3 files | T3 | the CI set; cold `./xtask/gate.sh` | 3 |
| G3 | Board 3; QA pass 1 on the owner's Mac (dashboard, an MCP session and the CLI on one daemon the owner started; `kill -9`; nothing restarts it) | R-TBD [3, 7, 8] | per finding | I3 | reviewer verdicts; QA report | 3 |
| S4 | Remove `orch-infra` and `orch-engine` from client code; crate-graph allowlist and spawn-token check; delete `serve/disk_observer.rs`, the MCP lane and other dead cross-process paths | R-TBD [2] | `xtask/**`, `crates/orch-cli/src/serve/**`, `crates/orch-cli/src/mcp/**`, `crates/orch-cli/Cargo.toml` | PR 3 frozen | `cargo xtask crate-graph` passes, and fails on a planted `orch_infra` import in a client file; `cargo test --workspace` | 4 |
| W4 | ARCHITECTURE §2 rewrite; RUNBOOK; manual 17 and 18; spec Verification table; spec and plan set `done`; mirror | R-TBD [12] | `docs/**`, `README.md`, `CHANGELOG.md` | S4 | links resolve; every example re-run against the frozen commit; fact-check | 4 |
| I4 | Integrate PR 4; freeze | — | PR 4 files | S4, W4 | the CI set; cold `./xtask/gate.sh` | 4 |
| G4 | Board 4; QA pass 2 | R-TBD [all] | per finding | I4 | reviewer verdicts; QA report | 4 |

## Status

| Item | State |
|---|---|
| Gate 1 (plan and design questions) | approved by the owner, 2026-11-09 |
| A1, B0, T0, SE, RT | pending |
| W0 | drafted (the commit that adds this file) |
| CP1, gate 2 (spec) | pending |
| PR 1–4 | pending |

## Test plan
- The spec's acceptance checks, each with its platform <!-- TBD: after A1 -->.
- The stress test (T2a) is the evidence that D10 is closed and replaces the runbook hazard.
- No-daemon matrix: every client kind (CLI read, CLI write, dashboard, web, MCP `initialize` and a tool call)
  against no socket, a stale socket and a daemon of another version; each prints the uniform message, exits 7
  (or returns the error) and a process-table check shows nothing was spawned.
- Lifecycle: two explicit starts racing; daemon killed mid-run, then the next client call reports no daemon;
  explicit restart reconciles running tasks; a stale socket is replaced on start.
- Pause atomicity and cancel-stops-process tests (T0, B0).
- No test touches a real project's daemon or `~/.orchestry`; binary tests use an explicit test daemon on a short
  `ORCH_SOCKET` path.
- CI set: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test --workspace`; `cargo xtask crate-graph`; `cargo deny check`; cold `./xtask/gate.sh` on macOS before
  each PR (process boundary, socket, untrusted input); Linux CI green on every PR.

## Rollback / compatibility
No on-disk format changes: `.orchestry/` stays readable by v0.13.0 once the daemon is stopped. New files
(`.orchestry/daemon.json`, the socket) are ignored by older binaries <!-- TBD: after A1 (confirm `.gitignore` and
what v0.13.0 does with them) -->. Behaviour change: from PR 2 on, no `orch` command but `init` and the daemon's own
entry points works without a running daemon. Remote peers keep the same protocol, so 0.5.x peers still
interoperate. To back out, stop the daemon and install v0.13.0.

## Docs to update
Spec; ADR 0012 and the ADR index; ARCHITECTURE §2 (single-writer model: what the daemon guarantees and what it
does not) and §3.1; RUNBOOK (daemon lifecycle, no daemon, stale socket, crash recovery, version skew; the
index-rebuild and concurrent-MCP-writers procedures removed or marked legacy); manual chapter 18 (new), chapter 17
and every changed command, the command reference, troubleshooting and exit codes; README "Upgrading from v0.13.0";
CHANGELOG `[Unreleased]`.

## Open questions
- Which commands map how, beyond the approved Q1: `orch serve --once` versus the plan's `orch run --once`,
  `orch run --all`, the dashboard's `--observe` and `--listen` flags, and whether `orch tui` can still be the
  orchestrator. <!-- TBD: after A1 -->
- How an agent's `orch` command, run inside a task workspace, finds its daemon (project-root discovery or an
  inherited `ORCH_SOCKET`). <!-- TBD: after A1 -->
- Whether the index-rebuild procedure stays for a daemon crash between an entity write and its index write
  (single writer removes the race between processes, not a crash between two writes). <!-- TBD: after A1 -->
