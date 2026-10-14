# Gate re-review — resilience (SRE)

**Candidate:** `ed05007` (final release candidate), worktree `../r-orch-wt/gate-review`
**Previous review:** [gate-sre-review.md](gate-sre-review.md) — verdict **block** on B1, with conditions C1–C5
**Reviewer:** gate-sre
**Date:** 2026-10-14
**Scope:** my block and my conditions only, re-checked against the code. Read-only; nothing in the candidate worktree was modified.

---

## Verdict: **pass**

The blocking defect is fixed, all five conditions are met, and the one advisory item I expected to be contested (F2) was taken as well. Every fix is accompanied by a test that fails if the fix is reverted — which is what I was most concerned about, since four of the seven items live in paths no parity scenario exercises.

Two advisory items from the first review remain open by choice; neither gates this pass and both are recorded in §3.

---

## 1. Item-by-item

### B1 — index rebuild silently dropped a corrupt entity file and persisted the loss — **MET**

`crates/orch-infra/src/storage/index.rs:127-130`:

```rust
match item? {
    Some(item) => items.push(item),
    None => continue,
}
```

The `?` propagates a parse error; only `Ok(None)` — a file that vanished between `list_files` and the read — is skipped. That restores the distinction `read_yaml` (`fs_utils.rs`) draws between "absent" and "unparseable", so a truncated `tasks/tsk_X.yml` now fails the rebuild loudly instead of producing a shortened `_index.json` that the caller persists. The comment at `:116-126` records the old behaviour, why it was wrong, and the TS semantics it was mistakenly claimed to match — the false "the TS version drops nulls the same way" claim is gone.

**Tests, both halves of the distinction:**

- `an_unreadable_entity_file_fails_the_rebuild_rather_than_shortening_it` (`index.rs:327`) — inverted from the original assertion, and it asserts the error **names the file** (`:347-349`), so an operator can act on it;
- `a_missing_entity_file_is_still_skipped` — confirms the `Ok(None)` path did not regress into an error.

Verified green: `cargo test -p orch-infra --lib storage::index` → **12 passed, 0 failed**.

### C1 — signal arms installed before the first tick — **MET**

`crates/orch-engine/src/orchestrator.rs:439-445` installs `SignalArms` immediately after the lock is taken and **before** any fallible startup work or `tick_once()`. `start_watch` was split: the fallible half is now `start_watch_prepared(signals, unsubscribe)` (`:470-495`), which runs state load, both recovery sweeps, the state save, the first tick, and only then `spawn_watch_loop(signals, …)`. The arm is created in the outer function and moved in, so it exists for the whole window.

The comment at `:416-437` states the mechanism correctly, including the part that matters: creating the arm is what replaces the default disposition, so a signal arriving mid-tick is held by tokio's signal driver and handled when the loop next polls, rather than killing the process.

**Test:** `a_signal_during_the_first_tick_shuts_down_instead_of_killing_us` (`orchestrator/tests/resilience.rs:752`). This is a real test, not a structural one — it wraps the task store to slow the first `list()`, waits on a `Notify` until the tick is demonstrably under way (so the signal cannot land before the arm exists), sends an actual `SIGTERM` to the test process, and asserts `wait_for_stop` resolves within 10 s. If the arms moved back after `tick_once`, the test binary would be killed outright, which is a loud and unambiguous failure. The old structural test (`the_watch_loop_installs_both_signal_arms`) is retained alongside it.

### C2 — startup sweep of orphaned `Running` runs, plus the child kill — **MET**

`cleanup_orphaned_running_runs` (`crates/orch-engine/src/orchestrator/recovery.rs:166-217`), called unconditionally from `start_watch_prepared:485` — **not** gated behind `if !dead.is_empty()`, which was the defect. It selects runs with `status == RunStatus::Running` whose id is absent from `state.running` (`:183-186`), then for each:

1. kills the unparented child with the default 10 s grace, guarded on `run.pid` being present **and** alive (`:191-198`) — so a run record with no pid, or whose process is already gone, is not signalled;
2. finishes the run `Cancelled` with reason `"Orchestrator restarted"`, matching the wording the `Preparing` sweep already uses.

The loop is sequential rather than `join_all`, with the reason given (each may spend the full kill grace, and the population is small) — correct, and it avoids a thundering herd of SIGTERMs on a bad restart.

**Tests, both directions:**

- `startup_cancels_a_running_run_with_no_entry_and_kills_its_child`
- `startup_leaves_a_running_run_that_still_has_its_entry_alone` — the important negative: this is what stops the sweep from killing the current process's own in-flight dispatch.

### C3 — operator documentation — **MET**

`docs/RUNBOOK.md` (14.9 KB) and `README.md` (7.1 KB) both now exist. The runbook covers every item I asked for, each as its own section: corrupt lock file (`:27`), `serve --once` hangs on `review` (`:76`), rebuilding an index after a lost update (`:109`), `state.json` parse failure (`:138`), temp-file litter (`:163`), `serve:started` precedes the lock check (`:185`), plus debugging (`:197`) and two sections I had not asked for.

Spot-checked for correctness rather than mere presence:

- **Corrupt lock (`:27-74`)** names the exact symptom including the literal `undefined`, and — this is the part that matters — documents all three ways the product misdirects: the `Use: orch status` hint, `status` reporting the project unlocked, and `status` printing `watching` off a stale `state.json` pid with no liveness check, plus the TUI's silent observer mode. It says plainly that all three are faithful to the original, which is why the remedy is documentation. The recovery procedure leads with **"Confirm no orchestrator is running. Do not skip this; deleting a live lock lets two orchestrators dispatch the same tasks"** — a safety step I did not ask for and which closes the obvious footgun of the advice itself.
- **Index rebuild (`:109-136`)** correctly scopes itself to the *lost-update* cause and keeps it separate from the now-loud corrupt-file case, so it did not go stale against the B1 fix. It states that no command does this for you and nothing reports the divergence.

### C4 — tracing subscriber, stderr only — **MET**

`init_tracing` (`crates/orch-cli/src/lib.rs:712-731`), called from `run()` at `:48`:

- **stderr** via `.with_writer(std::io::stderr)` — stdout stays the byte-compared contract;
- **silent by default**: `EnvFilter::new("off")` when neither variable is set (`:720`), so no invocation's output changes;
- `RUST_LOG` takes a full filter spec, `ORCHESTRY_DEBUG` (the existing cause-chain flag) turns on `debug` (`:715-717`);
- `try_init` rather than `init` (`:725`), so a test binary that installed its own subscriber cannot abort the process over logging.

The 42 previously-dead call sites now have a destination — including the two I flagged as load-bearing: `lock.rs` (the only signal distinguishing "someone holds the lock" from "I could not read the lock file", both of which print `PID: undefined`) and `manager.rs` (failed child reap, after which `is_alive` keeps answering `true`).

### C5 — fsync before rename, and the parent directory — **MET**

`atomic_write_with_mode` (`crates/orch-infra/src/storage/fs_utils.rs:69-128`):

- `file.sync_all().await?` after the write and **before** the rename (`:96`), as a hard error — the comment states the failure it prevents (a correctly named file full of zeroes);
- parent-directory `sync_all` after the rename (`:122-126`), **best-effort** under `#[cfg(unix)]`, correctly justified: a filesystem that refuses to open a directory for sync is not a reason to fail a write that already succeeded.

`sync_all`/`sync_data` appear nowhere else in the repo, which is right — `atomic_write` is the single chokepoint every store writes through.

**ARCHITECTURE text reconciled:** `docs/ARCHITECTURE.md:409-410` now specifies the order explicitly — "write temp → **fsync the temp file** → rename over the target → **fsync the parent directory**" — with the reason both are required. The documented-vs-implemented divergence is closed in the direction of implementing it.

**Unrequested hardening I'll note approvingly:** the temp file is now opened with `create_new(true)` (`:78-80`) instead of `tokio::fs::write`, so the open fails rather than following a symlink or truncating an existing file. With an 8-hex random name, a collision means another writer is mid-flight on the same path and failing is the correct answer.

### F2 (advisory in the first review) — lock released on `start_watch` errors — **MET**

`crates/orch-engine/src/orchestrator.rs:493-501`: a single `match` on `start_watch_prepared`'s result clears `lock_acquired` and calls `lock.release()` on **every** error path. The comment at `:445-448` gives the motivating case precisely — `serve` and `run --watch` exit so their dead pid makes the file stealable, but the TUI stays up in observer mode holding a lock stamped with its own *live* pid, which nothing reclaims for the full staleness window.

**Test:** `a_failed_startup_releases_the_lock_it_acquired`.

---

## 2. Verification performed

| Check | Result |
|---|---|
| `cargo test -p orch-infra --lib storage::index` | 12 passed, 0 failed |
| `cargo test -p orch-infra --lib storage::fs_utils` | 47 passed, 0 failed |
| `cargo test -p orch-engine` | 468 + 5 + 2 + 3 passed, 0 failed |
| Candidate tree clean at `ed05007` | `git status --porcelain` empty |

Shared `CARGO_TARGET_DIR` as instructed. I did not re-run the full cold gate or the parity harness — the lead reports 12/12 steps, 2581 tests, 58/58 parity, 11/11 probes and `cargo-deny` clean, and my own prior run measured 57/57 parity at `0b67e4d`. My scope here was the seven items above, each of which I verified in the source and exercised through its own test.

Worth restating from the first review: **none of B1, C1, C2 or C5 is observable to the parity harness.** It compares agreement with the oracle on well-formed inputs and orderly shutdowns; these four live in malformed-input and abrupt-termination paths that no scenario induces. The green gate is necessary evidence, not sufficient evidence, for the items in this re-review — which is why I checked each fix's test for non-vacuousness individually rather than reading the totals. The C1 test in particular would announce a regression by killing the test binary, and the B1 pair covers both sides of the absent/unparseable distinction.

---

## 3. Residual advisory items — open by choice, not gating

Neither was a condition; both were "cheap divergence or ship + document" recommendations, and declining them is defensible.

1. **`kill_with_grace` still returns when the direct child dies** (`crates/orch-infra/src/process/manager.rs`, unchanged: the `while` loop returns on `!is_alive(pid)` before the final `SIGKILL`). A group member forked inside the kill window therefore survives unsignalled — measured, with the preserved gate log in that file's `the_group_kill_reaps_the_children_of_the_child` test comment. I recommended a final `kill(-pid, SIGKILL)` sweep after the child is confirmed dead, at near-zero differential cost. It was not taken, and strict parity with `killWithGrace` is a legitimate reason. **It is also not documented** — the runbook does not mention that an agent's grandchildren can outlive a cancel or stall kill. One sentence there would close the gap; the behaviour itself can stay.
2. **`agent.config.timeout_ms` is still inert** — enforced in neither implementation (only `stall_timeout_ms` is, and only from `reconcile`, which runs only in the watch loop). Not documented as inert in `RUNBOOK.md`, `README.md` or `PARITY.md`. A config field named `timeout_ms` that does nothing reads as a safety net and is not one, which is worse than its absence. One line in the runbook.

For the record, `--allow-foreign-project` (F6 in the first review) **was** reconciled: `docs/ARCHITECTURE.md:66` and `:1308` now state the valve is retired rather than deferred, with the reasoning, instead of promising a flag that was never built.

---

## 4. Verdict line

**pass** — B1 fixed and its test inverted (`index.rs:127-130`, `:327`); C1–C5 all met with non-vacuous tests (`orchestrator.rs:439-445`/`:470-495`, `recovery.rs:166-217`, `docs/RUNBOOK.md`, `lib.rs:712-731`/`:48`, `fs_utils.rs:96`/`:122-126`); F2 taken as well (`orchestrator.rs:493-501`). Two advisory items remain open by choice (grandchild survival on kill, inert `timeout_ms`), each closable with one sentence of documentation and neither gating the release.

---

**Addendum: §3 items closed by `9df8ba9`.** Both residual advisory items are now documented and verified: `docs/RUNBOOK.md:232` ("Stray agent subprocesses after a cancel" — mechanism, a `ps`/`pgid` procedure for spotting survivors, an explicit group-kill cleanup, and the pid-recycling caveat) and `docs/RUNBOOK.md:276` ("`timeout_ms` is not a timeout" — with a bounded/not-bounded table that names the `orch run <id>` no-stall-detection gap, and `timeout 3600 orch run …` as the external backstop), backed by `PARITY.md §6.5:38,:51-69` which carries the authoritative note and distinguishes `ReviewRunner`'s own enforced per-criterion `timeout_ms`. Spot-checked against the source: the process group id is the child's pid (`process_group(0)`, asserted via `getpgid` in `manager.rs`), the run record carries that pid, and the `§6.5` cross-reference resolves. Verdict unchanged — **pass**, with nothing outstanding.
