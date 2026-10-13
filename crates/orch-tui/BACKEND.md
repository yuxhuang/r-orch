# `TuiBackend` — the engine facade the dashboard needs

`orch-tui` does **not** depend on `orch-engine`. Everything the dashboard needs
from the rest of the system goes through one trait, `orch_tui::backend::TuiBackend`
(`src/backend/mod.rs`), implemented in the TS original by the ~45 callbacks that
`src/cli/commands/tui.ts` passes to `App`. The CLI (WP-5) implements this trait
over the DI container; the tests use the in-crate `FakeBackend`.

Everything below is the contract. If you need a shape changed, say so rather than
widening it locally — the TUI's parity tests are written against this surface.

## Two shape changes from the TS callbacks

1. **Synchronous.** Every method returns a value, not a future. A call is a
   *request*: an implementation must not block on long-running work. `run_task`
   asks the orchestrator to dispatch and returns; the outcome arrives through
   `poll_events`. Use `tokio::runtime::Handle::block_on` for the short
   store reads/writes (`refresh_*`, `create_task`, …) and `spawn` for anything
   that waits on an agent.
2. **Polled streams.** `onSubscribeEvents`, `onLoadHistory`,
   `onLoadModelCatalog` and `onCheckUpdate` were callback registrations. Here
   they are `poll_*` methods that the TUI drains **once per frame**:

   | TS callback | Facade method | Contract |
   |---|---|---|
   | `onSubscribeEvents(handler)` | `poll_events() -> Vec<OrchestratorEvent>` | Events since the last poll, oldest first. Drain a `broadcast::Receiver` without blocking; drop nothing silently — the feed's caps are the TUI's job. |
   | `onLoadHistory(onBatch)` | `poll_history() -> Vec<Vec<HistoryEntry>>` | Zero or more batches since the last poll. The TS host delivers two (the 3 newest runs, then the next 7, 30 tail events each, sliced to the last 200). |
   | `onLoadModelCatalog(onUpdate)` | `poll_model_catalog() -> Option<ModelCatalog>` | The newest catalog, if one arrived. The TUI rebuilds an open agent wizard in place when it does. |
   | `onCheckUpdate` | `poll_update() -> Option<String>` | A newer version, once known. The TUI starts `background_install` exactly once and then reads `installed_version()`. |

   This is what makes the TUI tests synchronous: push a key, `tick()`, draw,
   assert (ARCHITECTURE §8.4).

## Errors

`BackendResult<T> = Result<T, String>`. The TUI only ever renders
`Failed: {err}`, so the message is the whole contract — use the same text the
CLI would print for that failure.

## Capability flags

The TS App keys several bindings off "was this callback passed?"
(`onCreateTask && …`). Those predicates are the `can_*` methods, all defaulting
to `true`:

| Method | Gates |
|---|---|
| `can_create_task` | `n`/`N` and the `+ add task...` row on the tasks view, `/task add` |
| `can_add_agent` | `n`/`N` and `+ add agent...`, `/agent add` |
| `can_create_goal` | `n`/`N` and `+ add goal...`, `/goal add` |
| `can_force_stop_agent` | whether a *running* agent can be soft-deleted at all (otherwise the TUI refuses with `Cannot delete — agent "{name}" is running. Press S to stop first.`) |
| `can_paste_image` | the `⌘+V paste image` hint in the task wizards |

## Launch context

`context() -> LaunchContext` carries the non-callback props of the TS `App`:
project name, version, default adapter, the persisted palette / activity filter
/ notification preferences / max-concurrent value, and the watch-mode outcome
(`initial_watch_active`, `observer_mode`, `watch_error`). The TUI emits the
observer-mode banner or the watch-failure banner from these on launch, exactly
as `App.tsx` does.

## Operations

Grouped as in the trait; names map 1:1 onto the TS callbacks.

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
  this (docs/parity/tui.md §5.2) — switching an agent away from `shell` sends
  `command: Some("")`.
* `TaskFields::description`: always sent by the edit-task wizard (`?? ''`), so
  an emptied description clears it.
* `update_goal_status(.., force = true)` is only used by `C` on the goals view
  (achieve, cancelling cancellable tasks).
* `CreateTaskOpts::attachments` / `TaskFields::attachments` carry the pasted
  image paths; the TUI clears its pending list once the call returns.

## Wiring sketch (for WP-5)

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
clear the event bus, as `tui.ts` does.

Enable the Kitty keyboard protocol at startup
(`PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)`) so `Ctrl+Enter` is
distinguishable from `Enter` in the wizard textarea; the `Tab` confirm fallback
stays for terminals that refuse it.
