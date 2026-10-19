# `TuiBackend` — the engine facade the dashboard needs

`orch-tui` does **not** depend on `orch-engine`. Everything the dashboard needs
from the rest of the system goes through one trait, `orch_tui::backend::TuiBackend`
(`src/backend/mod.rs`). The CLI implements this trait over the DI container
(`crates/orch-cli/src/tui/backend.rs`); the tests use the in-crate `FakeBackend`.

Everything below is the contract. If you need a shape changed, say so rather than
widening it locally — the TUI's tests are written against this surface.

## Shape of the facade

1. **Synchronous.** Every method returns a value, not a future. A call is a
   *request*: an implementation must not block on long-running work. `run_task`
   asks the orchestrator to dispatch and returns; the outcome arrives through
   `poll_events`. Use `tokio::runtime::Handle::block_on` for the short
   store reads/writes (`refresh_*`, `create_task`, …) and `spawn` for anything
   that waits on an agent.
2. **Polled streams.** Events, history, the model catalog and update checks are
   `poll_*` methods that the TUI drains **once per frame**, not callback registrations:

   | Stream | Facade method | Contract |
   |---|---|---|
   | Events | `poll_events() -> Vec<OrchestratorEvent>` | Events since the last poll, oldest first. Drain a `broadcast::Receiver` without blocking; drop nothing silently — the feed's caps are the TUI's job. |
   | History | `poll_history() -> Vec<Vec<HistoryEntry>>` | Zero or more batches since the last poll. The host delivers two (the 3 newest runs, then the next 7, 30 tail events each, sliced to the last 200). |
   | Model catalog | `poll_model_catalog() -> Option<ModelCatalog>` | The newest catalog, if one arrived. The TUI rebuilds an open agent wizard in place when it does. |
   | Update check | `poll_update() -> Option<String>` | A newer version, once known. The TUI starts `background_install` exactly once and then reads `installed_version()`. |

   This is what makes the TUI tests synchronous: push a key, `tick()`, draw,
   assert (ARCHITECTURE §8.4).

## Errors

`BackendResult<T> = Result<T, String>`. The TUI only ever renders
`Failed: {err}`, so the message is the whole contract — use the same text the
CLI would print for that failure.

## Capability flags

Several bindings depend on whether the backend supports an operation. Those
predicates are the `can_*` methods, all defaulting to `true`:

| Method | Gates |
|---|---|
| `can_create_task` | `n`/`N` and the `+ add task...` row on the tasks view, `/task add` |
| `can_add_agent` | `n`/`N` and `+ add agent...`, `/agent add` |
| `can_create_goal` | `n`/`N` and `+ add goal...`, `/goal add` |
| `can_force_stop_agent` | whether a *running* agent can be soft-deleted at all (otherwise the TUI refuses with `Cannot delete — agent "{name}" is running. Press S to stop first.`) |
| `can_paste_image` | the `⌘+V paste image` hint in the task wizards |
| `can_send_message` | whether the TUI offers to message an agent at all (`/msg` and the `m` binding) |

## Launch context

`context() -> LaunchContext` carries the static launch facts:
project name, version, default adapter, the persisted palette / activity filter
/ notification preferences / max-concurrent value, and the watch-mode outcome
(`initial_watch_active`, `observer_mode`, `watch_error`). The TUI emits the
observer-mode banner or the watch-failure banner from these on launch.

## Operations

Grouped as in the trait.

* **Refresh** — `refresh_tasks`, `refresh_agents`, `refresh_goals`,
  `refresh_state`, `list_teams`. Called on launch and after every mutation, plus
  on the 150 ms debounced event refresh. The TUI commits a list only when
  `entityListChanged` says so, so returning fresh-but-equal `Vec`s is cheap and
  safe.
* **Tasks** — `create_task(title, CreateTaskOpts)`, `update_task(id, TaskFields)`,
  `delete_task`, `run_task`, `run_all`, `cancel_task`, `retry_task`,
  `assign_task(task, agent)`, `approve_task`, `reject_task(id, feedback)`.
* **Agents** — `add_agent(name, adapter, AgentOpts)`,
  `update_agent(id, AgentFields)`, `delete_agent`, `force_stop_agent`,
  `toggle_autonomous(id, enabled)`, `disable_agent`, `enable_agent`.
* **Teams** — `create_team(CreateTeamInput)`, `join_team`, `leave_team`,
  `disband_team`, `set_team_lead`.
* **Goals** — `create_goal(CreateGoalInput)`, `update_goal(id, GoalFields)`,
  `update_goal_status(id, status, force)`, `delete_goal`, `goal_progress(id)`.
* **Messaging** — `send_message(MessageTarget, body) -> Vec<String>`, the id of
  every record created, in fan-out order (one for a direct send, one per
  recipient for a broadcast, **zero** for a broadcast that reached nobody — a
  success, so the caller reports a count). Ids rather than the `Message`
  records because that is all the dashboard consumes; required rather than
  defaulted, so a backend without messaging is unrepresentable, and
  `can_send_message()` defaults to `true` like every other capability flag —
  overriding it to `false` is the kill switch if messaging has to be disabled
  without a revert. `MessageTarget::Agent` carries an **already-resolved agent
  id**, never a name: the TUI owns name resolution (`orch-tui/src/msg.rs`) and
  the backend does none, so the two cannot disagree about the recipient
  without the engine saying so; a *direct* send to a disabled agent still
  delivers, and only the broadcast fan-out skips them. The implementation
  sends as `"cli"` — a constraint, not a preference: it is the one sender id
  `MessageService` accepts without a registered agent behind it — with an
  empty subject (the service renders `(no subject)`) and the default TTL, and
  does **not** pre-truncate the body, so a TUI-sent message is identical on
  disk to a CLI-sent one and `truncate_utf16` stays the only place the
  4000-unit cut happens. Both are pinned in
  `crates/orch-cli/tests/tui_backend.rs`. It works in observer mode, where
  `message:sent` goes to *this* process's bus rather than the queue the
  dashboard drains — so confirmation is a locally emitted feed line, not an
  event.
* **Watch mode** — `start_watch`, `stop_watch` (reachable only through `/watch`
  and `/pause`).
* **Settings persistence** — `save_palette`, `save_activity_filter`,
  `save_notifications`, `save_max_concurrent`, `complete_onboarding`. All write
  `~/.orchestry/global.yml` or `.orchestry/config.yml`; the TUI treats a failure
  as non-fatal.
* **Side channels** — `paste_image() -> ClipboardPaste` (the TUI writes nothing
  itself; the implementation puts the image in a temp file and returns its path
  plus its size) and `bell()`, a direct terminal write outside the draw loop.

### Field semantics worth preserving

* `AgentFields::command` / `effort`: `Some(String::new())` means **clear the
  field**, `None` means **leave it unchanged**. The edit-agent wizard relies on
  this — switching an agent away from `shell` sends
  `command: Some("")`.
* `TaskFields::description`: always sent by the edit-task wizard, so
  an emptied description clears it.
* `update_goal_status(.., force = true)` is only used by `C` on the goals view
  (achieve, cancelling cancellable tasks).
* `CreateTaskOpts::attachments` / `TaskFields::attachments` carry the pasted
  image paths; the TUI clears its pending list once the call returns.

## Wiring sketch

```rust
struct EngineBackend {
    container: Container,
    runtime: tokio::runtime::Handle,
    events: broadcast::Receiver<OrchestratorEvent>,
    history: mpsc::Receiver<Vec<HistoryEntry>>,
    catalog: mpsc::Receiver<ModelCatalog>,
}

impl TuiBackend for EngineBackend {
    fn refresh_tasks(&mut self) -> BackendResult<Vec<Task>> {
        self.runtime
            .block_on(self.container.task_service.list())
            .map_err(|e| e.to_string())
    }

    fn poll_events(&mut self) -> Vec<OrchestratorEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }
    // …
}
```

The event loop lives in the CLI: read a crossterm event (with a poll timeout no
longer than the 120 ms animation tick), call `app.handle_key(key)` or
`app.state.width/height = …` on a resize, then `app.tick()` and
`terminal.draw(|f| orch_tui::views::draw(f, &app))`. Exit when
`app.state.should_exit` is set, then stop watch mode / the disk observer and
clear the event bus.

Enable the Kitty keyboard protocol at startup
(`PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`) so `Ctrl+Enter` is
distinguishable from `Enter` in the wizard textarea; the `Tab` confirm fallback
stays for terminals that refuse it.

## Entry point

`orch_tui::run(backend)` owns the whole terminal lifecycle — raw mode, the
alternate screen, the Kitty enhancement flags (popped on exit), the
poll/tick/draw loop at one animation tick, and restoring the terminal on every
exit path including a panic. `orch tui` should be:

```rust
let backend = EngineBackend::new(container, handle);
orch_tui::run(backend)?;
// then: stop watch mode or the disk observer, and clear the event bus.
```

## Implementer answers

1. **`goal_progress` with no report.** Map "absent" to `Ok(String::new())`, not
   `Err` — `Err` renders `Failed: …` and "nothing recorded yet" is not a
   failure. The TUI treats an empty-or-whitespace report as absent and renders
   no `progress` section (case `v094`).
2. **Stringly-typed enums.** Parse `AgentOpts::{effort, approval_policy}` and
   `AgentFields::effort` with `FromStr`; an unparseable value is
   `Err("Unknown effort: \"{v}\"")` — the TUI shows it verbatim. On clearing:
   `Some(String::new())` means *clear* and `None` means *unchanged* for
   `command`, `effort` and `model`. The TUI never sends `Some("")` for `role`:
   choosing the `Skip` preset maps to `None`, so a role cannot be cleared from
   the UI — that is deliberate, not an oversight.
   The engine accepts a clear for all four, so **`role` is the one place where
   the backend is more capable than the UI on purpose.** Do not "fix" the
   mapper: the wizard → entity field mapping is on-disk data, so making `Skip`
   clear a stored role would change what the same keystrokes write for existing
   users. If we ever want that behaviour it is a contract change (a plan and an
   ADR); `edit_agent_wizard_to_fields` carries the same note.

   `approval_policy` is deliberately absent from `AgentFields`: the edit-agent
   wizard never passes it and its step is always skipped, so no edit path can produce one.
3. **Fire-and-forget is about *dispatch*, not validation.** `run_task`,
   `run_all` and `start_watch` must return `Err` for anything knowable before an
   agent starts — a lock conflict, an unknown id, no eligible agent — because
   the TUI prints `Failed: {err}` for them, and because **the TUI does not render
   `orchestrator:error`**: the event formatter has no case for it, so an error delivered only through the event stream is
   invisible. Everything that happens *after* the run starts belongs in
   `poll_events`.
4. **`stop_watch`** may spawn and return `Ok(())` — the TUI only needs the
   request accepted. It sets `watch_active = false` and prints
   `✓ Watch mode paused` on `Ok`; the header chip follows that flag.
5. **The update facade is intentionally inert.** Leave `poll_update`,
   `background_install` and `installed_version` at their trait defaults
   (there is no self-update and no network access). No test requires a real version —
   case `v070` scripts one through `FakeBackend`. The header update chip then
   simply never appears.
6. **`refresh_state`** re-reads `state.json` on every call; no caching. The TUI
   calls it on launch, after each mutation, on the 150 ms event-driven refresh
   and on the periodic poll (3 s observer / 5 s watch), and always commits the
   result — unlike the entity lists, which it commits only when
   `entityListChanged`.
7. **`poll_history` — the exact two batches:**
   * list **all** runs once (not per task), sort by `started_at` **descending**,
     keep only `succeeded` and `failed`;
   * batch 1 = runs `[0..3)`, batch 2 = runs `[3..10)`;
   * per run, read the **last 30** events (`read_events_tail(run.id, 30)`) and map
     each to `HistoryEntry { timestamp: event.timestamp, run_id: run.id,
     agent_id: run.agent_id, task_id: run.task_id, event_type: event.type,
     data: event.data }`;
   * flatten each batch, sort **ascending** by timestamp, keep the **last 200**;
   * deliver batch 1 as soon as it is ready and batch 2 afterwards (the host
     loads it in the background). Failures go to stderr and must never bubble
     into the UI.
   The TUI prepends each batch and re-truncates to 500 messages, de-duplicates
   repeated nested provider errors per `run_id`, and skips entries whose
   formatter yields no summary.
8. **`save_max_concurrent`** writing `scheduling.max_concurrent_agents` with
   silent-until-restart semantics is correct. The TUI
   prints `Settings saved` and shows the new value in the wizard default.
9. **Capability flags in observer mode** all stay `true`; let
   `run_task`/`run_all`/`start_watch` fail with the lock holder's PID (see #3).
   The flags mean "this build supports the operation", not "it will succeed now".
10. **`bell()`** is yours: write `\x07` to stdout directly, outside the draw
    loop. The TUI calls it only for `failed` and `review` transitions and only
    when `notifications.bell` is set — never for `done`.

Two notes back:

* **A lossy `poll_events` is fine.** The TUI's 80 ms batching, 500-message ring
  and 2 KB detail caps already assume `agent:output` can be dropped under load
  (ARCHITECTURE §6.5). Log the lag; never fail the call.
* **Feeding `DiskObserver` into the same queue is exactly right.** The TUI has
  no way to tell the sources apart and must not: `observer_mode` in
  `LaunchContext` only drives the header chip and the 3 s poll interval.

## Copy request and result boundary

`copy_text(text)` delegates to `copy_text_via(text, CopyTarget::Auto)`. The explicit targets are Auto, Terminal
and Native. Implementations return a `CopyTicket` or an immediate safe error; the default facade declines copy
and drains no results. `EngineBackend` overrides both `copy_text_via` and `poll_copy_results` with its CLI
transport. A ticket is registered with the full viewer/editor/field origin before input handling returns.

Only the viewer Tick hook drains `poll_copy_results`, including when no viewer is open. Outcomes are
`NativeWritten`, `TerminalRequested` and `Failed(String)`, with per-request fixed `guidance`. The bounded
`CopyFeedbackStore` correlates full origins and generations; completed-but-undrained tickets count toward the
two-ticket transport limit. Dismissal hides presentation without canceling a pending association; obsolete
origins are drained without presenting a result on another field. The TUI never calls arboard or writes OSC.

The CLI serializes bounded OSC 52 on its output sink or queues the native worker in orch-infra. Native retains
one clipboard handle, at most two tickets and a three-second UI deadline. An OS stall is not cancellable; no
worker replacement or uncertain-result fallback is allowed. Explicit Native never writes OSC, including on
failure. Auto chooses terminal under web/SSH/TMUX/STY; otherwise it attempts the host route, with terminal
fallback only for definite initial unavailability. The browser PTY marker and first-party write-only handler
belong to orch-cli. Their gesture/denial/fallback UI is separate from the terminal-request acknowledgement.
