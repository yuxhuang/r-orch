# TUI counted case inventory

The TUI is gated on a *counted* `TestBackend` case inventory, not on a human
reading a checklist: one case per wizard field, per validation message, per
command-bar verb, per filter predicate, and per view key binding including
every overloaded key. This file is that count. It must not
regress — if a number goes down, a case was deleted.

Run everything with `cargo test -p orch-tui`.

## Totals

| Suite | Cases | What it enumerates |
|---|---:|---|
| `tests/keymap.rs` (`k001..k096`) | 96 | The five-stage dispatch chain and every per-view binding |
| `tests/wizards.rs` (`w001..w093`) | 93 | All 11 wizards: fields, defaults, skip predicates, mappers, validation |
| `tests/command_surface.rs` (`c001..c112`) | 112 | Every verb, subcommand, alias, usage and confirmation string |
| `tests/views_render.rs` (`v001..v094`) | 94 | Sort orders, glyphs, caps, badges, detail panels, header, onboarding, overlays |
| `tests/filters.rs` (`f001..f047`) | 47 | Both filter pickers, every filter predicate, and the logs row→index mapping |
| `tests/mouse.rs` (`m001..m111`) | 111 | Every clickable target, every unclickable surface, and the identity/reflow/wheel hazards |
| `tests/hitmap.rs` (`h001..h021`) | 21 | The per-frame hit map: row↔frame agreement, every clickable target, clipping, invalidation |
| `tests/live_updates.rs` (`l001..l062`) | 62 | Batching, caps, the run-id LRU, soft-delete timing, notifications, history and its timestamps, endurance, repeated-line collapse |
| `tests/msg.rs` (`msg001..msg068`) | 73 | Bracketed paste and its per-stage routing (including the 64 KiB cap), `/msg`'s grammar and every refusal, broadcast confirm/disarm on every exit, autocomplete and its fills, the `m` binding, the recipient chip, the cursor and its window budget, the retained refusal **as drawn**, and the frame cost of a 64k value |
| **Integration total** | **709** | |
| In-module unit tests | 358 | Pure logic: see the table below |
| **Grand total** | **1067** | |

In-module unit tests, by file:

| Module | Cases | Covers |
|---|---:|---|
| `messages/format.rs` | 84 | Agent output formatting — every legacy adapter shape |
| `text/cursor.rs` | 27 | Grapheme, width, navigation, kills |
| `state.rs` | 28 | Sort orders, selection spaces, rows, batching, deletions, toasts |
| `wizard/engine.rs` | 27 | Wizard engine and validation gating |
| `keys.rs` | 24 | Every named action leaves the state its key leaves |
| `app.rs` | 23 | Event ingestion, `entityListChanged`, notifications, soft delete |
| `text/input.rs` | 17 | Kill ring, undo, debounce, plus the edit generation |
| `messages/mod.rs` | 14 | Event formatting, message types, activity presets |
| `onboarding.rs` | 12 | Onboarding |
| `command_bar.rs` | 21 | Completion, suggestions, history, plus the `/msg` rows |
| `theme.rs` | 8 | Palettes, remap, WCAG contrast |
| `hit.rs` | 8 | Z-order, clipping, scroll vs click, invalidation, identity |
| `layout.rs` | 8 | The region arithmetic and the clamp tests |
| `format.rs` | 8 | Line and text caps, durations, relative time, `cap_display` |
| `widgets/wizard.rs` | 6 | The text field's display-width sliding viewport |
| `widgets/command_bar.rs` | 5 | The cursor-anchored value window — its budget, both clips, wide graphemes, and that its cost is the columns drawn |
| `tty.rs` | 5 | Hang-up beats pending input, the poll timeout, no controlling tty, which fd is polled |
| `views/mod.rs` | 2 | Frame composition and the minimum-size guard |
| `clock.rs` | 2 | The injected clock |
| `backend/fake.rs` | 5 | `send_message` recording, fan-out, a disabled recipient, injected failure, refusals |
| `msg.rs` | 21 | The recipient grammar, its quoter's round trip over 35 awkward names, commit resolution, prefix-never-commits, the unquoted-prefix shadow (whole and partial), the broadcast keyword, suggestions and their exact-first order, the chip |
| `runner.rs` | 3 | The DEC mode set, its reverse order, and the env switch |

## Feature → case map

| Feature | Cases |
|---|---|
| Tabs, cycle order, `ACTIONS` label, tab badge | `k026`–`k034`, `v017`, `v072` |
| Goals list: chips, progress bar, assignee, age, pseudo-row | `v003`, `v014`, `v018`, `v025`, `v026` |
| Goal detail panel incl. scrolling | `k074`, `k076`, `v052`–`v059` |
| Tasks list: chips, pips, title, attachments, goal badge, time | `v001`, `v004`–`v012`, `v020`–`v024` |
| Goal grouping headers and percentages | `v027`–`v030`, `k045` |
| Task detail panel and its row budget | `v042`–`v047` |
| Agents list: chips, autonomous/lead glyphs, team badge, stats | `v002`, `v013`, `v031`–`v033` |
| Agent detail panel, `ERROR_HINTS` panel | `v034`–`v041`, `v048`–`v051` |
| Logs view: status bar, agent chips, rows, empty states | `f035`–`f047`, `v092` |
| Log detail panel | `k085` |
| Dispatch order (overlay, pickers, Ctrl+S, input modes) | `k018`–`k023`, `k080`, `k081`, `k089`–`k091` |
| Normal-mode chain, every binding and overload | `k001`–`k088` |
| Command-mode keys | `c0xx` command-bar key cases, `k092` |
| Text editing (kill ring, undo, both undo flavours) | `text/input.rs` (16), and `is_meta` — Cmd is not Meta |
| Agent filter picker | `f001`–`f012` |
| Type filter picker | `f013`–`f027` |
| Command bar hints and navigate bar | `c0xx` frame cases |
| Help overlay (incl. both F1 cases) | `k019`, `k020`, `v085`–`v089` |
| `LogsSearchBar` is not implemented | n/a |
| Toasts, bell, tab flash | `l027`–`l036`, `v080`–`v082` |
| No confirmation prompts — soft delete instead | `k046`–`k049`, `l022`–`l026` |
| Undo banner | `v083`, `v084` |
| Registry, completion, suggestions, history | `command_bar.rs` (21), `c0xx` |
| `/msg`, autocomplete, the chip, `m`, bracketed paste | `msg.rs` (21), `command_bar.rs` `/msg` rows, `widgets/command_bar.rs` (5, the window), `msg001`–`msg068` |
| Command semantics, every verb and refusal | `tests/command_surface.rs` (109) |
| FormWizard engine, validation gating, keys | `wizard/engine.rs` (25), `w0xx` |
| All 11 wizards, fields, skips, mappers | `tests/wizards.rs` (93) |
| Validation messages, verbatim | `w0xx` validation group |
| Model options and fallbacks | `w0xx` model-options group |
| Layout arithmetic, resize, minimum-size guard | `layout.rs` (8), `k007`–`k017`, `v060`–`v062`, `v090`, `v091` |
| Header contents | `v063`–`v072` |
| Live updates: batching, debounce, caps, LRU, formatters | `l001`–`l021`, `l044`–`l048`, `messages/format.rs` (84) |
| Repeated feed lines collapse | `l056`–`l062` |
| History loading, incl. per-entry timestamps | `l037`–`l043`, `l049`–`l055` |
| Soft delete with undo | `l022`–`l026`, `k046`–`k049`, `k061`, `k062`, `k068` |
| Paste image / attachments | `app.rs` paste cases, `w0xx` paste cases, `v020`, `v044` |
| Onboarding | `onboarding.rs` (12), `v073`–`v079` |
| Goal↔task linking (read-only) | `v022`–`v030`, `v055` |
| Observer mode, palette switching, activity filter, watch toggle | `app.rs` observer cases, `f033`, `f034`, `c0xx` `/watch`,`/pause`,`/config palette` |
| Palettes, semantic roles, glyphs | `theme.rs` (8), glyph cases in `v004`–`v014` |

## Deliberate design choices, each with a case

| Choice | Why | Case |
|---|---|---|
| Both filter pickers swallow global keys | A type picker that lets `q` quit was unintended | `k081` |
| Minimum terminal size guard | Below ~`fixedRows` the layout would produce negative heights | `v090`, `v091` |
| Wizard `Esc` goes back a step; step 0 cancels | The documented design is back-a-step | `wizard::engine` `esc_at_step_zero_cancels_and_later_steps_go_back` |
| Cancelling a wizard clears pending attachments | Otherwise they leak into the next wizard | `app.rs` `cancelling_a_wizard_clears_pending_attachments` |
| The `W start orchestrator` onboarding hint is dropped | It was advertised but never handled | `v079`, `onboarding.rs` `the_dropped_w_hint_is_not_advertised` |
| No `'new_task'` input mode, `LogsSearchBar`, `Footer`, `TabBar`, stats ribbons or `sparklineData` | Dead code, not implemented | no cases, by design |
| `Tab` on a separator row is inert | It reset the input and wiped it; hit by the category headers | `msg043` |
| The command bar renders a real cursor and anchors its window on it | The `TextInput` cursor API was present but unused | `msg038`–`msg040` |
| A retained `/msg` refusal is drawn in the suggestion rows, which it owns while shown | The feed is not rendered in command mode, so the refusal was the one thing off screen; a reopened agent list covered it | `msg052`–`msg059` |
| Bracketed paste is enabled; a paste in normal mode is dropped | A pasted body submitted line 1 and ran lines 2+ as keys | `msg001`–`msg007` |

## Not covered, and why

* **Colour and style assertions** are only spot-checked (`theme.rs` covers the
  palettes and the WCAG invariant). Buffer assertions here read the *symbols*;
  exact per-cell styling is not asserted.
* **The real terminal loop** (crossterm event reading, Kitty flags, resize
  events) lives in `orch-cli` and is not reachable from `TestBackend`. The facade
  sketch in `BACKEND.md` is the contract.
* **`tui-textarea-2` internals** (wrapping, undo coalescing) are the library's
  own tested behaviour; the cases here cover the keymap we drive it with.
