# Gate review — resilience (SRE)

**Candidate:** `0b67e4d`, worktree `../r-orch-wt/gate-review`
**Reviewer:** gate-sre (did not write this code)
**Date:** 2026-10-14
**Stance:** 0.5. Stance governed none of the calls below — every finding here is on the correctness/data-integrity floor, which stance never lowers.

---

## Verdict: **block**

One data-integrity defect blocks. **Everything else in scope is `pass with conditions`** — five conditions (C1–C5), enumerated below. The lock, `stop()`, startup recovery and the circuit breaker are better-reasoned than most production daemons I have reviewed, and the known-bug decisions in §14 are measured against the oracle rather than assumed. The block is one `if let` and its fix is ~4 lines.

Re-review needed on: the blocking finding, plus C1–C5. The "cheap fixes" section is advisory.

---

## 1. The blocking failure

### B1 — `scan_items` silently discards an unreadable entity file and persists the loss, diverging from the reference in the unsafe direction

`crates/orch-infra/src/storage/index.rs:116-120`:

```rust
let item = match self.format {
    ItemFormat::Yaml => read_yaml::<T>(&path).await,
    ItemFormat::Json => read_json::<T>(&path).await,
};
// A single unreadable entity file must not lose the whole index;
// the TS version drops nulls the same way.
if let Ok(Some(item)) = item { items.push(item); }
```

`read_yaml` (`crates/orch-infra/src/storage/fs_utils.rs:197-204`) deliberately distinguishes two cases: `Ok(None)` = file absent, `Err` = parse failure. `if let Ok(Some(..))` collapses them, so a malformed file is dropped as though it had never existed. `rebuild_index` (`index.rs:95-99`) then calls `write_index_unlocked(&items)`, **writing the shortened index back to disk**.

**Failure scenario.** One `tasks/tsk_X.yml` is truncated — power loss, the missing fsync (C5), a bad editor save. The next `task list` triggers a rebuild, drops `tsk_X`, and persists a `_index.json` without it. From then on `task list`, `status`, `serve` and every dispatch path cannot see the task; `task show tsk_X` still finds it. No log line, no event, no non-zero exit. The loss is durable: the index on disk is authoritative until someone deletes it, and nothing detects the divergence — `doctor` has no index-consistency check (`crates/orch-engine/src/doctor_service.rs:98-135`).

**TS does the opposite.** `readYaml` (`parity/orch-ts/src/infrastructure/storage/fs-utils.ts:37-45`) returns `null` *only* for `ENOENT` and **rethrows a parse error**, so the `Promise.all` in `rebuildIndex` (`index-manager.ts:99-112`) rejects and the whole command **fails loudly**. The `item != null` filter there only drops files that vanished between `listFiles` and the read.

Consequences for the review record:

- the code comment "the TS version drops nulls the same way" is **false** for the case that matters;
- the test `an_unreadable_entity_file_is_skipped_not_fatal` (`index.rs:316-328`) **locks in the wrong behaviour**;
- the codebase is internally inconsistent — `crates/orch-infra/src/storage/run.rs:60-62` propagates a corrupt run file and fails the listing, the safe choice — so the unsafe side was picked precisely where the loss becomes permanent.

**Recommended action: fix.** Match on the error: propagate `Err`, keep skipping `Ok(None)`. Invert the test. **Differential cost: none** — it moves r-orch *toward* the oracle, which is the stronger argument for it. Take C5 (fsync) in the same change, since the two together are what make a truncated entity file likely in the first place.

**Why this blocks rather than being a condition:** silent, durable data loss, diverging from the reference toward the *less* safe behaviour, with a test and a code comment that would prevent anyone from noticing. None of the 57 green parity scenarios can see it, because no scenario truncates an entity file — the harness measures agreement on well-formed inputs, which is exactly where a silent-drop-on-malformed-input divergence hides.

---

## 2. Conditions

### C1 — Signal handlers are installed *after* the first tick (fix)

`crates/orch-engine/src/orchestrator.rs:446-447` runs `tick_once()` and only then `spawn_watch_loop`, which calls `SignalArms::install()` at `:719` — *inside* the spawned task. TS installs both handlers at `parity/orch-ts/src/application/orchestrator.ts:224`, **before** `await this.tick()` at `:232`.

The first tick reconciles and dispatches every ready task — `git worktree add`, agent spawns — so the window is seconds to minutes. A `SIGTERM`/`SIGINT` in it takes the process's **default disposition: immediate death.** No `stop()`, no wind-down, no state save, no lock release. Children are in their own process groups with no `kill_on_drop`, so they survive as orphans; `state.running` keeps their *live* PIDs, so the next startup's `cleanup_stale_running_entries` (`crates/orch-engine/src/orchestrator/recovery.rs:22-30`, which filters on `!is_alive`) leaves the entries alone and the tasks sit `in_progress` until stall detection fires (default 600 s).

`systemctl restart orch` during startup is the everyday trigger. **This compounds C2**: it turns the crash-mid-dispatch window from a kernel-level event into something an ordinary restart can hit.

**TS behaves better here** — this is an unintended divergence that lowers the floor below the reference.

- **Action: fix.** Build `SignalArms` in `start_watch` before `tick_once`, move it into the loop. **Differential cost: none** (moves toward TS).
- **Test gap:** `crates/orch-engine/src/orchestrator/tests/resilience.rs:236` asserts only that `install()` returns both arms — never *when* they are installed.

### C2 — Crash mid-dispatch orphans the run record and the child process (fix)

`crates/orch-engine/src/orchestrator/dispatch.rs:525-542` orders: `adapter.execute` (child spawned) → `run_service.start` (run → `running`) → `state.claimed.shift_remove` + `state.running.insert` → `save_state`. Die anywhere in that window and `state.running` has no entry for a task whose child is already running.

On restart, `dead` is computed from `state.running` and is therefore empty, so:

- recovery's orphaned-task sweep is **skipped** — it is gated on `if !dead.is_empty()` at `recovery.rs:72`;
- `cleanup_orphaned_preparing_runs` (`recovery.rs:107-125`) filters `RunStatus::Preparing` **only**;
- `reconcile` (`crates/orch-engine/src/orchestrator/tick.rs:82-87`) iterates `state.running`, so it can never see the entry.

**Correction to the first-pass reading of this finding:** the *task* does self-heal. `reconcile_orphaned_tasks` (`tick.rs:265-297`) runs unconditionally on every tick and marks an `in_progress` task with no running entry as `failed`, emitting `task:orphaned`. What does **not** heal:

- the **run record stays `running` forever** — nothing anywhere sweeps `RunStatus::Running` orphans;
- the **agent child keeps running unparented**, writing into a workspace whose task is now `failed`.

- **Action: fix.** Sweep `Running` runs with no running entry — the same shape as the `Preparing` sweep already present. **Not currently listed in §14.**

### C3 — Three replicate decisions route their remedy to user-facing documentation that does not exist (document)

Owed by three separate accepted decisions:

| Decision | Obligation |
|---|---|
| PARITY.md §14.17 | "delete `.orchestry/orchestry.lock` if you see `PID: undefined`" |
| PARITY.md §11.1 | `serve --once` never returns while a task sits in `review` |
| ARCHITECTURE.md §6 A2 | concurrent-index hazard "belongs in user-facing docs — not only here" |

There is **no user-facing documentation in the repo** — no README, no runbook; `find` returns only fixture READMEs. Each `REPLICATE` ruling is defensible *only with* that document, so as shipped the justification for all three is incomplete.

Compounding it, the product actively misdirects in the lock case:

- the error's hint is `Use: orch status`;
- `check_lock` (`crates/orch-infra/src/storage/lock.rs:139-155`) is deliberately mtime-blind and reports the project **unlocked** on a corrupt lock, so the suggested command says nothing is wrong;
- `render` in `crates/orch-cli/src/commands/status.rs:67-71` reports `watching` whenever `state.pid.is_some()`, with **no liveness check** — after a `SIGKILL`, `state.pid` is still set because `stop()` never ran, so `orch status` says `watching` indefinitely. Faithful (`parity/orch-ts/src/cli/commands/status.ts:37` is identical), which is why it belongs in the document rather than in the code;
- in the TUI — the dominant UI — `EngineBackend::launch` (`crates/orch-cli/src/tui/backend.rs:149-157`) converts the refusal into **silent observer mode** with a badge and no message, so the operator sees a dashboard apparently watching someone else's orchestrator, forever.

**Action: ship + document.** One operator runbook covering: the corrupt-lock wedge and its manual remedy; the `serve --once`/`review` hang and its three remedies (`approval_policy: auto`, set `review_criteria`, approve out of band); `rm <entity>/_index.json` to force a rebuild after a lost update; `rm .orchestry/.state.json` after a parse failure; the `.state.json.*.tmp` litter; and that `serve:started` precedes the lock check so it must not be read as "the daemon is up". **Zero differential cost.** This is the condition I weigh most heavily after the block.

### C4 — No `tracing` subscriber is ever installed (fix)

`tracing-subscriber` is a declared production dependency (`crates/orch-cli/Cargo.toml:65`) and **no `.rs` file in the workspace references it** — nothing calls `init()` or `set_global_default`. All 42 `tracing::` call sites write to nowhere in every build.

Dead where it matters:

| Site | What is lost |
|---|---|
| `crates/orch-engine/src/lock.rs:70` | the **only** signal distinguishing "another process holds the lock" from "I could not read/write the lock file" — both print `PID: undefined` |
| `crates/orch-infra/src/process/manager.rs:166` | "failed to reap child" → the child stays a zombie and `is_alive` keeps answering `true` |
| `crates/orch-infra/src/process/manager.rs:59` | "refusing to signal: not a single process id" |
| `crates/orch-cli/src/tui/backend.rs:155` | the observer-mode record whose own comment claims "it is logged rather than dropped, since otherwise nothing tells the user which PID holds the lock" — **false as built** |

**Action: fix.** Install a subscriber gated on `ORCHESTRY_DEBUG`, already the established escape hatch (`crates/orch-cli/src/lib.rs:126`), writing to stderr. **Differential cost: none** when the variable is unset; the harness does not set it.

**Correction to one claim I checked and rejected:** event-bus lag is **not** affected. `note_lag` (`crates/orch-engine/src/events.rs:428`) counts it, `skipped()` exposes it, `report_dropped_events` (`crates/orch-cli/src/logs.rs:543-554`) prints `warning: N event(s) dropped` to stderr from **every** reader including the serve pump (`logs.rs:520`, `:529`), and `run <id>` falls back to the run record when the count grows. That path is clean and strictly better than TS can manage. ARCHITECTURE R4's mitigation is real; only the `tracing::warn` half of it is dead.

### C5 — `atomic_write` has no fsync, and ARCHITECTURE.md specifies one (fix, or correct the doc)

`docs/ARCHITECTURE.md:390`, verbatim:

```rust
/// temp file + fsync + rename. Temp name: `.{basename}.{8 hex}.tmp` in the same directory.
pub async fn atomic_write(path: &Path, content: &str) -> Result<()>;
```

The implementation (`crates/orch-infra/src/storage/fs_utils.rs:34-61`) is `tokio::fs::write(tmp)` then `tokio::fs::rename(tmp, path)`. No `sync_all` before the rename, no parent-directory fsync after; `sync_all`/`sync_data` appear **nowhere in the repo**.

Process-crash safe (rename means a reader sees old-or-new, never partial). **Not machine-crash safe:** on power loss the rename can be durable while the data is not — ext4's `auto_da_alloc` partially covers rename-over-an-existing-file, but not a new file, and not XFS or btrfs. The result is a zero-length or garbage `_index.json`, entity `.yml` or `state.json` — which is precisely the truncated file that **B1 then silently and permanently deletes**.

TS has the same gap (`writeFile` + `rename`), so the *behaviour* is faithful; the **documented-vs-implemented divergence is not**.

- **Action:** implement the fsync (zero observable output change; one state write per tick, so the cost is irrelevant), or correct `ARCHITECTURE.md:390`. Given B1, implement it.

---

## 3. Further findings — ship + document, or cheap divergence

### F1 — `kill_with_grace` returns when the direct child dies, so a group member forked in the kill window survives

`crates/orch-infra/src/process/manager.rs:111-127` returns as soon as `is_alive(pid)` goes false, so the final `SIGKILL` is never sent. A grandchild forked between `kill(-pgid, SIGTERM)` and the child's death inherits the group **without having been signalled**. Measured and recorded: that file's `the_group_kill_reaps_the_children_of_the_child` test comment preserves a gate log showing `pid … state S ppid 1 pgid <target>` — a live, reparented, never-signalled grandchild. Load-dependent, which is why it only appeared in a cold gate.

**Faithful to TS** (`killWithGrace` has the same early return).

- **Recommended action: take the divergence.** After the direct child is confirmed dead, send one final `kill(-pid, SIGKILL)` to sweep the group. **Cost to the differential comparison: effectively nil** — kill errors are swallowed, nothing is printed, and no scenario asserts on surviving processes. The only behaviour change is that orphans die.
- If the team prefers strict parity: ship + document, and say in the runbook that an agent's grandchildren can outlive a cancel.

### F2 — `start_watch` leaks the lock file on any error after acquisition

`crates/orch-engine/src/orchestrator.rs:417-434` sets `lock_acquired = true`, then `load_state` / `cleanup_stale_running_entries` / `save_state` can each return `Err` with **no release**. For `serve` and `run --watch` the process exits and the dead PID makes the lock immediately stealable, so it self-heals. The **TUI keeps running** in observer mode while holding a lock file with its own live PID, never touched, for the full 60 s `LOCK_STALE` window.

TS is identical (`orchestrator.ts:206-221`). **Action:** ship, or a 3-line release on the error path — zero differential cost.

### F3 — No run timeout exists anywhere, in either implementation

`agent.config.timeout_ms` (default `3_600_000`) is **enforced nowhere** — in TS or Rust. Only `stall_timeout_ms` is, and only from `reconcile`, which runs **only in the watch loop**. Therefore:

- an agent that keeps emitting events but never completes runs forever;
- `orch run <id>` and `run --all` have **no stall detection at all** — a hung agent wedges the CLI indefinitely (`crates/orch-cli/src/commands/run.rs:78-106` ends the wait only on the completion event or a terminal run record).

**Action: document** that `timeout_ms` is inert. It reads as a safety net and is not one, which is worse than its absence.

### F4 — `serve --once` is unbounded, but is interruptible

`crates/orch-cli/src/serve/once.rs:103-121` loops while any task is non-terminal; `review` is non-terminal, and `orch init`'s default `agt_creator` ships `approval_policy: suggest`, so it is reachable out of the box (documented at PARITY.md §11.1). `SIGTERM` **does** break the loop via the shutdown-event flag at `once.rs:112`, so it is not unkillable — in CI it hangs until the job timeout.

**Action:** ship + document (C3). Optionally add `--max-wait <secs>`: an additive flag, default off, so **zero differential cost**.

### F5 — `orch doctor` checks nothing operational

`crates/orch-engine/src/doctor_service.rs:98-135` is exactly: an adapter `test()` per adapter, `git --version`, `git rev-parse --is-inside-work-tree`, `.gitignore` contains `.orchestry`, and a toolchain row. It exits 0 always. It detects **none** of: a stale or corrupt lock, a dead daemon PID, orphaned children, `.state.json.*.tmp` litter, tasks stuck in a transient state, an unparseable `state.json`, or an index/file divergence (B1, F7).

**Faithful to TS** (`parity/orch-ts/src/application/doctor-service.ts:40-80`) and rated L1. Adding a check changes `cmd-status-and-doctor`'s compared stdout, so a divergence here costs one weakened scenario. **Action:** defer; rely on C3. Worth revisiting after parity ships, because "doctor detects no production failure" is the honest operator-facing summary today.

### F6 — `--allow-foreign-project` was never built

`docs/ARCHITECTURE.md:66` says it "stays **default-on** until V32 is green"; `:1247-1248` calls it "the only real safety valve". Grep over `crates/`, `xtask/`, `fixtures/` returns zero hits.

Its stated precondition **is now satisfied**: V32's js-yaml cross-parse is implemented (`xtask/src/harness.rs:1814`) and ran as part of the green sweep below, and every cross-compat scenario passes in both directions. So omitting the valve is defensible — but the docs still promise it.

**Action:** update ARCHITECTURE §6 and §11 to record that V32 went green and the valve was therefore not built. Otherwise a future operator hunts for a flag that does not exist.

### F7 — No index↔file reconciliation, and `.state.json.*.tmp` litter is never swept

The documented recovery for the accepted cross-binary lost update (ARCHITECTURE §2.1 A2, PARITY.md §6.10) is "delete `_index.json`", but **no command does it** and nothing surfaces the divergence. The only symptom is `task show` and `task list` disagreeing. This is also the only recovery for B1.

Separately, `atomic_write`'s tmp cleanup exists only on its own two error paths (`fs_utils.rs:44`, `:53`). A process killed between the write and the rename — including via C1's unhandled-signal window — leaves litter permanently. The litter is **inert** (`list_files` filters by `.yml`/`.json`, so rebuild scans ignore it; it is gitignored) and acknowledged at `orchestrator.rs:519-523`, but it accumulates unbounded and nothing reports it.

### F8 — `serve:started` is logged before the lock check

`crates/orch-cli/src/serve/mod.rs:100-111` versus the `run_once`/`start_watch` calls at `:122`/`:133`. A `serve` about to exit 4 still emits one `serve:started` record and **no error record** — the error leaves via `?`, bypassing `done.cancel()`, `pump.await` and `logger.flush()`.

**Faithful** — TS does the same (`serve.ts:84` vs `:113`/`:131`) — and TS's `finally` flush is matched in effect, because every Rust sink write self-flushes (`crates/orch-cli/src/serve/logger.rs:101-112`), so no record is actually lost. **Action:** ship + document (C3): log consumers must not treat `serve:started` as "the daemon is up".

### F9 — Serve log file has no rotation or size cap

`crates/orch-cli/src/serve/mod.rs:46-57` opens `--log-file` in append mode. A write failure prints `Log file error: {err}` to stderr and continues — correct, losing the daemon over a full disk is worse. `--verbose` adds `agent:output` at 200 chars per event. TS the same. **Action:** ship; note in the runbook that rotation is the operator's job (logrotate / systemd).

### F10 — Bus-lag warning bypasses the log file

`report_dropped_events` prints to **stderr only**, never as a structured record, so an operator shipping only `--log-file` does not see it. Deliberate — stdout is byte-compared by the harness — but a gap. TS cannot lag at all.

### F11 — `ensure_dir` memoizes created directories process-wide

`crates/orch-infra/src/storage/fs_utils.rs:763-778`. If `.orchestry/tasks` is deleted under a running daemon, the cached hit skips `create_dir_all` and the next write fails `ENOENT` until restart. Low impact; noted for completeness.

---

## 4. What I checked that came back clean

**Lock mechanism** (`crates/orch-infra/src/storage/lock.rs`) — `O_CREAT|O_EXCL` creation with the kernel, not the preceding liveness check, as the guarantee; a process-wide serializing mutex (16-way concurrent acquire test yields exactly one winner); dead-PID steal; mtime staleness as the PID-recycling defence, with `touch` on **every** tick including failing ones (`tick.rs:36-39`); `EPERM`-means-alive; whitespace-tolerant parse so an older build's lock still reads; `check_lock` deliberately mtime-blind and non-mutating. The corrupt-lock matrix matches PARITY.md §6.7 row for row, and `require_lock`'s quirk is correctly identified as **dead code in the TS**, with the live path (`acquire` + reject any failed acquisition) verified against the oracle.

**`stop()`** — idempotent via `swap(true)`. The §14.15 fix is real: `shutdown_running_entries` (`recovery.rs:178-236`) reports each wind-down failure as `orchestrator:error` and continues, and the final save's error no longer costs the lock release (`orchestrator.rs:537-560`). Release precedes `stopped.send(true)`. The watch handle is dropped rather than aborted, and both reasons are correctly reasoned in the comment — a self-cancel when the breaker or a signal arm calls `stop()` from inside the watch task, and `.tmp` litter from an `atomic_write` cancelled between its temp file and its rename.

**Startup recovery** (`recovery.rs`) — `state.claimed` cleared unconditionally; dead running entries **cancelled, not failed**, so committed agent work is not redone; orphaned `in_progress` tasks cancelled only when a restart was actually detected; retry-queue entries pruned; orphaned `preparing` runs finalized while correctly excluding runs belonging to live entries. Matches PARITY.md §7.6. (Its gap for `Running` orphans is C2.)

**Circuit breaker** — 5 consecutive tick failures → `orchestrator:error{fatal:true}` + `orchestrator:shutdown` + `stop()`; the counter resets on success; `wait_for_stop` is released even when the breaker stops the loop from inside itself (`resilience.rs:299`).

**Timeout and retry bounds** — retry queue deduplicated by task id and capped at 100 with oldest-dropped (`orchestrator.rs:80`); reactive dispatch debounced 500 ms and capped at 10 reschedules; an unparseable `due_at` **stays queued** rather than being silently dropped (`tick.rs:304-315`); 30 s per-agent auto-seed cooldown so a failing agent is not handed a task every tick.

**Kill safety** — `signal_target` (`manager.rs:55-64`) refuses `0` and anything above `i32::MAX`, with a test (`signalling_a_non_process_pid_does_not_signal_us`) that would terminate the test runner if it regressed; `is_alive(0) == false`. PARITY.md §14.22's fix confirmed in full.

**Child reaping** — every spawn gets a background `child.wait()`, so an unreaped zombie cannot fool `is_alive`; each child leads its own process group via `process_group(0)`, asserted with `getpgid` rather than parsed out of `ps` (the comment records both flakes that cost a morning).

**Event-bus lag** — bounded at 4096 (`events.rs:37`); `RecvError::Lagged` handled in both `recv` (`:395`) and `try_drain` (`:408`), counted, exposed via `skipped()`, and surfaced to stderr by every reader including the serve pump. `run <id>` compares the count across calls and falls back to the run record, so a dropped `agent:completed` cannot hang it. Better than the TS can manage.

**Shutdown log completeness** — the serve logger pump is cancelled-and-drained rather than aborted, with a test that fails if the 6-record wind-down burst is lost (`serve/mod.rs:213`).

**Caching** — no cache outlives a tick: `get(id)` is never cached, every `save`/`delete` clears the whole cache, all three caches are invalidated at the top of every tick, and `load_state` re-reads `state.json` from disk inside every critical section (`orchestrator.rs:320-323`). A long-lived `orch serve` therefore **cannot** serve stale state or write back from a stale snapshot. The 500 ms debounced save writes from the same mutex-held slot, so it cannot resurrect a stale one; worst case is losing ≤500 ms of `last_event_at`.

**`state.json` read tolerance** — untyped read then per-field coercion via `OrchestratorState::from_raw`; unparseable JSON hard-fails rather than silently defaulting to an empty state (`state.rs:30-40`). Correct, and the right side of the trade-off that B1 got wrong.

**Writer inventory** — every `.orchestry/**` writer goes through `atomic_write` via `write_json`/`write_yaml`; the temp file is in the **target's own directory** so the rename is same-filesystem and atomic; names carry 8 random hex from nanoid so two concurrent writers cannot collide; cleanup on both error paths. Two deliberate exceptions, both correct: run-event JSONL through a cached `O_APPEND` handle with sub-`PIPE_BUF` lines, and the lock file via `O_CREAT|O_EXCL`.

**Concurrent-index lost update** — documented rather than asserted away, with the three invariants that actually hold (`crates/orch-infra/tests/concurrent_index.rs:1-22`): every entity file survives; `_index.json` is always valid JSON and always a **subset** of the files, never corrupt or invented; deleting the index recovers everything. A same-binary two-process test proves it is not a TS-vs-Rust artifact.

**Serve log records** — shapes match PARITY.md §11.1 field-for-field including severities. Tick failure → `orchestrator:error{error, context:"tick", fatal}`; dispatch failure → context `dispatch task <id>` / `adapter execution for <id>`; state-store failure → `debounced state save`; agent death → `agent:error{runId, agentId, error, errorKind}`. ~30 `emit_error` call sites cover recovery, collect and stop.

**Exit codes** — 1/2/3/4/5/6 match `crates/orch-domain/src/error.rs:104-122`, PARITY.md §328-336 and TS throughout. Held-lock and corrupt-lock both collapse to 4 with `PID: undefined`, which is replicated by decision (§14.17) and is part of C3's documentation burden.

**Cross-compatibility / rollback** — the strongest part of the submission. `crates/orch-infra/tests/cross_compat.rs` exercises all nine stores plus `config.yml` and `state.json` through the **real TS store classes** in both directions and interleaved in one directory, asserts every timestamp reads back as a JS `string`, carries a non-vacuousness self-check, and covers the lone-surrogate hazard. No `deny_unknown_fields` anywhere. I found **no field, file or format that Rust writes and TS cannot read.** `state.json` carries `version: 1` on both sides, read tolerantly by both. The only write on a read path is the `_index.json` rebuild, which TS also performs — so, B1 aside, **nothing irreversibly migrates on first touch** and "keep using the TS binary on the same project" holds up.

---

## 5. Differential evidence I generated

The evidence handed to me was **stale** and should be replaced: `target/gate-logs/parity.log` reports 51 scenarios with 46 skipped ("the Rust binary has no serve, run, status, logs, …") and 1 failure. It predates the WP-4/WP-5 CLI wiring — `not_wired` no longer exists, and the candidate binary registers all 17 commands. A reviewer reading that log concludes dissent item 8's accepted mitigation never ran.

I rebuilt `0b67e4d` and reran the harness in full:

| Run | Result |
|---|---|
| `cargo xtask parity --filter o7-` | **9 runnable, 9 green, 0 failed, 0 skipped** |
| `cargo xtask parity --exclude o7-` | **48 runnable, 48 green, 0 failed, 0 skipped** |

Covering every resilience scenario — `o7-stall-detection`, `o7-child-crash`, `o7-killed-mid-run`, `o7-cascade-failure`, `o7-concurrency-cap`, `o7-retry-backoff`, `o7-serve-once-exit-codes`, `o7-review-approve`, `o7-review-reject` — plus `cmd-corrupt-lock`, `index-tie-order` in both directions, `v7-run-*`, and every cross-compat and interleaved scenario. Logs:

- `$TMPDIR/sre-parity-o7.log`
- `$TMPDIR/sre-parity-rest.log`

Flake evidence reviewed (`target/flake-hunt/failures.log`): process-group spawn ×300, process-spawning suites ×20 rounds, full workspace ×5 with `--no-fail-fast` — **0 failures across 2334 tests**.

**Dissent item 8's mitigation is therefore satisfied in fact.** Note what the green does *not* cover: B1, C1, C2 and C5 are all invisible to it. The harness measures agreement with the oracle on well-formed inputs and orderly shutdowns; three of the four live in malformed-input or abrupt-termination paths that no scenario induces.

---

## 6. Process notes

- Two subagents I dispatched for the state-integrity and observability sweeps could not return through the handback path and delivered by message after I had drafted. Three of their findings were new to me; I verified each against the source before adopting it, and **corrected two**: the crash-mid-dispatch task *does* self-heal via `reconcile_orphaned_tasks` (C2 as written), and the event-bus lag path is clean rather than silenced (C4's correction). B1 came from that sweep and is the reason this is a block rather than a conditional pass — the credit is theirs.
- One claim I rejected on checking: that the dead `tracing` sites are a Rust-only regression *across the board*. For the lock and reap sites that holds; for bus lag it does not, because `report_dropped_events` reaches stderr independently of `tracing`.
