# Token eco mode, adapter effort levels and model defaults

status: approved
owner: orchestrator (lead), for the repository owner
date: 2026-10-22
plan: [../plans/2026-10-22-token-eco-mode.md](../plans/2026-10-22-token-eco-mode.md)

## Source
> Review token usage related code in r-orch, web research best practices of claude code, codex, antigravity, and
> opencode; and implement the best practices of token usage of these latest agent clis.
>
> 1. You should provide a mode switch so we can use the normal mode (so does not change the current behavior,)
>    and an eco mode - which will opt-in to token saving behaviors. Rely on best practices from the agent
>    providers; as well as other sources.
> 2. The eco mode should be as close to the normal mode as possible in terms of agent orchestration / approaching
>    the same outcome.
> 3. Not specific to eco or normal mode, update the effort level settings of each agent adapter to their latest
>    configurations and levels.

Owner's approval reply, 2026-10-22 (approves R1–R9 and adds the items that became R11–R13):
> tier=high stance=balanced pr=open q1. use recommendation; also add a shortcut in TUI to switch. q2. allowed. and
> if you need to measure cache for codex / opencode, feel free as well. others - a. also update default models for
> known adapters, if they are not dynamic; b. test session reuse with resume across different adapters. if it
> works better, implement that as part of the eco mode, or both modes, or as an additional setting, whatever works
> best.

R2, R4, R5, R8 and R9 carry the lead's rulings of 2026-10-22 and 2026-10-23 (R2: rulings 1 and 24; R4: ruling 23;
R5: ruling 5; R8: rulings 2, 6 and 7; R9: ruling 3); the original wording is kept in the source plan. See Open
questions.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check (test or command; platform) |
|---|---|---|---|---|
| R1 | orch shall have a prompt mode `normal` (default) or `eco`, set per project in `defaults.agent.prompt_mode` and overridable per agent in `config.prompt_mode`; an agent with no override follows the project value at each dispatch. `orch init`, `orch agent add`, `orch agent update`, the wizard and `orch --json agent show <id> --effective` shall surface it. | item 1; approval q1 | must | `crates/orch-domain` inline serde/default tests; CLI examples in [manual ch. 16](../manual/16-eco-mode-and-effort.md); T1 suite; macOS |
| R2 | In `normal` mode the rendered system and user prompts, stdin and system-prompt-file bytes shall be byte-identical to v0.9.4 (`d8c2319`) for the same inputs. Adapter argv shall be identical too, except these named effort deltas: Codex `-c model_reasoning_effort="<level>"` and Antigravity `--effort <level>` when the agent stores an effort; OpenCode `<model>#<level>` when it stores an effort, an explicit known model that lists the level, and the installed CLI passes the v2 wire check. An agent with no stored effort has identical argv on every adapter. For an OpenCode agent with a stored effort and a listed model, normal mode also runs the two bounded version checks (`opencode --version`, `opencode run --help`) before the model child. Named exception (ruling 24): the retry previous output of a failed run whose event file exceeds 64 KiB carries that run's correct last 50 events (`d946a66`). | item 1; rulings 1, 24 | must | `cargo test -p orch-infra --test prompt_golden` (normal goldens unchanged); T1 frozen argv/stdin captures from `d8c2319`; Linux CI + macOS |
| R3 | In `eco` mode the team roster shall keep every agent's id, name, adapter and capabilities with a one-line role hint of at most 192 UTF-8 bytes instead of its full role; the agent's own full role and loaded skills stay in its prompt, and the prompt requires `orch agent show <id>` before any role-dependent routing decision. | item 1 | must | eco render tests in `crates/orch-infra/src/template/`; T1 28-agent fixture; macOS |
| R4 | In `eco` mode the stable blocks (CLI reference, rules, project, roster, the agent's own role and skills, workspace) shall come first, in the system half where the adapter has a native system channel, and task-specific text and shared context (ruling 23) in the user half. No cache-hit rate is promised; a positive Claude cache read on a repeated run is reported as measured. | item 1; ruling 23 | must | eco template tests; `crates/orch-infra/tests/token_eco_prompt_eco.rs::eco_shared_context_keys_and_full_values_are_only_in_the_user_half_for_every_adapter`; perf-eng warm run (real CLI, owner-approved spend) |
| R5 | In `eco` mode the previous-attempt output of a retry shall be clipped to 8,192 bytes (2,048-byte head, tail, middle marker) only when the source failed run is known, and the goal task list to 40 rows / 12,288 bytes, each with a `[truncated, N more …]` marker and the `orch` command that fetches the rest. Shared-context entries (the same ≤15 as normal) and goal progress shall be kept in full. Without a source run id, retry output is kept in full. | item 1; ruling 5 | must | `crates/orch-engine/src/orchestrator/tests/token_eco.rs::the_retry_context_names_the_same_failed_run_its_evidence_came_from`, `::a_still_draining_collector_withholds_the_source_so_eco_keeps_full_output`; T1 real-reader `orch --json logs` window test; macOS |
| R6 | The prompt mode shall not change scheduling, the task graph, scopes, reviews, retries, timeouts, permissions or which agent is picked. | item 2 | must | `crates/orch-engine/src/orchestrator/tests/token_eco.rs::equal_outputs_give_an_identical_engine_trace_in_both_modes`; held-out outcome evaluation E2 (pending, separate budget) |
| R7 | `eco` shall add only verified prompt-placement changes and one concise-reporting instruction; it shall not lower effort, model, turns, timeouts or output limits, or add unverified CLI flags. | item 1 | should | eco adapter argv equals normal argv for the same config (T1) |
| R8 | `config.effort` shall accept `none`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`, validated per adapter and model against a dated capability table when an agent is added or changed; an unsupported value is refused before anything is saved. Codex `ultra` needs an explicit known model and is never chosen by eco. Grok accepts only `low`, `medium`, `high` (evidence-limited). A stored `low`, `medium` or `high` written by v0.9.4 shall never make a dispatch fail: it is forwarded where the adapter can, otherwise ignored with a warning. Every forwarded effort is a request; orch shall not report an effective reasoning level. | item 3; rulings 2, 6, 7 | must | `crates/orch-domain/src/effort.rs` cross-product tests; `crates/orch-infra/src/adapters/` selector and probe tests; T1 compatibility matrix; macOS |
| R9 | On M0's frozen 28-agent fixture (SHA-256 `33f89dc7b4f457e9b7f40fe51cd86bae845e3440bd12c9a5e2ed9e653329d2a0`), the aggregate and the median ratio of eco to normal rendered system+user UTF-8 bytes shall each be at most 0.25 (worst agent reported). Warm-cache counters are reported separately and are not this gate. | approval; ruling 3 | should | perf-eng harness on the production renderer |
| R10 | The spec, plan, manual chapter, command reference and `CHANGELOG.md` `[Unreleased]` shall describe the shipped behaviour, compatibility and rollback; measured numbers appear only after their gate ran. | plan | must | this document set; fact-check reports |
| R11 | When `F3` is pressed in ordinary navigation, the dashboard shall toggle the project prompt mode under the definition lock, keep agent overrides, apply it from the next dispatch, refuse while a change is pending, and leave the display unchanged on failure. | approval ("shortcut in TUI") | must | `crates/orch-tui` inline tests; QA tmux pass |
| R12 | The static tier-to-model table shall name current models per adapter; it applies to new selections only (shop, wizard, `orch init`'s Agent Creator) and never rewrites a stored `model`. | approval item a | must | `crates/orch-domain/src/model_tiers.rs::static_choices_match_the_dated_catalog_for_all_tiers`; macOS |
| R13 | Session reuse was measured and is not shipped: every dispatch in both modes starts a fresh CLI session, and orch shall pass no resume, continue or session argument. | approval item b | must | T1 D7 tests; [measurement summary](../plans/2026-10-22-token-eco-mode.md#session-reuse) |

## Non-goals
- Automatic or configurable session reuse (R13).
- Cheaper models, lower turn or output budgets, early compaction or cache-TTL tuning in eco.
- Rewriting custom prompt templates for eco: eco with `prompt.template`, `prompt.system_template` or
  `prompt.user_template` is refused.
- Universal effort discovery, or verifying the effective reasoning level a provider uses.
- Changing existing agents' models or roles.

## Constraints
- On-disk: `defaults.agent.prompt_mode` (written by `orch init` and any config write), `config.prompt_mode` and
  `config.role_summary` are additive; old files read as `normal` with no override. Files with the new effort
  values or fields are not readable by v0.9.4; see the plan's rollback section.
- CLI/`--json`: ordinary `agent show`/`list` JSON keeps its shape; `--effective` adds a new envelope. Warnings never
  appear in JSON or YAML stdout.
- Effort diagnostics (lead ruling 8): not provider output. Authoring (`agent add`/update) and `agent show` print
  them as `⚠` lines on stdout in human output and as `warning: …` lines on stderr under `--json`/`--yaml`/`--quiet`;
  `agent list` prints them on stdout after the table in human output and omits them under `--json`/`--yaml`/
  `--quiet`. The start of `orch run` prints them to stderr in every mode; the dashboard shows them in its Activity
  feed; `orch serve` logs one `serve:diagnostic` record per warning, not per agent (`agentId`, `agentName`,
  `message`; an agent with two warnings gets two records) after `serve:started`. Each dispatch reports its own
  warnings (`is_warning()`), once each, before the model starts, as a non-fatal `orchestrator:error` bus event
  (`error` = the diagnostic, `context` = `effort preparation for run <run> (agent <agent>)`, `fatal: false`),
  shown by the `serve` log and `orch run --watch` (ruling 20). The informational request-only note is never
  reported at run start; `agent show` prints it. They are never `agent:output` or run-log events, so a run's
  result and a retry's previous output cannot contain them.
- Peer protocol: unchanged; remote and mirror dispatch ignore the prompt mode.

Security checklist:
- Default bind and exposure: n/a (no listener).
- Bounds on input size, count and rate: role hint ≤192 bytes; OpenCode wire probe 2 s and 64 KiB per child, two
  children; retry and goal previews as R5.
- Permissions of files it writes: existing owner-only atomic definition writes.
- Secrets and environment passed to children: probes use the agent's execution environment and `inherit_env`;
  diagnostics carry static reasons only, never probe output or environment values.
- Untrusted text rendered to a terminal or passed to a shell: roster and shared-context rows are JSON-encoded
  data lines; no shell interpolation in probes.
- Prompt role of agent-writable text: eco keeps shared context in the user half, never in a provider's system
  prompt, and so for Pi on stdin rather than in argv (ruling 23).

## Verification environment
Unit, integration and fake-process tests run on macOS and Linux CI and call no real AI CLI. The dashboard `F3`
path is checked in tmux. The R9 cold ratio needs no provider. Warm-cache counters (R4) and the held-out outcome
evaluation (R6) need real CLIs and owner-approved spend, run by perf-eng and QA on the owner's machine.

## Open questions
- R2, R4, R5, R8 and R9 were amended by lead rulings (R2 by rulings 1 and 24, R4 by ruling 23). AGENTS.md says
  changing an approved `must` row needs the owner: the owner confirms these amendments when reviewing the PR.
- Grok effort levels beyond `low|medium|high` need a primary source or an installed binary (follow-up).

## Verification
The board gates ran on the frozen integration commit `c2f6962`. Their conditions were fixed in the CP3 condition
round; a row naming the fix-round head is re-checked by the delta re-review on the head the PR names. `pending`
means no gate has passed the row yet.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | pass | c2f6962 (T1 D1-1…D1-5, QA gate) + fix-round head (delta re-review) | macOS |
| R2 | pass | c2f6962 (board, fact-check R2-a..j) + fix-round head (delta re-review) | macOS; Linux pending CI |
| R3 | pass | c2f6962 (T1 D3-1…D3-8) | macOS |
| R4 | pass | fix-round head (`f4f81bf` placement + T1 vector `token_eco_prompt_eco.rs::eco_shared_context_keys_and_full_values_are_only_in_the_user_half_for_every_adapter`; delta re-review) + c2f6962 (Claude second-read cache > 0 in both eco pairs; OpenCode warm not measured) | macOS |
| R5 | pass | c2f6962 (T1 D3-9, D3-10 with the real `orch --json logs` reader) | macOS |
| R6 | blocked | engine-trace test passes at c2f6962 (orchestration equality, T1); held-out outcome evaluation E2 not run: 2com Linux containment matrix partial (75/145; the frozen confinement helper needs CAP_MKNOD and a uid map an unprivileged user cannot grant), SRE and security reviews block, 0/7 route-1 prerequisites closed; environment decision with the owner (ateam escalation-1) | macOS (engine trace); Linux 2com partial |
| R7 | pass | c2f6962 (T1 D2-2 pairing) | macOS |
| R8 | pass | c2f6962 (T1 compatibility matrix, 544 cells, real 0.9.4 reader) + fix-round head (ruling-13 shell row, delta re-review) | macOS |
| R9 | pass | c2f6962 | macOS (perf gate, cold 0.089) |
| R10 | pass | df3c712 (fact-check gate at c2f6962 passed with conditions; conditions re-checked on 8757de3 and df3c712, both pass; Pensieve spec copy byte-identical) | macOS |
| R11 | pass | c2f6962 (QA gate, tmux) + fix-round head (delta re-review) | macOS |
| R12 | pass | c2f6962 (`static_choices_match_the_dated_catalog_for_all_tiers`) | macOS |
| R13 | pass | c2f6962 (T1 D7-1, D7-2) | macOS |

Defects found outside the requirements:
- JSONL tails of run event files larger than 64 KiB could repeat or reorder records; fixed in `d946a66`
  (`crates/orch-infra/src/storage/fs_utils.rs`). It affects `orch logs` (run, `--task`, `--agent`, `--since`),
  the dashboard's run history and, in both modes, a retry's previous output; it is the named R2 exception
  (ruling 24).
