# Changelog

All notable user-visible changes. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning:
[SemVer](https://semver.org/spec/v2.0.0.html). Add entries under `[Unreleased]` in the PR that makes the change.

## [Unreleased]

### Changed
- r-orch is now licensed under the Apache License 2.0 instead of MIT: the repository ships `LICENSE`, `NOTICE` and `THIRD-PARTY-NOTICES.md`, and every crate declares `license = "Apache-2.0"`. r-orch is a derivative of `oxgeneral/ORCH` (MIT), and the material that comes from other projects keeps its MIT licence: the ORCH-derived Rust source, shop catalogue and prompt templates, the library skills (ORCH's adaptations of `garrytan/gstack` skills), and the xterm.js files served by `orch web`. `THIRD-PARTY-NOTICES.md` has their full licence texts and what a binary or package distribution must ship.
- The a-team templates now use a sibling `../workspace` directory for the specs and reports some roles write (it was named after a private project before); rename that sibling directory, or edit a copy of the template, when upgrading.

### Fixed
- The a-team orchestrator's rules now say to create work trees under `<repo root>/.worktrees` (they said `.worktree`). A copy of the template you made with `orch org template init --from a-team`, and agents already deployed from it, keep the old wording.

## [0.9.2] - 2026-10-20

Dashboard patch release: the Agents list is a sortable, grouped table with an UPDATED column. No peer-protocol
change, so 0.5.x through 0.9.2 peers can be mixed.

### Changed
- The dashboard's Agents list is a table like Goals and Tasks: one header row (the `o sort` / `b group:` chip row above it is gone, and its row goes to the list), every column sortable by `o`, `O`, `0` or a header click, and the active column marked `↑`/`↓`. The default order, and what `0` restores, is STATUS ascending (ACTIVE, IDLE, ERROR, OFFLINE, DISABLED); equal values keep the agents' configured order and blanks sort last. A new UPDATED column reads `R <age>` for the newest start or event of the agent's live run, else `T <age>` for the newest update of a task assigned to it (an assignment or an edit counts too, so it is not proof the agent ran), else `—`; while it is the sort, the footer says `R run · T task`. `b` and `B` cycle the grouping through Team, Node, Status, Adapter, Caps (each agent once, under its alphabetically first capability; the keyword filter matches every capability) and None (no sections). The footer always leads with `b <Mode>`, clickable, and from 108 columns the header ends with a clickable `GROUP: <Mode>` chip; at 80 columns the table shows the same columns as before, and a sort whose column is hidden is named in the footer as `o <Field>↑`.
- The Agents list's RUNS column now always shows lifetime completed/total runs, also for an active agent (which showed its live elapsed time there); the live run's elapsed time is in the agent detail's Overview, with its capabilities, node and UPDATED source.
- UPDATED, and the agent Overview's `updated` and `live` rows, count only a timestamp in the form ORCH writes (`YYYY-MM-DDTHH:MM:SS`, optional fraction, `Z`) for a real date from 1970 to at most a day past now (clock skew). An impossible, malformed or absurd time, such as year 9999 or 31 February, is skipped instead of reading `now` or a made-up age, so an agent with no other valid time shows `—` (sorted last both ways) and its Overview reads `unknown`. On a narrow terminal UPDATED is the first column to go, then CAPS; from 93 columns every column shows.
- The `?` help describes what UPDATED answers, what its `T` marker means and how the Caps grouping files an agent.

## [0.9.1] - 2026-10-19

Dashboard patch release: a goal detail's Tasks section is the Tasks list's table. No peer-protocol change, so 0.5.x
through 0.9.1 peers can be mixed.

### Changed
- The dashboard's goal detail Tasks section is now the Tasks list's table for that goal's tasks: the same column header and rows (status chip, priority pips, title with attachment count, assignee, Started, Elapsed*), without the goal column, whose width goes to the title. It is sorted by the Tasks list's sort, so `o`, `O`, `0` and clicking a header there (which also focuses the section) re-sort both; the selected task stays selected across a sort or refresh, and Enter and `Ctrl+Y` still expand and copy its description. The Tasks section is now in every goal's detail, and a goal without tasks shows the header and "No entries." instead of having no Tasks section.

## [0.9.0] - 2026-10-19

Dashboard (TUI) release: sortable, filterable lists, newest-first Activity in goal and agent details, and agent
pickers that filter as you type; `orch task add --attach` now saves its files. No peer-protocol change, so 0.5.x
through 0.9.0 peers can be mixed.

### Added
- The dashboard's Goals, Tasks and Agents lists have sortable column headers: `o` moves the sort to the next column, `O` reverses it, `0` restores the default order, and clicking a header sorts by it (ascending when it was inactive, flipped when it was already active). Goals and Tasks gain Started and Elapsed* columns, where Elapsed* runs from the start to now and stops at the last update once the row is terminal.
- The dashboard's Goals, Tasks and Agents lists can be filtered by keyword: press `Ctrl+K` and type to narrow the list as you go. It works with sorting, grouping and show all; `Esc`, `Ctrl+U` or the `filter:` chip clears it. `/` still opens the command bar.
- The dashboard's Agents view can group agents By node as well as By team: press `b` or click the `b group:` chip for a Local section, then one section per remote alias marked online or offline. The sort applies within each section and the selected agent stays selected.
- A user manual in [docs/manual/](docs/manual/README.md): installing, concepts, every command, the dashboard and web dashboard with screenshots, templates and the agent shop, remote agents, configuration and troubleshooting.
- Dashboard goal and agent details end with an Activity section in the Activity panel's columns, newest first: a goal lists the entries of its tasks, an agent its own entries. It follows new entries at the top; moving down pauses it and `Home` resumes, as the footer hint and the help overlay say. An empty one reads "No activity.".
- Dashboard wizards' agent steps (task and goal assignee, team lead and members) filter as you type: letters narrow the numbered list to agents whose name, adapter or role contains them, `Backspace` edits the filter and `Esc` clears it, `1`–`9` still pick a listed row, and `Ctrl+O` cycles the order through Original, Name, Adapter and Status. Rows show each agent's status, the Auto-assign/None choice stays first, and only a listed agent or that choice can be submitted; `j`/`k` on these steps now type into the filter (use the arrow keys to move).

### Changed
- Dashboard goal, task and agent details show their Overview as an aligned block: the title on a `━` rule, then Amber labels with values in two columns (one column on narrow terminals) and statuses, priorities and per-status task counts in their list colours. A value or title too long for its row wraps onto the rows below it, indented to its column, so nothing is cut off; only the two-row summary pinned above other sections, which uses the same style, still shortens them with `…`. The fields are unchanged; the goal's `Updated` label is now lower case like the others.
- The dashboard's task-detail Activity section shows each entry in the same columns and colours as the Activity panel (time, agent, type icon, text, error and agent-group highlighting) instead of bare text.
- On the dashboard's Tasks list, showing all tasks or collapsing back to ten (`S`, the `Show all` row or the footer's `S show all`) keeps the selected task selected instead of jumping to the first row. If collapsing hides the selected task, the cursor stays on the last row still listed.
- On the dashboard's Tasks list, grouping tasks by goal or ungrouping them (`G`) keeps the selected task selected instead of jumping to the first row.

### Removed
- Parity validation against the TypeScript original: `docs/PARITY.md`, `cargo xtask parity` and `lint-scenarios`, the scenario corpus and the oracle setup are gone, and the docs no longer promise compatibility with the npm package. Compatibility with `.orchestry` data written by r-orch releases is unchanged ([ADR 0009](docs/decisions/0009-remove-parity-tooling.md)).

### Fixed
- An Activity entry with no agent (a system entry) at the top of the Activity panel or a detail's Activity section now shows its time, like the first entry of any other group. It also counts as the first group, so when the top entry has no agent, the alternating group shading below it can swap.
- Dashboard goal, task and agent details keep their section titles in the same order when you move between sections; only the focused title changes style, and a narrow rail drops the titles farthest from the focused one instead of moving it to the front.
- A key pressed while the terminal is being resized is no longer ignored until the next key, in the `orch agent shop` picker and the dashboard. In tests on Linux, about one in five keys sent right after a resize was lost.
- Keys that reach the dashboard together (typed fast over SSH or `orch web`, sent by tmux, or pasted without bracketed paste) are all handled at once. Only the first used to show, so `jj` in a wizard list moved one row and `hello` read `h` until the next key was pressed.
- Dashboard task, goal and team wizards check the chosen assignee, lead and members against the current agent list when you submit. An agent disabled or removed after you picked it is refused with "Nothing was saved: …": nothing is written, your other answers stay, and the step reopens on the agents that can still be chosen (a refused team member stays listed until you untick it). Before, the stale agent was usually saved anyway, and a new task whose assignee had been removed was created unassigned. The edit wizards' first assignee row now reads "Keep current", which is what it always did.
- `orch task add --attach` now saves the files: each one is copied into `.orchestry/attachments/<task id>/` and listed on the task. A missing file stops the command with `Attachment file not found` before anything is saved.
- Task attachments reject duplicate names (including case variants), names already attached on append, and non-file sources before anything is written; copy errors name the source, destination and cause.
- The dashboard's `?` help overlay fits an 80×24 terminal: its last rows, including the dismiss hint and the Elapsed* explanation, were cut off below the shortcut columns. On a short terminal the box drops its blank rows and may cover the header.

## [0.8.0] - 2026-10-18

Command-line release: YAML output, file-based `update`/`patch`, and organization templates. No peer-protocol
change, so 0.5.x through 0.8.0 peers can be mixed.

### Added
- `--yaml`, a global flag beside `--json`: every command that prints JSON can print the same data as one YAML document with every key and string double-quoted. The two flags together are a usage error, and `--json` output is byte-for-byte unchanged.

- `orch agent update|patch <id> -f FILE`, `orch task update|patch <id> -f FILE` and `orch config update|patch -f FILE`: replace a definition from a JSON or YAML file (or stdin), or apply an RFC 7396 merge patch to it, without an editor. `--input-format` overrides detection; identity and runtime state stay read-only, and nothing is written unless the whole result validates.
- Deploy a team from a template directory: `orch org deploy --template-dir DIR` (or `--template NAME`) reads `org.yml`, one YAML file per member and optional shared rules, and creates the agents, a team and an optional goal. Your own templates live under `~/.orchestry/templates/`. See [docs/org-templates.md](docs/org-templates.md).
- A built-in **a-team** template with 27 specialist roles: `orch org deploy a-team`. Shared rules are embedded into each role, so deployed agents need no rule files. The ten existing presets are unchanged.
- `orch org template list|show|init|edit|update|patch` and `orch org template agent list|add|edit|update|patch|delete` manage templates without an editor: `update` replaces a file from a complete JSON or YAML document, `patch` applies an RFC 7396 merge patch, and a failed change leaves the template untouched. Builtins are read-only; copy one with `init --from`.
- `orch agent shop --template NAME` and `--template-dir DIR` install a single member of a template, with the same role and settings `deploy` would give it.

### Fixed
- `orch org template agent add` accepts valid JSON surrogate-pair escapes and checks the original stdin byte limit before normalization.
- `orch agent shop` now draws a readable picker: every row starts at the left edge, rows are cut to the terminal width with an ellipsis instead of wrapping, the heading and the `n-m of total` footer stay on screen on short terminals, and compact headings keep movement, selection and cancellation keys visible at narrow widths.
- `orch remote remove` can no longer race a live peer link: a link still holding the removed registration, or one since replaced with a different address, cannot recreate `.orchestry/remotes/<alias>.json` or keep reporting the alias online. A link that is stopped while it writes its status reports the remote offline at once, not when that write finishes.
- Dashboard select lists, such as the Agent Shop opened with `Ctrl+S` in the New Agent wizard, with ten or more options keep the number apart from the label (`10 Content Creator`), mark a label or hint that was cut to the row with an ellipsis, and show `n of total` in the footer when the list is longer than its window.
- The `orch agent shop` picker draws every frame at the terminal's current size, so a resize that arrives together with a keypress, without a separate resize event, still gets a frame that fits.

## [0.7.0] - 2026-10-17

Dashboard (TUI) release: agent sorting and live-following activity feeds. No peer-protocol or on-disk change, so
0.5.x, 0.6.0 and 0.7.0 peers can be mixed.

### Added
- Sort the Agents list by Original, Name, Status, Adapter/Model or Location in either direction for the current session, with keyboard and clickable reset controls.
- `docs/specs/` and a spec-driven workflow for contributors and agents ([ADR 0004](docs/decisions/0004-spec-driven-development.md)): requirements with acceptance checks, short execution plans with task tables, and bounded design-review loops.

### Fixed
- Keep the Logs inspector pinned while messages arrive, preserve `G` navigation with `End` for follow, sort agent names and locations without case sensitivity, and reserve Activity header space for the follow state.
- Follow new entries in the Activity panel, ACTIONS list and task-detail Activity at the bottom; scrolling up pauses, and returning to the bottom or focusing a feed there resumes.

## [0.6.0] - 2026-10-17

Dashboard (TUI) quality-of-life release. No peer-protocol or on-disk change, so 0.5.x and 0.6.0 peers can be
mixed.

### Added
- Inspect complete retained messages and fields in a snapshot viewer, select text, and copy through explicit host or terminal routes, with browser gesture and selectable fallback support.
- Focus and page through lists, activity and detail sections with keyboard or mouse; Agents rows now align in a table.
- Confirm changed-editor exits and recover bounded drafts for the current dashboard session.

### Changed
- Tab moves pane focus; Left/Right switch views, and entity action letters require main-list focus.
- Mouse capture includes button-motion reporting for text selection; F2 returns selection to the terminal.

### Fixed
- Retain complete admitted message bodies, charge owned text capacity, share pinned Raw ownership, and count oversized-body rejection once.
- Repaint cleared continuation cells after inspected ZWJ text in the browser.

## [0.5.2] - 2026-10-16

Maintenance release: no behaviour or protocol change, so peers on 0.5.1 and 0.5.2 can be mixed.

### Added
- `AGENTS.md` (and a `CLAUDE.md` that imports it): engineering instructions for the agents and humans working
  on the repo.
- `docs/plans/` and `docs/decisions/` with templates; ADRs 0001 (plans and decision records) and 0002.
- `CHANGELOG.md`.
- `docs/plans/2026-10-16-open-source-readiness.md`: the checklist of gaps before the repo can be opened.

### Changed
- Building from source needs Rust 1.99.1 (was 1.94.1); `rust-toolchain.toml` pins it and `kstring` is no longer
  held back for MSRV.
- Parity with the TypeScript original is no longer a goal. `docs/PARITY.md` and `docs/parity/` are marked
  historical ([ADR 0002](docs/decisions/0002-parity-is-no-longer-a-goal.md)).
- Dependencies refreshed (`cargo update`), including `fancy-regex` 0.19 and `tokio-tungstenite` 0.30.

## [0.5.1] - 2026-10-16

Upgrade every peer together: most fixes are on the worker.

### Added
- A remote run's phase on the running entry, shown in task views: `dispatched -> <alias>`, `queued @<alias>`
  (with `awaiting approval: orch task approve <id> on <alias>`), `RUN @<alias>`, and `RUN? (<alias> offline)`;
  the dashboard colours the remote phase and shows its hint.
- `orch remote status` shows why a worker's last session ended and, on the controller, the retry countdown and
  attempt.
- Deadlines for the dispatched and queued phases: a submit the worker never acknowledges fails after 5 minutes
  online; a task queued for an hour is reported hourly but never failed.
- A worker cancels leftover todo copies of a controller it has not seen for 24 hours.

### Changed
- A controller restart, graceful or not, keeps remote runs and reattaches to them instead of cancelling them.
- Peer-supplied strings and a worker's free-form error text are bounded at ingest, and a controller name that is
  not plain display text is refused.
- Rolling back to 0.5.0 cancels remote runs in flight; stop with none in flight first.

### Fixed
- A task file containing Markdown emphasis no longer fails with `contains N YAML aliases, over the limit of 64`;
  aliases are now counted from parser events.
- One unreadable task file on a worker no longer drops every connection.
- A remote task's queued hint no longer says "running", and a phase clock starts at zero, not the epoch.
- Reconnect backoff resets only after a healthy session; a cancel the worker could not apply is retried; every
  worker-side mirror cancel says why.
- Owner startup no longer fails over a rehydrate read; the orphan sweep never waits on a session.

See [docs/RUNBOOK.md](docs/RUNBOOK.md) for each new message.

## [0.5.0] - 2026-10-16

Previewed as `v0.5.0-alpha.1` and `v0.5.0-alpha.2`.

### Added
- Remote agents: orch peers connect over TCP so one installation can delegate tasks to another
  (`orch remote ...`, `orch serve --listen`, `--peer-name`; see README "Remote agents").
- Remote tasks wait for worker approval and run without credential-shaped environment variables.

### Changed
- The peer listener binds `127.0.0.1:7690` by default; pass `0.0.0.0:7690` or a LAN IP to accept other hosts.
  There is still no authentication or encryption.
- Entity indexes (tasks, agents, messages, context) take a cross-process writer lock (`.index.lock`); a writer
  that cannot get it within 5 seconds fails with a retryable error.
- A remote counts as online only while the controller's engine holds a lock on
  `.orchestry/remotes/<alias>.health`.
- `.orchestry/peer-id` and `.orchestry/remotes/*.json` are written readable by the owner only.

### Fixed
- `claude` agents no longer fail with `Failed to spawn process: claude` on Linux for prompts over the 128 KiB
  argument limit: the system prompt goes through a private file (`--system-prompt-file`) and the task prompt
  through stdin. A blank system prompt now counts as none.

## Earlier releases

- [0.4.0] - 2026-10-15: TUI messages to agents with name autocomplete.
- [0.3.1] - 2026-10-15: TUI collapses repeated feed lines.
- [0.3.0] - 2026-10-15: TUI mouse support.
- [0.2.0] - 2026-10-15: `orch web`, the TUI in a browser.
- [0.1.2] - 2026-10-14: reports r-orch's own version, not the original's.
- [0.1.1] - 2026-10-14: `orch update` and the README point at Homebrew.
- [0.1.0] - 2026-10-14: first tagged release of the Rust port.

[Unreleased]: https://github.com/yuxhuang/r-orch/compare/v0.9.2...HEAD
[0.9.2]: https://github.com/yuxhuang/r-orch/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/yuxhuang/r-orch/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/yuxhuang/r-orch/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/yuxhuang/r-orch/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/yuxhuang/r-orch/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/yuxhuang/r-orch/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/yuxhuang/r-orch/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/yuxhuang/r-orch/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/yuxhuang/r-orch/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/yuxhuang/r-orch/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/yuxhuang/r-orch/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/yuxhuang/r-orch/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/yuxhuang/r-orch/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/yuxhuang/r-orch/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/yuxhuang/r-orch/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/yuxhuang/r-orch/releases/tag/v0.1.0
