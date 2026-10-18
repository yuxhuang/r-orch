# Agent shop display

status: active
owner: engineering lead
date: 2026-10-17
spec: none: a display defect fix; no contract changes

## Goal
`orch agent shop` draws a readable picker on any terminal size, and dashboard select lists keep their numbers
and cut labels legible.

## Non-goals
Template-backed shops, new keys, a dashboard template browser, any change to the shop catalogue, the `--list`
table or the agent created on selection.

## Context
A reproduction (T0) of the picker on a pseudo-terminal found five defects: raw mode's disabled output
post-processing made a bare LF move down without returning to column 0; rows were not fitted to the terminal
width and wrapped; the row budget did not count the footer, so a paged list scrolled the heading away; two-digit
option numbers ran into their labels in the dashboard's select lists; and a label or hint longer than its row was
clipped silently. No contract changes, so no ADR.

## Design
`shop_picker::Picker` builds the whole frame as a list of lines, one per terminal row. Each line is fitted to the
width with a cell-aware ellipsis (description first, then tier, then name), control and bidi characters in
template text are replaced, and the page size counts the heading, blank lines and footer. `encode_frame` joins the
lines with `\r\n` and no trailing newline. A resize recomputes width and page. In `orch-tui`, the select field
right-aligns its numbers in a column as wide as the largest, cuts label then hint to the row with an ellipsis, and
the wizard footer shows `n of total` for a list longer than its window.

## Tasks
| Id | Task | Covers | Scope (paths) | Depends on | Verify | PR |
|---|---|---|---|---|---|---|
| T0 | Reproduce the five display defects | display | none (investigation) | — | PTY reproduction | 1 |
| S1 | Repair the picker frame | display | `crates/orch-cli/src/shop_picker.rs` | T0 | `cargo test -p orch-cli --lib shop_picker` | 1 |
| S2 | Repair select-list numbers, cuts and position | display | `crates/orch-tui/src/widgets/wizard.rs`, `crates/orch-tui/tests/wizards.rs` | T0 | `cargo test -p orch-tui --test wizards` | 1 |
| W1 | Docs | display | `README.md`, `CHANGELOG.md`, `docs/RUNBOOK.md` | S1, S2 | README example run in a disposable project | 1 |

## Status
| Item | State (implemented / verified on <platform> / pending) |
|---|---|
| T0 | done |
| S1 | implemented; unit tests; picker driven in tmux on macOS (80x24, 70x12, 40x8) |
| S2 | implemented; wizard tests |
| W1 | implemented in this PR; independent QA, final gate and publication pending |

## Test plan
Unit tests on the `Picker` state machine and frame (no line wider than the terminal at every size, CRLF framing,
control-character neutralisation, resize); `orch-tui` wizard tests for numbering, ellipsis and the footer
position. The picker is also driven in tmux. Not covered: Linux, Windows consoles, native terminal emulators other
than tmux.

## Rollback / compatibility
Display only: no on-disk, `--json` or peer-protocol change. Revert the two source files.

## Docs to update
README ("The agent shop"), CHANGELOG (Unreleased / Fixed), RUNBOOK ("The agent shop picker").

## Open questions
- None.
