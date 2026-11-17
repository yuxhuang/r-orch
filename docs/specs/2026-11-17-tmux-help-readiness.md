# Tmux help content readiness

status: approved
owner: test-engineer
date: 2026-11-17
plan: [../plans/2026-11-17-tmux-help-readiness.md](../plans/2026-11-17-tmux-help-readiness.md)

## Source

Approved bounded repair instructions:

> 1. Preserve both terminal sizes, exact original assertions and existing deadline.
> 2. Require title and complete required disclosure in the same captured screen.
> 3. Implement deterministic prefix-then-complete and permanently missing/each-sentence-missing controls through a minimal test-local capture/clock seam only if needed; reject old title-only mutant.
> 4. Do not change tmux commands, subprocess/environment/cleanup, shared helpers or production code. No fixed-sleep workaround or retry.

## Requirements

| Id | Requirement | Source | Priority | Acceptance check (test or command; platform) |
|---|---|---|---|---|
| R1 | At 120×40 and 80×24, help readiness requires the title and all eight existing disclosure/footer lines in one captured screen. Preserve all existing assertions, including the 80-column constraint. | 1, 2 | must | `tables_headers_sort_and_elapsed_in_tmux`; macOS focused target, Linux CI follow-up |
| R2 | A title-only prefix followed by complete content returns the complete capture; complete content is accepted immediately. | 3 | must | `help_readiness_` deterministic controls; macOS |
| R3 | A permanent title-only screen, each independently missing required line, a missing title, each independently missing disclosure sentence, and incomplete content split across captures fail at the existing twenty-second deadline with the final screen retained. | 1, 3 | must | `help_readiness_` fake-clock negative controls; macOS |
| R4 | Polling retains capture errors, process behavior, deadline and existing 100 ms interval. Only the owned test file and these two documents change. | 4 | must | Diff inspection, capture-error control, `cargo fmt --all --check`; macOS |
| R5 | The old title-only readiness mutant is rejected by the deterministic regression. | 3 | must | Temporarily restore title-only predicate, run controls, preserve failure, restore fix |

## Non-goals

No production rendering change, transport-causation claim, timeout increase, retries, new dependencies, shared helper changes, workflow change or performance claim.

## Constraints

No CLI, protocol or data-format change. The partial terminal frame explanation remains provisional: the observed title-only acceptance defect independently justifies this test repair. Existing deadline and subprocess/cleanup behavior remain unchanged. No CHANGELOG entry is required for this test-only change.

Security checklist: n/a; the existing isolated tmux and child-process setup is unchanged, and synthetic controls start no processes.

## Verification environment

The builder runs formatting and deterministic controls on macOS, then explicitly builds the CLI and runs `cargo test -p orch-cli --test tui_tmux_qol2_tables` once with an isolated target, HOME and private tmux servers. Linux CI and main acceptance remain follow-ups owned by the monitor. The current owner exception defers the full local CI set and cold gate; they are not claimed green. Native terminal profiles, native clipboard and browser behavior are outside this test change.

## Open questions

None for this bounded repair. If real disclosure content never arrives, preserve the first failure and block for a separately scoped frontend investigation.

## Verification

Scoped macOS checks passed. The real tmux target passed once (10 tests) before adding one sentence-omission control; its polling loop, readiness predicates and all original scenarios are byte-identical afterward. All eight final deterministic controls pass. The builder handoff records the exact frozen commit and source hashes. Linux acceptance remains pending, so this spec remains approved.

| Id | Result (`pass` / `env-limited` / `waived by owner`) | Commit the check ran at | Platform |
|---|---|---|---|
| R1 | pass: both sizes in the single tmux target | Pre-commit repair tree based on `9255a0e`; runtime code unchanged afterward | macOS |
| R1 | env-limited: Linux CI/main acceptance pending | Not run | Linux CI |
| R2–R3 | pass: eight controls, including eight line and seven independent sentence omissions | Final pre-commit test blob `45c854538bf0ec4529bde765edbf5aa0c9e5792c` | macOS |
| R4 | pass: formatting, preserved assertions/process code, capture-error control | Same repair tree | macOS |
| R5 | pass: title-only predicate rejected, including the final sentence control | Temporary mutants of the repair tree, restored | macOS |

Defects found outside the requirements: none established; transport cause remains unproven.
