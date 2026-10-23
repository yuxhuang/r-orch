# Token eco mode, adapter effort levels and model defaults

status: active
owner: orchestrator (lead)
date: 2026-10-22
spec: [../specs/2026-10-22-token-eco-mode.md](../specs/2026-10-22-token-eco-mode.md)

## Goal
An opt-in `eco` prompt mode sends a compact, stable-first prompt with the same orchestration as `normal`; `normal`
stays byte-identical to v0.9.4. Independently of the mode, every adapter forwards the effort levels its current
CLI documents, validated when an agent is written, and new agents get current default models.

## Non-goals
Session reuse, cheaper models or lower budgets in eco, custom-template translation, a provider capability
service, rewriting stored agents. See the spec.

## Context
- Branch `feature/token-eco-mode` from `main` at `d8c2319` (v0.9.4).
- Baseline measurement (M0, frozen 28-agent fixture): median total rendered input 241,478.5 UTF-8 bytes (60,370
  estimated tokens at bytes/4), of which the Team block, every other agent's full role, is 226,440 bytes (93.77%).
  These are rendered-size estimates, not billed tokens. In the same measurement a second identical Claude run
  read 108,041 tokens from cache with 2 new input tokens, so the user half of a prompt can already be cached.
- Research and design inputs: provider practice research, effort/model research, session-reuse measurement, the
  final design and the lead's rulings 1–7. They are kept with the team's working files, not in the repo.

## Design

**Prompt mode (D1).** `PromptMode { Normal, Eco }` in `orch-domain`; project value `defaults.agent.prompt_mode`
(serde default `normal`, written by `orch init`), optional agent override `config.prompt_mode` (absent means
inherit, never copied from the project). Dispatch reads the project value once per dispatch, after the agent is
chosen, through an injected `ConfigStore`, and captures it on the invocation's config clone, so a running child
keeps its mode and the next dispatch sees a change made by another process. A read failure aborts that dispatch
before any run is created: the task stays `todo` with its attempts unchanged, the error is reported as
`orchestrator:error`, and the next tick tries again. Project writes (`config set`, `config patch`, `F3`) re-read
the raw file inside the definition lock, change one key, validate the merged result and publish atomically;
unknown keys survive. Eco plus any custom template (`prompt.template`, `system_template`, `user_template`, project
or agent) is refused at write time with every offending agent listed, and again at dispatch for hand-edited
files (the same no-run path as a read failure).

**Normal byte identity (D2).** `system.liquid`, `user.liquid`, the normal context builder and the fixtures under
`fixtures/prompts/` are unchanged. Eco is a separate pair of templates and a pure projection
(`orch-infra/src/template/eco.rs`) selected by the context; the shared entry point is `render_prompt`.

**Eco layout (D3).** System half: CLI reference plus record-lookup commands, unchanged rules, mandatory-lookup
rules, the concise-reporting instruction, project, roster, the agent's own full role and skills, workspace and
the same ≤15 shared-context entries in full. User half: task, retry, feedback, inbox, autonomous instructions,
goal. Roster rows are sorted by agent id and JSON-encoded as data. Each row's `summary` is `config.role_summary`
(≤192 bytes, whitespace-normalized) or the first sentence of the role's first paragraph after skipping headings
and the exact shared-rule preamble, clipped to 192 bytes with `[truncated, N more bytes]`. Retry output is
clipped to 8,192 bytes (head 2,048) only with the same failed run's id and a finished collector; the prompt then
requires `orch --json logs <run-id>` before any retry action. The goal preview keeps at most 40 rows and 12,288
bytes and points to `orch --json goal show <goal-id>`.

**Transport (D4).** No new flags. Claude, Pi and Grok keep their native system channel; Codex, OpenCode,
Antigravity and Cursor keep concatenating system and user text.

**Effort (D5).** One table in `orch-domain/src/effort.rs` (checked 2026-10-22) gives each adapter's authorable
levels, native selector, exact-id model rows, unknown-model policy and evidence status. Authoring validates the
final candidate for every mutation surface. Dispatch forwards new values strictly and treats stored
`low|medium|high` as legacy: forwarded where representable, otherwise ignored with a `legacy-ignored` warning.
Adapters build argv from the table: Claude/Antigravity/Grok `--effort`, Pi `--thinking`, Codex `-c
model_reasoning_effort="<level>"`, OpenCode `<model>#<level>`. OpenCode alone overrides the new async
`Adapter::prepare_effort` hook: before synthesizing a suffix it runs `opencode --version` and `opencode run
--help` (2 s and 64 KiB each, process group killed on failure) and requires major version 2 and the
`provider/model#variant` signature. `execute` stays synchronous and returns the model child's PID. Diagnostics
use five statuses (`request-only`, `clamp-possible`, `local-override-possible`, `capability-unknown`,
`legacy-ignored`) and never claim an effective level. They are never agent output: each run reports its own once,
before the model starts, as a non-fatal `orchestrator:error` with context `effort preparation for run <run> (agent
<agent>)` (lead ruling 8).

**Models (D6).** `orch-domain/src/model_tiers.rs` changes the static capable/balanced/fast choices for new
selections only; stored models are never resolved through it.

**Session reuse (D7).** <a id="session-reuse"></a>Not shipped. One measured pair per CLI (warm cache, fixed order,
no tool calls): Claude new input plus cache write 615 fresh vs 671 resumed, wall 1.503 s vs 1.557 s; Codex
uncached input 8,815 vs 2,992 (66.1% less) but total input 21,103 vs 21,680 (2.7% more), wall 3.333 s vs
3.391 s. Only resumed runs recalled an earlier user-only nonce, so resume carries old history into new work.
OpenCode's pair was excluded (its session directory did not match the working directory). The Codex saving
does not establish a lifecycle-safe design; a future default-off setting needs its own design.

Rejected, one line each: a single template with mode conditionals (whitespace drift in normal); regenerating old
goldens (destroys the oracle); LLM-written summaries (cost, nondeterminism); dropping the roster (loses
discovery); byte caps on shared context (no recovery for overwritten or expired values, ruling 5); a hard total
prompt cap (cuts task evidence); strict dispatch failure for legacy effort (breaks unattended runs); copying the
project mode into agents (a later switch would not apply).

## Tasks

| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| B1 | Domain mode, role summary and effort contracts | R1, R3, R8 | `crates/orch-domain/src/{agent,config,effort,lib}.rs` | — | `cargo test -p orch-domain`; `cargo clippy -p orch-domain --all-targets -- -D warnings` | 1 |
| B2 | Adapter selectors, `prepare_effort` hook, OpenCode probe, model tiers | R2, R8, R12 | `crates/orch-infra/src/adapters/**`, `crates/orch-infra/src/models/**`, `crates/orch-core/src/{adapter,lib}.rs`, `crates/orch-domain/src/model_tiers.rs`, `crates/orch-domain/src/shop/**` | B1 | `cargo test -p orch-core --lib`; `cargo test -p orch-infra --lib adapters::`; `cargo test -p orch-infra --test adapter_opencode` | 1 |
| B3 | Eco projection and templates, dispatch mode refresh, hook wiring, validation | R1–R6 | `crates/orch-engine/src/**`, `crates/orch-infra/src/template/**`, `crates/orch-core/src/{template.rs,test_support/**}`, `crates/orch-infra/src/storage/config.rs` | B1, B2 | `cargo test -p orch-engine --lib`; `cargo test -p orch-infra --lib`; `cargo test -p orch-infra --test prompt_golden` | 1 |
| B4 | CLI and dashboard surfaces, `F3`, warnings | R1, R8, R11 | `crates/orch-cli/src/**`, `crates/orch-tui/src/**` | B3 | `cargo test -p orch-cli --lib`; `cargo test -p orch-tui --lib`; tmux `F3` checks | 1 |
| T1 | External tests: frozen baseline, compatibility matrix, real-reader retry recovery, caller audit | R2, R5, R8, R13 | `crates/*/tests/**`, `fixtures/prompts-eco/**`, `fixtures/token-eco/**` | B4 | `cargo test --workspace --no-fail-fast` | 1 |
| W | Spec, plan, manual, command reference, CHANGELOG | R10 | `docs/**`, `README.md`, `CHANGELOG.md` | B4 | `git diff --check`; examples re-run on the B4 commit | 1 |
| I | Integrate, run the CI set, freeze the reviewed commit | all | — | T1, W | AGENTS.md pre-PR check | 1 |
| P | R9 cold ratio, warm counters, OpenCode probe latency | R4, R9 | measurement only | I | perf-eng harness on `render_prompt` | — |
| E | Held-out normal/eco outcome evaluation | R6 | evaluation only | I, owner-approved budget | QA report | — |

The PR is about one concern (prompt modes and the effort contract) but exceeds 3,000 changed lines: the four build
slices share one compile dependency chain (B1 → B4), and the workspace does not build between them.

## Status

| Item | State |
|---|---|
| B1 | implemented, `7df6879`; domain tests pass on macOS |
| B2 | implemented, `a813a54`; adapter tests pass on macOS in a prerequisite copy |
| B3 | implemented, `6400433`; engine, infra and `prompt_golden` pass on macOS |
| B4 | implemented, `7f96bdd`; CLI and TUI lib tests pass on macOS; `orch serve` start-up diagnostics `784169c`, record fields renamed `agentId`/`agentName` in `3b03c62` |
| T1 | in progress |
| W | committed with this plan; examples re-run on `3b03c62` in disposable projects |
| F1 | implemented: effort diagnostics left the agent-output stream (lead ruling 8); adapters `72125b1`, engine `6ae9839` (one non-fatal `orchestrator:error` per run before spawn); manual and spec updated in the W follow-up |
| I, P, E | pending |

## Test plan
The spec's acceptance checks; the AGENTS.md CI set on Linux CI and macOS; `F3` and the wizard in tmux; the cold
`./xtask/gate.sh`, because process handling changes (the OpenCode probe). Paid measurements (P, E) run only
with the owner-approved budget and are not part of CI.

## Rollback / compatibility
- **Back to normal prompts:** `orch config set defaults.agent.prompt_mode normal` and `orch agent update <id>
  --prompt-mode inherit` for each eco override. Applies from the next dispatch; running children finish in their
  mode; tasks created in eco stay.
- **Effort and models are separate from the mode.** Codex and Antigravity now forward a stored effort, and
  OpenCode may append `#<level>`; clearing an agent's effort (`--effort default`) restores v0.9.4 argv for it.
  New default models apply only to agents created after the upgrade.
- **Downgrade to v0.9.4:** v0.9.4 cannot read `config.effort` values other than `low|medium|high`; clear them with
  the new binary first (`--effort default`), or restore the definitions from a backup. Do not map `max` or `ultra`
  to `high` by hand. v0.9.4 ignores or drops `prompt_mode` and `role_summary` when it rewrites a file.
- **Peers:** no protocol change; a v0.9.4 peer can be mixed. Remote dispatch ignores the mode.

## Docs to update
Spec and this plan; [manual chapter 16](../manual/16-eco-mode-and-effort.md); chapters 3, 8, 12, 13 and the
regenerated command reference; `docs/file-mutations.md`; `README.md`; `CHANGELOG.md`.
[ADR 0010](../decisions/0010-prompt-mode-and-effort-contract.md) (proposed) records the effort contract and the
`prepare_effort` trait addition ([TRAITS-FROZEN.md](../TRAITS-FROZEN.md), Amendment 2); `docs/decisions/` is a
process file the owner merges (AGENTS.md, merge policy).

## Open questions
- R9 and the warm-cache and outcome results land in a follow-up docs change after the freeze.
- B3 points the goal recovery command at `orch --json goal show <goal-id>` (it includes dependencies) instead of
  the design's `orch task list --goal-id`; kept unless the lead decides otherwise.
