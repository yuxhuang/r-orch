# Stdio MCP server (`orch mcp`)

status: approved
owner: orchestrator (lead), for the repository owner
date: 2026-11-07
plan: [docs/plans/2026-11-07-mcp-server.md](../plans/2026-11-07-mcp-server.md)

## Source
> Based on the orch architecture and all the commands, use your best judgement and capability, to add a stdio
> MCP component to orch (such as a subcommand `mcp`) to allow other agents to communicate to the orch
> orchestrator through the MCP aspect.

Items, split from the request above without rewording:
1. "Based on the orch architecture and all the commands, use your best judgement and capability,"
2. "to add a stdio MCP component to orch (such as a subcommand `mcp`)"
3. "to allow other agents to communicate to the orch orchestrator through the MCP aspect."

Owner's approval reply, 2026-11-07 (approves R1–R12 and answers the three gate questions):
> tier=mid stance=0.5 pr=yes q1=yes q2=use recommendation q3=adopt rmcp

The questions it answers, as put in the approval request:
- **q1** Mutating tools on by default: read *and* mutate tools are registered by default; destructive tools only
  with `--allow-destructive`; `--read-only` registers read tools only. Answer: yes (R4).
- **q2** Identity: `orch mcp --agent <id>` or `ORCH_AGENT_ID` sets the default sender and inbox; without it,
  sending a message needs an explicit `from`. Answer: the recommendation (R6).
- **q3** Dependency: adopt `rmcp` 3.9 (the official MCP Rust SDK) under the library-first policy, with an ADR.
  Answer: adopt (R10).

Reconciled 2026-11-07 per lead decision-1: R2, R3, R4 and R8 carry the lead's rulings (R2: rulings 1 and 3a;
R3: rulings 2a, 2b and 5; R4: rulings 2c and 3b; R8: ruling 3c). See Open questions.

Amended 2026-11-08 by the owner's follow-up goal `goal_h-gIBCW` ("MCP server follow-ups from PR #69"): R4 and R8
changed, R13–R16 added. The goal items that authorise the changes to approved `must` rows, quoted:
> 1\. E1 / R4, goal pause gate. Root cause: `goal pause` cancels the assignee's todo/retrying autonomous tasks
> across ALL goals, so `goal_status status=paused` sits behind --allow-destructive. Scope the cancellation on pause
> to the paused goal's own tasks (engine change, with tests for the G1/G2 shared-assignee fixture in the spec).
> Then move `goal_status status=paused` into default mode; `force=true` stays destructive. Update spec R4, the
> tool description, initialize instructions, manual 17, ADR 0011 and the orch MCP server instructions text
> accordingly.

> 4\. SIGHUP (R8). The server exits on SIGHUP in addition to SIGTERM/SIGINT. Owner confirms this: amend spec R8 to
> name SIGHUP and make sure the stdio test covers it.

> 5\. Spec amendments to R2/R3/R4/R8 made by lead decision-1 and decision-2 are confirmed by the owner. Record the
> confirmation in the spec's Open questions section and the verification table (date 2026-11-08), and remove the
> "the owner confirms these amendments when reviewing the PR" pending wording.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check (test or command; platform) |
|---|---|---|---|---|
| R1 | When started as `orch mcp`, orch shall run an MCP server over stdio (JSON-RPC 2.0 framing per the MCP spec version `rmcp` 3.9 implements): stdout carries protocol frames only and logs go to stderr; it resolves the project from the working directory, `--project-root <path>` overrides that, and it refuses with exit 3 when the project is not initialised. | item 2 | must | `crates/orch-cli/tests/mcp_stdio.rs`: initialize handshake, stdout/stderr discipline, exit 3 uninitialised (cwd and `--project-root`); Linux CI + macOS |
| R2 | The server shall expose a tool for every one-shot command that reads or mutates project state: task add/list/show/assign/cancel/approve/reject/retry/update/patch; goal add/list/show/status/update/delete; agent list/show (`status`)/add/update/patch/enable/disable/autonomous/remove; msg send/broadcast/inbox/list; context set/get/list/delete; team create/list/show/join/leave/add-task/set-lead/disband; status; logs (run events); doctor; config get/set/update/patch; remote list/status. Tool inputs mirror the CLI flags and results are the same JSON the command prints with `--json`; for the commands that print only a human message (task cancel/approve/reject/retry, agent enable/disable/remove, team join/leave/add-task/set-lead/disband, config set, context delete) the tool returns the documented MCP projection in [MCP-only result projections](#mcp-only-result-projections) (the mutated entity, `{key, value}` for config set, or `null` for a deletion), and `doctor` returns the CLI's JSON report without its header. | items 1, 3; decision-1 | must | table-driven test: `tools/list` ⊇ the catalogue; per-tool round-trip tests on a temp project comparing with `orch --json <cmd>`, and for each projection tool with `orch --json` reading the entity after the call; Linux CI + macOS |
| R3 | The server shall not expose interactive, long-running, process-spawning or host-mutating commands as tools; each is listed with its reason in [R3 exclusions](#r3-exclusions). | item 1 | must | catalogue test asserts every excluded name is absent in all three modes (R4); Linux CI + macOS |
| R4 | Destructive tools (agent remove, goal delete, team disband, context delete, task cancel) shall be registered only with `--allow-destructive`, and `goal_status` with `force=true` is an argument-level destructive gate: without `--allow-destructive` the call is rejected as JSON-RPC invalid params, naming `force=true` and `--allow-destructive`, before the goal is touched, whether or not the goal has an assignee. `goal_status` with `status=paused` and without `force=true` is allowed in default mode. The `goal_status` tool description and the default-mode `initialize` instructions shall disclose that pausing a goal cancels the todo/retrying autonomous tasks of the goal's assignee that belong to that goal (R13), and that `force=true` needs `--allow-destructive`. `--read-only` shall register read tools only; every tool shall carry `readOnlyHint`, `destructiveHint` and `idempotentHint` annotations consistent with that classification. `destructiveHint=true` may also appear on mutate tools outside the destructive set, because the MCP annotation `destructiveHint=false` means additive updates only. | approval q1; decision-1; decision-2; goal `goal_h-gIBCW` item 1 (owner, 2026-11-08): "Then move `goal_status status=paused` into default mode; `force=true` stays destructive." | must | `tools/list` tests over default, `--read-only` and `--allow-destructive`, including the annotations; on a fixture of goals G1 and G2 sharing one assignee that has todo/retrying autonomous tasks in both, in default mode: `goal_status` G1 `status=paused` succeeds, G1's todo/retrying autonomous tasks are cancelled, and G2 and all its tasks are unchanged; `goal_status` with `force=true` is rejected, with both goals and all their tasks unchanged after the denial (`crates/orch-cli/tests/mcp_tools.rs`: `default_mode_pausing_g1_cancels_only_g1s_own_autonomous_tasks`, `default_mode_pauses_a_goal_that_has_no_assignee_too`, `default_mode_refuses_force_even_on_a_status_that_does_not_pause`, `allow_destructive_pausing_g1_is_scoped_to_g1_as_well`); the static gate: unit test `mcp::tools::goal::tests::status_force_gate_is_static_and_pause_is_an_ordinary_change`; the disclosure: `crates/orch-cli/tests/mcp_stdio.rs` `default_mode_instructions_say_force_is_unavailable`, `default_mode_instructions_say_pause_works_and_is_goal_scoped`, `default_mode_instructions_do_not_describe_pause_as_cross_goal`, and unit test `mcp::server::tests::initialize_names_the_server_and_quotes_the_project_in_its_instructions`; the description and instructions also say that resuming does not restore cancelled tasks (CP1 ruling on red-team RT1, 2026-11-08): `default_mode_instructions_say_what_pause_spares_and_that_resume_does_not_restore`, `goal_status_description_says_resume_does_not_restore_cancelled_tasks`; Linux CI + macOS |
| R5 | The server shall serve the resources `orch://status`, `orch://tasks`, `orch://tasks/{id}`, `orch://goals`, `orch://goals/{id}`, `orch://agents`, `orch://agents/{id}`, `orch://context` and `orch://inbox/{agent}` as `application/json`, listed by `resources/list` and `resources/templates/list`. | item 3 | should | resource tests: list, templates list, read of each URI on a temp project, unknown id; Linux CI + macOS |
| R6 | When started with `--agent <id>` or with `ORCH_AGENT_ID` set, the server shall use that agent as the default `from` of msg send/broadcast and the default inbox agent, and the `initialize` result's `instructions` shall name the agent and the project; without either, `msg_send` shall require an explicit `from`. | approval q2; item 3 | must | tests with the flag, with the env var, and with neither; Linux CI + macOS |
| R7 | When a tool call fails, the server shall return a tool result with `isError: true` carrying the CLI's error message and hint for an `orch_core::Error`, a JSON-RPC invalid-params error for a schema violation, and shall never panic or exit on a bad call. Inputs get the same validation as the CLI; frame size is bounded by `rmcp`. | item 3 | must | malformed-input tests (bad JSON, unknown tool, wrong types, unknown ids, oversized fields) followed by a successful call on the same session; Linux CI + macOS |
| R8 | The server shall exit cleanly on stdin EOF and on SIGTERM, SIGINT or SIGHUP and not take the orchestrator's PID lock (`.orchestry/orchestry.lock`); storage writes use the stores' existing per-write file locks (for example `.index.lock`). One `orch mcp` process serves one client; the server is non-transactional, and running it alongside other writers is the same concurrent-writer hazard class as two `orch` CLI/TUI processes (ARCHITECTURE §2.1). The manual and RUNBOOK shall state that hazard, including running it alongside `orch run --watch`, the RUNBOOK shall give the recovery procedure, and both shall state that `--read-only` prevents authored mutations while storage maintenance writes (index rebuild, context expiry purge) still occur. `--read-only` together with `--allow-destructive` shall be a usage error (exit 1), reported before any project resolution. | item 2; decision-1; decision-2; goal `goal_h-gIBCW` item 4 (owner, 2026-11-08): "The server exits on SIGHUP in addition to SIGTERM/SIGINT. Owner confirms this" | must | EOF and signal tests in `crates/orch-cli/tests/mcp_stdio.rs` (exit status, no leftover child, no PID lock): `stdin_eof_after_initialized_exits_zero`, `stdin_eof_before_initialize_exits_zero_with_nothing_on_stdout`, `stdin_eof_leaves_no_project_lock_file_behind`, `sigterm_with_stdin_still_open_exits_zero_within_the_shutdown_bound`, `sigint_with_stdin_still_open_exits_zero_within_the_shutdown_bound`, `sighup_with_stdin_still_open_exits_zero_within_the_shutdown_bound`, `{sigterm,sigint,sighup}_leaves_clean_stdout_and_no_lock_or_file_behind`, `a_doctor_probe_stalled_at_{shutdown,sigint,sighup,stdin_eof}_leaves_no_child_behind`; `orch mcp --read-only --allow-destructive` exits 1, also from an uninitialised directory; docs check by the fact-checker; Linux CI + macOS |
| R9 | The PR shall ship an MCP chapter in `docs/manual/` (client configs: Claude Code `claude mcp add orch -- orch mcp`, Codex `config.toml`, generic), a README section, a CHANGELOG `[Unreleased]` entry, ADR `docs/decisions/0011-mcp-server.md`, this spec and its plan, ARCHITECTURE §3.3 module map and §4 dependency entry, and a RUNBOOK entry. | plan (AGENTS.md documentation duties) | must | fact-check report on the PR head; every listed file present in `git diff --stat main..HEAD`; macOS |
| R10 | `rmcp` shall be adopted under ARCHITECTURE §4.8 (maintenance, MSRV, licence recorded), with `cargo deny check` and `cargo xtask crate-graph` green. | approval q3 | must | `cargo deny check`; `cargo xtask crate-graph`; Linux CI + macOS |
| R11 | The CI set shall be green locally on macOS and on Linux CI, with a binary-level stdio test and unit tests per tool group. | plan (AGENTS.md pre-PR check) | must | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo xtask crate-graph`; `cargo deny check`; Linux CI + macOS |
| R12 | A real MCP client (Claude Code or MCP Inspector) shall initialise against `orch mcp`, list its tools and run `task_add` then `task_show` on a temp project. | item 3 | should | qa-engineer capture (transcript + `orch --json task show <id>`); macOS only, env-limited to the owner's Mac |
| R13 | When a goal with an assignee is paused (`orch goal status <id> paused`, the dashboard, or the `goal_status` tool), orch shall cancel only the assignee's `todo`/`retrying` autonomous tasks linked to that goal; the assignee's autonomous tasks linked to another goal, or to no goal, shall stay unchanged. | goal `goal_h-gIBCW` item 1 | must | engine unit tests in `crates/orch-engine/src/goal_service.rs`: `pausing_one_goal_spares_the_assignees_tasks_of_other_goals`, `pausing_the_last_active_goal_disables_autonomous_but_spares_other_goals_tasks`; the G1/G2 shared-assignee fixture through `goal_status` in default mode and in allow-destructive mode (`crates/orch-cli/tests/mcp_tools.rs`: `default_mode_pausing_g1_cancels_only_g1s_own_autonomous_tasks`, `allow_destructive_pausing_g1_is_scoped_to_g1_as_well`; R4); Linux CI + macOS |
| R14 | When a tool call is rejected as JSON-RPC invalid params because an argument has the wrong type or form, the error text, both in the client's response and in the server's stderr under `RUST_LOG=warn`, shall name the argument and the expected type and shall not contain the argument's value. | goal `goal_h-gIBCW` item 2; owner's plan answer q3=A (value-free in both) | must | unit test `mcp::registry::tests::decoding_reports_fields_and_types_without_argument_values`; stdio test under `RUST_LOG=warn` with a mistyped scalar carrying a sentinel value: response `-32602` names the field and expected type, and neither the response nor stderr contains the sentinel (`crates/orch-cli/tests/mcp_stdio.rs`: `a_mistyped_priority_is_invalid_params_naming_the_field_and_type_but_not_the_value`, `a_mistyped_priority_warn_line_on_stderr_names_the_field_and_type_but_not_the_value`, `a_mistyped_priority_value_appears_in_neither_the_response_nor_stderr`, `other_mistyped_scalars_name_field_and_type_but_never_the_value`, and `the_connection_serves_requests_after_a_mistyped_argument_was_refused` for the session afterwards); Linux CI + macOS |
| R15 | When `task_add` is called with `attach` and a nonempty entry resolves to a path that is not valid UTF-8, the tool shall return an error and create no task; it shall not panic, alter the path or drop the attachment. A relative entry is joined to the project root (D12), so this happens when the project root's path is not valid UTF-8 and the list has at least one relative entry; one such entry rejects the whole call. An absolute entry is used as given and does not depend on the root. Empty entries are dropped, so a list with no nonempty entry attaches nothing. | goal `goal_h-gIBCW` item 6; wording corrected 2026-11-08 by the CP2 ruling on fact-gate F2 (see [Open questions](#open-questions)) | must | unit test `mcp::tools::task::tests::task_add_rejects_non_utf8_attachment_root_without_creating_a_task` (Unix; a synthetic non-UTF-8 project root and one relative entry, `relative.txt`): an invalid-arguments tool error (`isError: true`) and no task created. The test does not cover absolute or empty entries under a non-UTF-8 root; that behaviour follows from `add` in `crates/orch-cli/src/mcp/tools/task.rs` and was checked once with a throwaway probe on 2026-11-08, which is not kept in the suite; Linux CI + macOS |
| R16 | When a client reads `orch://agents/{id}@{alias}` or `orch://inbox/{id}@{alias}`, the server shall return the resource as `application/json`, as for the unqualified form (R5). | goal `goal_h-gIBCW` item 7 | must | binary-level stdio tests against a seeded offline peer (`crates/orch-cli/tests/mcp_stdio.rs`): `a_qualified_agent_resource_returns_the_peer_agent_under_its_qualified_id`, `a_qualified_inbox_resource_returns_the_messages_addressed_to_the_qualified_id`; Linux CI + macOS |

### R3 exclusions

Every row is `won't` (no tool in any mode).

| Command | Reason |
|---|---|
| `run` | Long-running and spawns agent processes; orchestrator control is a non-goal. |
| `serve` | Long-running headless daemon that dispatches agents. |
| `tui` | Interactive full-screen terminal UI; needs a TTY that stdio MCP does not have. |
| `web` | Long-running, listens on a socket. |
| `init` | Creates a project; the server requires an initialised one (R1). |
| `update` | Host-mutating: downloads and installs a new `orch` binary. |
| `agent shop` | Interactive template picker by default. |
| `org` (`list`, `template …`, `deploy`) | Bulk agent creation from templates and template authoring on disk; deferred with `agent shop`. |
| `task edit`, `agent edit` | Interactive: open `$EDITOR`. `update`/`patch` cover the same change (R2). |
| `config edit` | Interactive: opens `$EDITOR`. |
| `config global` (`get`, `set`, `show`) | Reads and writes `~/.orchestry/global.yml`, host-wide state outside the project. |
| `remote add`, `remote remove` | Change remote-peer links; remote-peer control is a non-goal. |
| `remote listen` | Changes the peer listen setting, and so the network exposure of later engines. |
| `logs --follow` | Long-running live stream; `logs` without `--follow` is a tool (R2). Same class as R3. |
| `--file -` (`task`/`agent`/`config` `update`, `patch`) | Reads the document from stdin, which is the MCP transport (R1). `file: "-"` is rejected as JSON-RPC invalid params before stdin is touched; a regular file path works. |

Included tools whose behaviour R3 bounds:

| Tool | Bound |
|---|---|
| `doctor` (adapter probes) | The one permitted bounded spawning exception. Each probe runs with stdin null and piped output, so it cannot touch the transport, and is capped at 10 s; the whole call is capped at 120 s and past that returns a tool error. A failed check is report data, not `isError`. |
| `task_cancel` | Store-only: marks the task cancelled through the task service and never builds the full engine (no PID lock, R8). A live owner stops the run on reconciliation; with no live owner the tool does not kill an orphan agent process, as the CLI's `task cancel` would. |

### MCP-only result projections

For the R2 commands that print only a human message, the tool result is this projection (the CLI is unchanged):

| Tool | Result |
|---|---|
| `task_cancel`, `task_approve`, `task_reject`, `task_retry` | The `Task` after the write. |
| `agent_enable`, `agent_disable` | The `Agent` after the write. |
| `team_join`, `team_leave`, `team_add_task`, `team_set_lead` | The `Team` after the write. |
| `config_set` | `{key, value}`, the value as stored. |
| `agent_remove`, `team_disband`, `context_delete` | `null`. |
| `doctor` | The `orch --json doctor` report (`checks`, `adaptersReady`, `adaptersTotal`) without the header line. |

## Non-goals
- HTTP or SSE transport.
- Authentication: stdio inherits the permissions of the user who launched it.
- MCP prompts.
- Exposing `run` or other orchestrator control.
- Remote-peer control.
- Changing on-disk formats.

## Constraints
- On-disk: unchanged. Tools write through the same services as the CLI.
- CLI: one new subcommand, `orch mcp`, with `--project-root`, `--agent`, `--read-only` and `--allow-destructive`.
  Existing commands and their `--json` output are unchanged.
- `--json`: tool results reuse the existing `--json` shapes; no new envelope.
- Peer protocol: unchanged.
- Concurrency: the server is one more mutator alongside the CLI and `orch run --watch`, the same hazard as a
  CLI call (ARCHITECTURE §2.1); it takes no PID lock, only the stores' per-write file locks (R8).
- Platforms: macOS and Linux.

Security checklist:
- Default bind and exposure: no socket; stdio only, to the parent process that spawned it. No authentication;
  the client gets the launching user's access to the project.
- Bounds on input size, count and rate: frame size bounded by `rmcp`; field validation as the CLI (R7). No rate
  limit: the client is the local parent process.
- Permissions of files it writes: the project's existing store files, written by the same code paths as the
  equivalent CLI command; no new files.
- Secrets and environment passed to children: the `doctor` tool runs the same adapter probes as `orch doctor`;
  no other tool spawns a child.
- Untrusted text rendered to a terminal or passed to a shell: tool arguments come from an LLM client and are
  untrusted. They are never passed to a shell and never rendered to a terminal; stdout carries JSON frames only.
  Text stored through tools (messages, context, task descriptions) reaches agent prompts exactly as the same
  text stored through the CLI does; no new path.

## Verification environment
- Local (macOS) and Linux CI: R1–R8, R10, R11. Tests drive the built binary over stdin/stdout with a raw
  JSON-RPC client on a temp project and call no real AI CLI.
- R9: fact-checker on the PR head (macOS).
- R12: needs a real MCP client on the owner's Mac (Claude Code is installed there; MCP Inspector needs `npx` and
  network). Run by qa-engineer. Listing tools and calling `task_add`/`task_show` through MCP Inspector spends no
  model tokens; driving them through a Claude Code session does.
- No dashboard behaviour changes; no tmux test is needed.

## Open questions
- **Resolved by lead decision-1**, 2026-11-07 (recorded in the goal's ateam artefacts): CLI parity for
  commands without JSON output (MCP-only projections, R2), `task_cancel`, `doctor` probes and `--file -` (R3),
  `goal_status` `force=true` and annotations (R4), `config update` and `config patch` (added to R2), and
  read-only maintenance writes (R8).
- **Resolved by lead decision-2**, 2026-11-07 (recorded in the goal's ateam artefacts):
  - E1 (superseded 2026-11-08, see below): at the time, pausing a goal cancelled the assignee's todo/retrying
    autonomous tasks in every goal, so `goal_status` with `status=paused` joined `force=true` behind
    `--allow-destructive`, with that cancellation disclosed (R4), and a lead or orchestrator-type client that
    paused goals registered `orch mcp --allow-destructive`.
  - E2: the residual concurrent-store races (whole-record lost updates, context expiry against concurrent
    writes, the process-wide directory cache) are accepted as the existing hazard of concurrent `orch`
    writers and documented, with recovery in the RUNBOOK (R8). Storage repair is follow-up work outside this
    spec.
- R2, R3, R4 and R8 were amended by those rulings. The [specs README](README.md#lifecycle) says changing an
  approved `must` row needs the owner. **Confirmed by the owner, 2026-11-08**, in goal `goal_h-gIBCW` item 5:
  "Spec amendments to R2/R3/R4/R8 made by lead decision-1 and decision-2 are confirmed by the owner." Recorded in
  [Verification](#verification) too.
- **E1's resolution is superseded**, 2026-11-08, by goal `goal_h-gIBCW` item 1 (owner-authorised, quoted in
  [Source](#source)): pausing a goal cancels only that goal's own todo/retrying autonomous tasks (R13), so
  `goal_status` with `status=paused` is allowed in default mode and only `force=true` stays behind
  `--allow-destructive` (R4 re-amended). A lead-type client that pauses goals no longer needs
  `--allow-destructive`.
- **SIGHUP.** The server has exited cleanly on SIGHUP as well as SIGTERM/SIGINT since PR #69; the owner
  confirmed this behaviour on 2026-11-08 (goal `goal_h-gIBCW` item 4) and R8 names it.
- **`--read-only` together with `--allow-destructive`.** Resolved by lead decision-2 (F): a usage error,
  exit 1 (R8).
- **R15 wording corrected**, 2026-11-08, by the lead's CP2 ruling on fact-gate finding F2, under goal
  `goal_h-gIBCW` item 6 ("handle it (error cleanly, never panic or silently drop) with a test"). The first
  wording said every `attach` call fails when the project root is not valid UTF-8. The code fails only when a
  nonempty entry *resolves* to a non-UTF-8 path, which means a relative entry under such a root; absolute
  entries keep their D12 authority and a list with no nonempty entry attaches nothing. The guarantee item 6 asks
  for (a clean error, no panic, no silent drop, no task when a path is rejected) is unchanged. No code change.

## Verification
R1–R12: PR #69 ([yuxhuang/r-orch#69](https://github.com/yuxhuang/r-orch/pull/69), merged as `679011d`), head
`42d2808`. "macOS" is the local cold `./xtask/gate.sh` on macOS 27.2 arm64 (5,579 tests passed, 0 failed); "Linux
CI" is GitHub Actions `ubuntu-latest` x86_64, run 42430144474, jobs `check` and `test` passed at `42d2808`. The
amendments that lead decision-1 and decision-2 made to R2, R3, R4 and R8 were **confirmed by the owner on
2026-11-08** (goal `goal_h-gIBCW` item 5).

R4 (amended 2026-11-08), R8 (amended 2026-11-08) and R13–R16: the follow-up PR of goal `goal_h-gIBCW`, branch
`fix/mcp-followups`, frozen code commit `514560d`. "macOS" is `cargo test --workspace` (5,618 passed, 0 failed, 4
ignored) and the cold `./xtask/gate.sh` (all 9 steps pass) on macOS 27.2 arm64 at `514560d`, recorded in the goal's
review packet. Linux CI runs when the PR is opened; the PR records that run.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | pass | `42d2808` | macOS + Linux CI |
| R2 | pass; decision-1 amendment confirmed by the owner 2026-11-08 | `42d2808` | macOS + Linux CI |
| R3 | pass; decision-1 amendment confirmed by the owner 2026-11-08 | `42d2808` | macOS + Linux CI |
| R4 | pass, as worded at `42d2808` (default mode refused `status=paused` and `force=true`); decision-1 and decision-2 amendments confirmed by the owner 2026-11-08; re-amended 2026-11-08, see the R4 (amended) row | `42d2808` | macOS + Linux CI |
| R5 | pass | `42d2808` | macOS + Linux CI |
| R6 | pass | `42d2808` | macOS + Linux CI |
| R7 | pass | `42d2808` | macOS + Linux CI |
| R8 | pass, as worded at `42d2808` (stdin EOF, SIGTERM, SIGINT; the SIGHUP tests were already in the suite); decision-1 and decision-2 amendments confirmed by the owner 2026-11-08; amended 2026-11-08, see the R8 (amended) row | `42d2808` | macOS + Linux CI |
| R9 | pass: fact-checker-2 reviewed the docs at `847d823` and `0b62a00` (conditions, fixed in the PR) and the final delta at `42d2808` (pass); every listed file is in the PR #69 diff | `42d2808` | macOS |
| R10 | pass: `cargo deny check` (0.20.2) and `cargo xtask crate-graph` | `42d2808` | macOS + Linux CI (`check` job) |
| R11 | pass: the CI set and the cold gate | `42d2808` | macOS + Linux CI |
| R12 | pass: qa-engineer ran MCP Inspector 2.14.0 (CLI mode, no model) against the built binary: initialise, `tools/list`, `task_add`, `task_show`, compared with `orch --json task show` | `847d823` | macOS arm64 only (the owner's Mac; not run on Linux) |
| R4 (amended 2026-11-08) | pass on macOS (`cargo test --workspace` and `./xtask/gate.sh`, review packet); Linux CI pending the PR | `514560d` | macOS; Linux CI pending the PR |
| R8 (amended 2026-11-08) | pass on macOS (`cargo test --workspace` and `./xtask/gate.sh`, review packet); Linux CI pending the PR | `514560d` | macOS; Linux CI pending the PR |
| R13 | pass on macOS (`cargo test --workspace` and `./xtask/gate.sh`, review packet); Linux CI pending the PR | `514560d` | macOS; Linux CI pending the PR |
| R14 | pass on macOS (`cargo test --workspace` and `./xtask/gate.sh`, review packet); Linux CI pending the PR | `514560d` | macOS; Linux CI pending the PR |
| R15 (wording corrected 2026-11-08) | pass on macOS (`cargo test --workspace` and `./xtask/gate.sh`, review packet); the corrected wording matches what the cited test checks, and `task.rs` is unchanged since `514560d`; Linux CI pending the PR | `514560d` | macOS; Linux CI pending the PR |
| R16 | pass on macOS (`cargo test --workspace` and `./xtask/gate.sh`, review packet); Linux CI pending the PR | `514560d` | macOS; Linux CI pending the PR |

Defects found outside the requirements:
- PR #69 carried these as accepted residuals or follow-ups; goal `goal_h-gIBCW` handles them. Cross-goal
  cancellation on pause (E1): fixed, R13 and R4. Invalid-params text echoing a mistyped value on stderr: fixed,
  R14. Relative `task_add` attachments under a non-UTF-8 project root: fixed, R15. No stdio success test for qualified
  resources: added, R16. `yaml_contract` needing git ≥ 2.42 (gate-ci N4): test fixture fixed, no requirement row.
  An unverified client-version claim in the manual: rechecked by the fact-checker. The non-transactional store
  (D10) stays outside both goals.
