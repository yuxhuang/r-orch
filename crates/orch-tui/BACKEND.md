# `TuiBackend` — the daemon facade the dashboard needs

`orch-tui` does **not** depend on `orch-engine`. Everything the dashboard needs
from the rest of the system goes through one trait, `orch_tui::backend::TuiBackend`
(`src/backend/mod.rs`). The CLI implements this trait as `DaemonBackend` over typed daemon requests
(`crates/orch-cli/src/tui/backend.rs`); the tests use the in-crate `FakeBackend`.
The backend opens no project store and starts no daemon.

Everything below is the contract. If you need a shape changed, say so rather than
widening it locally — the TUI's tests are written against this surface.

## Shape of the facade

1. **Synchronous.** Every method returns a value, not a future. A call is a
   request to the daemon and can hold the draw thread while awaiting its reply.
   `run_task` asks for dispatch; later activity arrives through `poll_events`. `DaemonBackend` uses `tokio::runtime::Handle::block_on`
   for bounded socket requests on the draw thread. Background work opens
   separate sessions; the frame loop does not wait for an agent to finish.
2. **Polled streams.** Events, history, the model catalog and update checks are
   `poll_*` methods that the TUI drains **once per frame**, not callback registrations:

   | Stream | Facade method | Contract |
   |---|---|---|
   | Events | `poll_events() -> Vec<OrchestratorEvent>` | Bus events since the last poll, oldest first. Drain the backend's in-memory queue without blocking; it drops the oldest event at its 4096-event cap and logs that drop. |
   | Store changes | `poll_observations() -> Vec<Observation>` | Coalesced store invalidations and resync markers from the dedicated daemon subscription; no filesystem poll. |
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
/ notification preferences / max-concurrent value, and the initial dispatch switch.
`DaemonBackend` sets `initial_watch_active` from `ui.bootstrap`,
`observer_mode` to `false` and `watch_error` to `None`. Opening the dashboard
does not enable dispatch; pushed state changes refresh its WATCHING / IDLE chip.

## Operations

Grouped as in the trait.

* **Refresh** — `refresh_tasks`, `refresh_agents`, `refresh_goals`,
  `refresh_state`, `list_teams`, and `refresh_dispatch`. Launch reads the views;
  pushed invalidations mark dependent views dirty for a 150 ms debounced refresh.
  A resync refreshes every view immediately. `last_read_seq` reports the daemon
  commit ordinal observed by the last read, so already-observed changes need
  no duplicate entity read (`orch-tui/src/app.rs`, `refresh.rs`). Dispatch is
  re-read on every state invalidation; Runs/State-driven timing refreshes have
  their own five-second throttle.
* **Tasks** — `create_task(title, CreateTaskOpts)`, `update_task(id, TaskFields)`,
  `delete_task`, `run_task`, `run_all`, `cancel_task`, `retry_task`,
  `assign_task(task, agent)`, `approve_task`, `reject_task(id, feedback)`.
* **Agents** — `add_agent(name, adapter, AgentOpts)`,
  `update_agent(id, AgentFields)`, `delete_agent`, `force_stop_agent`,
  `toggle_autonomous(id, enabled)`, `disable_agent`, `enable_agent`.
  `take_agent_diagnostics() -> Vec<String>` (default empty) hands out, once,
  the effort diagnostics of the agent the last successful add/update wrote;
  the dashboard shows each as a yellow `⚠` line. The backend sends authored
  agent requests; daemon handlers take `.definition.lock` and validate against
  the project config read inside it (`orch-daemon/src/ops/authored.rs`).
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
  `crates/orch-cli/tests/tui_backend.rs`. The request runs in the daemon;
  the dashboard also receives the daemon's bus through its observation session.
* **Watch mode** — `start_watch`, `stop_watch` send `dispatch.set on/off`
  through `/watch` and `/pause`. Turning dispatch off leaves running agents alive.
* **Settings persistence** — `save_palette`, `save_activity_filter`,
  `save_notifications`, `save_max_concurrent`, `complete_onboarding`. These
  send daemon requests: preference patches update global config, max concurrency
  updates project config, and onboarding updates orchestrator state
  (`orch-daemon/src/ops/authored.rs`). The dashboard writes none of these files.
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

## Wiring and lifecycle

```text
Connector -> DaemonBackend::launch
                | subscribe to all stores + bus first
                | open request session, verify same daemon instance, ui.bootstrap
                v
          plain draw thread: TuiBackend calls -> bounded socket requests
                ^
background observation pump -> event queue + coalesced store invalidations
                |
         gap/resync -> subscribe again -> refetch every view
         EOF/new instance -> no-daemon screen -> explicit retry or quit
```

The implementation is `crates/orch-cli/src/tui/backend.rs`; the connection and
retry loop is `tui/command.rs`. `Connector` opens sessions without starting a
process. The backend subscribes before reading so a change between subscription
and bootstrap still reaches the dashboard. The observation pump only fills
memory; the draw loop drains it through `poll_events` and `poll_observations`.
There is no disk observer or periodic state poll. History, model-catalog loads,
prompt-mode writes and run observations use separate sessions.

The CLI runs `orch_tui::run_until_with_view_intent` on a plain thread because
synchronous facade calls use `Handle::block_on`. `orch-tui` owns raw mode,
alternate screen, keyboard enhancement flags and terminal restoration. Closing
the dashboard drops its backend and observation tasks; it does not stop dispatch
or the daemon's agents. On a lost session, `tui/command.rs` returns to the
`r retry · q quit` screen. Retry makes a new subscription and bootstrap; mutations
are not replayed.

The source-level push tests are in `crates/orch-cli/src/tui/push_tests.rs`:
subscription-before-read, store and bus delivery, commit ordinals, gap/resync,
new daemon instance and observation EOF. `crates/orch-cli/tests/tui_backend.rs`
exercises facade operations through a daemon harness.

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
3. **Run observation is separate from the frame loop.** `run_task` and
   `run_all` open a background session and wait for its first streamed event,
   terminal result or refusal for at most 60 seconds. If that confirmation
   wait expires, the method returns `Ok(())` while observation continues; it
   does not prove that a child started. The remaining run stream is
   drained in the background; the dedicated observation feed supplies activity.
   `start_watch` waits for the dispatch-switch reply. Ordinary refusals return
   `Err`; a lost session returns to the no-daemon screen
   (`crates/orch-cli/src/tui/backend.rs`).
4. **`stop_watch`** waits for the daemon's `dispatch.set off` reply and returns
   its result. It does not shut down the daemon or cancel running agents;
   the header follows the daemon's dispatch switch.
5. **The update facade is intentionally inert.** Leave `poll_update`,
   `background_install` and `installed_version` at their trait defaults
   (there is no self-update and no network access). No test requires a real version —
   case `v070` scripts one through `FakeBackend`. The header update chip then
   simply never appears.
6. **`refresh_state`** sends `state.get`; it does not read `state.json` in the
   frontend. `refresh_dispatch` separately reads `daemon.status`, because the
   dispatch switch lives in daemon memory. Refreshes follow bootstrap and
   invalidations, with no observer/watch periodic poll.
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
8. **`save_max_concurrent`** sends `config.set` for
   `scheduling.max_concurrent_agents`. The daemon refreshes engine configuration
   after the write; subsequent dispatch uses the new cap
   (`crates/orch-daemon/src/ops/authored.rs`, `orch-engine/src/container.rs`).
9. **Capability flags** describe supported UI operations. The production
   backend has no observer mode; run and watch requests return the daemon's
   refusal when they cannot execute (including after ownership loss).
10. **`bell()`** is yours: write `\x07` to stdout directly, outside the draw
    loop. The TUI calls it only for `failed` and `review` transitions and only
    when `notifications.bell` is set — never for `done`.

Two notes back:

* **A lossy `poll_events` is fine.** The TUI's 80 ms batching, 500-message ring
  and 2 KB detail caps already assume `agent:output` can be dropped under load
  (ARCHITECTURE §6.5). Log the lag; never fail the call.
* **Keep store invalidations separate from the activity queue.**
  `ObservationQueue` coalesces changes by store; a full bus-event queue may drop
  old activity, while store invalidations still cause the necessary reads.

## Copy request and result boundary

`copy_text(text)` delegates to `copy_text_via(text, CopyTarget::Auto)`. The explicit targets are Auto, Terminal
and Native. Implementations return a `CopyTicket` or an immediate safe error; the default facade declines copy
and drains no results. `DaemonBackend` overrides both `copy_text_via` and `poll_copy_results` with its CLI
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
