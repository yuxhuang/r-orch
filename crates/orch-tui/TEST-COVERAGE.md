# WP-6 counted case inventory (V33)

PARITY.md §12 and **V33** gate the TUI on a *counted* `TestBackend` case
inventory, not on a human reading the checklist: one case per wizard field, per
validation message, per command-bar verb, per filter predicate, and per view key
binding including every overloaded key. This file is that count. It must not
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
| **Integration total** | **636** | |
| In-module unit tests | 314 | Pure logic: see the table below |
| **Grand total** | **950** | |

In-module unit tests, by file:

| Module | Cases | Ported from |
|---|---:|---|
| `messages/format.rs` | 84 | `formatAgentOutput` and friends — every legacy adapter shape |
| `text/cursor.rs` | 27 | `text-cursor.test.ts` (grapheme, width, navigation, kills) |
| `state.rs` | 28 | Sort orders, selection spaces, rows, batching, deletions, toasts |
| `wizard/engine.rs` | 27 | `form-wizard.test.tsx`, `form-wizard-validation.test.tsx` |
| `keys.rs` | 24 | New: every named action leaves the state its key leaves |
| `app.rs` | 23 | Event ingestion, `entityListChanged`, notifications, soft delete |
| `text/input.rs` | 16 | `text-input.test.tsx` (kill ring, undo, debounce) |
| `messages/mod.rs` | 14 | `formatEvent`, message types, activity presets |
| `onboarding.rs` | 12 | `onboarding.test.tsx`, `onboarding-agents.test.ts` |
| `command_bar.rs` | 10 | `help-overlay.test.tsx` (completion, suggestions, history) |
| `theme.rs` | 8 | `colors.test.ts` (palettes, remap, WCAG contrast) |
| `hit.rs` | 8 | New: z-order, clipping, scroll vs click, invalidation, identity |
| `layout.rs` | 8 | The region arithmetic and the clamp tests |
| `format.rs` | 7 | `capLine`/`capText`/`formatDuration`/`relativeTime` |
| `widgets/wizard.rs` | 6 | The text field's display-width sliding viewport |
| `tty.rs` | 5 | New: hang-up beats pending input, the poll timeout, no controlling tty, which fd is polled |
| `views/mod.rs` | 2 | Frame composition and the minimum-size guard |
| `clock.rs` | 2 | The injected clock |
| `runner.rs` | 3 | New: the DEC mode set, its reverse order, and the env switch |

## Spec item → case map

`docs/parity/tui.md` section → where it is covered.

| Spec | Cases |
|---|---|
| §1 Tabs, cycle order, `ACTIONS` label, tab badge | `k026`–`k034`, `v017`, `v072` |
| §1.1 Goals list: chips, progress bar, assignee, age, pseudo-row | `v003`, `v014`, `v018`, `v025`, `v026` |
| §1.1 Goal detail panel incl. scrolling | `k074`, `k076`, `v052`–`v059` |
| §1.2 Tasks list: chips, pips, title, attachments, goal badge, time | `v001`, `v004`–`v012`, `v020`–`v024` |
| §1.2 Goal grouping headers and percentages | `v027`–`v030`, `k045` |
| §1.2 Task detail panel and its row budget | `v042`–`v047` |
| §1.3 Agents list: chips, autonomous/lead glyphs, team badge, stats | `v002`, `v013`, `v031`–`v033` |
| §1.3 Agent detail panel, `ERROR_HINTS` panel | `v034`–`v041`, `v048`–`v051` |
| §1.4 Logs view: status bar, agent chips, rows, empty states | `f035`–`f047`, `v092` |
| §1.4 Log detail panel | `k085` |
| §2.1 Dispatch order (overlay, pickers, Ctrl+S, input modes) | `k018`–`k023`, `k080`, `k081`, `k089`–`k091` |
| §2.2 Normal-mode chain, every binding and overload | `k001`–`k088` |
| §2.3 Command-mode keys | `c0xx` command-bar key cases, `k092` |
| §2.4 Text editing (kill ring, undo, both undo flavours) | `text/input.rs` (16), and `is_meta` — Cmd is not Meta |
| §2.5 Agent filter picker | `f001`–`f012` |
| §2.6 Type filter picker | `f013`–`f027` |
| §3.1 Command bar hints and navigate bar | `c0xx` frame cases |
| §3.2 Help overlay (incl. both F1 cases) | `k019`, `k020`, `v085`–`v089` |
| §3.5 `LogsSearchBar` **not ported** | n/a (PARITY §13) |
| §3.7/§8.4 Toasts, bell, tab flash | `l027`–`l036`, `v080`–`v082` |
| §3.9 No confirmation prompts — soft delete instead | `k046`–`k049`, `l022`–`l026` |
| §3.10 Undo banner | `v083`, `v084` |
| §4.1–§4.5 Registry, completion, suggestions, history | `command_bar.rs` (10), `c0xx` |
| §4.6 Command semantics, every verb and refusal | `tests/command_surface.rs` (109) |
| §5.1 FormWizard engine, validation gating, keys | `wizard/engine.rs` (25), `w0xx` |
| §5.2 All 11 wizards, fields, skips, mappers | `tests/wizards.rs` (93) |
| §5.3 Validation messages, verbatim | `w0xx` validation group |
| §5.4 Model options and fallbacks | `w0xx` model-options group |
| §6 Layout arithmetic, resize, minimum-size guard | `layout.rs` (8), `k007`–`k017`, `v060`–`v062`, `v090`, `v091` |
| §6 Header contents | `v063`–`v072` |
| §7 Live updates: batching, debounce, caps, LRU, formatters | `l001`–`l021`, `l044`–`l048`, `messages/format.rs` (84) |
| PARITY §12.8 (k) Repeated feed lines collapse | `l056`–`l062` |
| §7 History loading, incl. per-entry timestamps | `l037`–`l043`, `l049`–`l055` |
| §8.1 Soft delete with undo | `l022`–`l026`, `k046`–`k049`, `k061`, `k062`, `k068` |
| §8.2 Paste image / attachments | `app.rs` paste cases, `w0xx` paste cases, `v020`, `v044` |
| §8.3 Onboarding | `onboarding.rs` (12), `v073`–`v079` |
| §8.5 Goal↔task linking (read-only) | `v022`–`v030`, `v055` |
| §8.6 Observer mode, palette switching, activity filter, watch toggle | `app.rs` observer cases, `f033`, `f034`, `c0xx` `/watch`,`/pause`,`/config palette` |
| §9 Palettes, semantic roles, glyphs | `theme.rs` (8), glyph cases in `v004`–`v014` |

## Deliberate divergences, each with a case

| Divergence | Why | Case |
|---|---|---|
| Both filter pickers swallow global keys | PARITY §14.8 — the type picker letting `q` quit was unintended | `k081` |
| Minimum terminal size guard | PARITY §12.2 — the original produces negative heights below ~`fixedRows` | `v090`, `v091` |
| Wizard `Esc` goes back a step; step 0 cancels | The Ink double-dispatch made `Esc` always close the wizard; the documented design (and the TS unit tests) is back-a-step | `wizard::engine` `esc_at_step_zero_cancels_and_later_steps_go_back` |
| Cancelling a wizard clears pending attachments | The TS `Esc` path leaked them into the next wizard (§8.2) | `app.rs` `cancelling_a_wizard_clears_pending_attachments` |
| The `W start orchestrator` onboarding hint is dropped | PARITY §13 — advertised but never handled | `v079`, `onboarding.rs` `the_dropped_w_hint_is_not_advertised` |
| `'new_task'` input mode, `LogsSearchBar`, `Footer`, `TabBar`, the stats ribbons and `sparklineData` are not ported | PARITY §13 dead code | no cases, by design |

## Not covered, and why

* **Colour and style assertions** are only spot-checked (`theme.rs` covers the
  palettes and the WCAG invariant). Buffer assertions here read the *symbols*;
  exact per-cell styling is L2.
* **The real terminal loop** (crossterm event reading, Kitty flags, resize
  events) lives in WP-5 and is not reachable from `TestBackend`. The facade
  sketch in `BACKEND.md` is the contract.
* **`tui-textarea-2` internals** (wrapping, undo coalescing) are the library's
  own tested behaviour; the cases here cover the keymap we drive it with.
