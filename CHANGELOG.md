# Changelog

All notable user-visible changes. Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning:
[SemVer](https://semver.org/spec/v2.0.0.html). Add entries under `[Unreleased]` in the PR that makes the change.

## [Unreleased]

### Added
- `orch agent update|patch <id> -f FILE`, `orch task update|patch <id> -f FILE` and `orch config update|patch -f FILE`: replace a definition from a JSON or YAML file (or stdin), or apply an RFC 7396 merge patch to it, without an editor. `--input-format` overrides detection; identity and runtime state stay read-only, and nothing is written unless the whole result validates.

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

[Unreleased]: https://github.com/yuxhuang/r-orch/compare/v0.7.0...HEAD
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
