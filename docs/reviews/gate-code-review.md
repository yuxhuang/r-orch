# Gate review — code quality (Modern + Robust)

**Reviewer:** code-quality gate (read-only; did not write this code)
**Candidate:** commit `0b67e4d`, worktree `../r-orch-wt/gate-review`
**Date:** 2026-10-14
**Stance:** 0.5 (balanced)
**Scope:** Modern (no deprecated crates/APIs, idiomatic Rust 2024, consistent conventions, no port residue, `unsafe` justified) · Robust (edge correctness, error handling, test quality) · library-first compliance (ARCHITECTURE §4.8)

---

## Verdict: **PASS WITH CONDITIONS** (5 conditions)

This is a well-built codebase. The concurrency design is better than the TypeScript original it ports — not merely equivalent — and the parity-critical invariants are enforced structurally rather than documented and hoped for. Nothing I found is architectural, and every condition below is a small, enumerable fix.

Five conditions, not waivable: **M1–M4** and the `cross_compat` silent skip. Three of them (M2, M3, the silent skip) touch parity or safety rather than tidiness, which is why they are conditions rather than cleanups.

---

## 1. What I checked, and the evidence

| Check | Result |
|---|---|
| `cargo clippy --workspace --all-targets -- -D warnings` | **exit 0, zero warnings** — recompiled orch-infra/engine/cli, so not a cached no-op |
| `cargo test --workspace --no-fail-fast` | **2545 passed, 0 failed, 0 ignored**, 52 binaries, 0 build warnings |
| `unsafe` | **zero** in the workspace. `process/manager.rs:14` records that the process group comes from `Command::process_group(0)`, a safe std API, so none is needed |
| `todo!` / `unimplemented!` | **zero** |
| `panic!` in production code | **zero** — the single hit in `storage/fs_utils.rs` is inside `#[cfg(test)]` |
| `unwrap` / `expect` in production | ~7 `unwrap` (all `std::Mutex::lock`), 43 `expect`. Every one read is a real invariant: `LazyLock` static regexes, exhaustive map lookups, `"state loaded"` under the held guard. Exceptionally low density for ~50k LOC |
| Deprecated crates | no `serde_yaml` (archived); `serde_yaml_ng` 0.10 used. No `chrono`. Edition 2024, toolchain pinned 1.94.1 |
| `#[allow(...)]` in production | 3, all narrow and defensible (`too_many_arguments`, `should_implement_trait`, `field_reassign_with_default`) |
| `#[ignore]` | **zero** — nothing is parked |

**Method.** Deliberate sampling, not a full read. Read in full: `orchestrator.rs` (lock design), `orchestrator/tick.rs`, `storage/fs_utils.rs`, `storage/paths.rs`, `process/spawn.rs`, `process/manager.rs`, `tests/concurrent_index.rs`, `tests/cross_compat.rs`. Targeted reads in `orch-tui/src/app.rs`, `orch-tui/src/state.rs`, `orch-tui/src/messages/format.rs`, `orch-engine/src/goal_service.rs`, `orch-cli/src/output.rs`, `orch-cli/src/commands/init.rs`. Mechanical sweeps for the risk patterns above with `#[cfg(test)]` regions stripped. Two parallel sub-audits (test quality; library-first) whose every adopted claim I re-verified myself.

### Verified clean, and better than expected

- **The state mutex.** One `Mutex<Option<OrchestratorState>>` fuses the TS promise-chain lock with the data it guards. Lock order is documented (`state → active_collectors → cancels`, `orchestrator.rs:33-36`), and internal phases take `&mut OrchestratorState` as a parameter instead of re-locking — so the TS deadlock that `_handleRunFailure` exists to dodge **cannot be reintroduced; the compiler proves it**. This is the single best decision in the port.
- **No lost retries on partial failure.** I chased this specifically: `process_retry_queue` (`tick.rs:304-315`) removes due entries from `state.retry_queue` *before* dispatching, so an `Err` from `dispatch_task` could in principle drop a retry. It cannot, because `load_state` (`orchestrator.rs:320-323`) re-reads from the store unconditionally every tick, restoring the queue. Safe, though safe by a non-local argument.
- **Failure handlers that can themselves fail.** Both the crash path (`tick.rs:146-172`) and the stall path (`:204-224`) have an explicit fallback that frees the running slot and idles the agent when `handle_run_failure_locked` returns `Err`. The entry/agent leak I went looking for is handled, with a comment saying why.
- **`atomic_write`** (`fs_utils.rs:34-61`): temp-then-rename in the same directory, temp file removed on **both** failure paths, TS-compatible temp naming (`.{basename}.{8 hex}.tmp`) with the parity reason stated.
- **The YAML guard is unconditional.** `read_yaml_value` always calls `apply_merge()`; `write_yaml` always quotes ambiguous scalars. No caller-visible switch, so no caller can forget (`fs_utils.rs:12-16`, `:190-224`). Test vectors were measured against the vendored js-yaml (`node -e "yaml.load(...) instanceof Date"`), not derived by reading its regex.
- **`process/spawn.rs`** makes stderr draining structural with three documented policies (`Tail`/`Discard`/`Stream`), so no adapter can reintroduce the grok/antigravity pipe-buffer deadlock. Mutex poisoning handled with `unwrap_or_else(|e| e.into_inner())` rather than `.unwrap()`.
- **PID guard** (`manager.rs:41-53`): `0` and `> i32::MAX` are rejected before reaching `kill(2)`, with a comment explaining that `state.running` is populated from on-disk JSON so both are reachable from a stale or hand-edited file. This closes dissent-log item 16.
- **`find_project_root`** (`storage/paths.rs:224-236`) is TS-faithful and correct as written. Walks up, falls back to the resolved start. Acceptable **as code**; the exposure it creates is the subject of M2.
- **`validate_workspace_path`** (`paths.rs:180-192`) plus its test (`:388-407`) pins `..` traversal rejection *and* the sibling-string-prefix case (`/tmp/project-other` vs `/tmp/project`) — the classic `starts_with` bug, tested.
- **Unicode-safe truncation** where it matters: `slice_chars` (`tick.rs:512-517`) exists because autonomous task titles are user-supplied and a byte slice could split a code point.

---

## 2. Findings

### M1 — major. Dead no-op closure, and a comment asserting the opposite of the behaviour

`crates/orch-tui/src/app.rs:357-366`, inside `ingest_history`:

```rust
let mapped: Vec<PendingMessage> = messages
    .into_iter()
    .map(|(timestamp, mut pending)| {
        // History timestamps are preserved where parseable; the
        // relative clock reads them.
        let _ = parse_timestamp_ms(&timestamp);
        pending.detail = pending.detail.take();
        pending
    })
    .collect();
```

The closure does nothing. `parse_timestamp_ms` is computed and discarded; `pending.detail = pending.detail.take()` is a self-assignment no-op. `PendingMessage` (`messages/mod.rs:266-273`) has **no timestamp field**, so there is nowhere for the parsed value to go, and `state.rs:878` stamps every backfilled history message with the same `now_ms`.

The comment states the opposite of what the code does. `parse_timestamp_ms` is genuinely used only at `views/tasks.rs:305`. No test pins timestamp behaviour (`orch-tui/tests/live_updates.rs:531,540` assert rendered text only), so this is unexercised port residue — exactly the "dead code paths left from the port" class this gate was asked to find. It evaded clippy because `.take()` defeats the `self_assignment` lint.

**Fix:** delete the closure and the now-unused `app.rs:12` import, *or* add a timestamp field to `PendingMessage` and thread it into `Message::new`. Decide which — the comment currently promises the latter.

### M2 — major. The `--allow-foreign-project` valve does not exist

`docs/ARCHITECTURE.md:66`: the valve "stays **default-on** until V32 is green, not merely V8." `:1248`: "r-orch should refuse to write to a `.orchestry/` directory it did not create, behind a `--allow-foreign-project` flag. That is the only real safety valve and it costs almost nothing."

Grep finds **zero implementation** — no flag, no env var, no guard.

This compounds with `find_project_root`: a stray `~/.orchestry` (which a global config would create) makes every directory under `$HOME` a "project", and nothing prevents r-orch writing into a `.orchestry/` the TS binary created. V32's machinery *is* present and ran green here (`crates/orch-infra/tests/cross_compat.mts`, `cross_compat.rs` — 6 tests, including `rust_written_entities_read_back_in_typescript_with_string_timestamps`), so dropping the valve may be defensible — but see the silent-skip condition below, and the differential harness is not yet certified green at this commit.

**Fix:** implement the flag, or have the architect record in §11 that V32-green supersedes it. Do not leave the design doc asserting a safety valve that is not in the binary.

### M3 — major. L0 table padding diverges on astral characters

`crates/orch-cli/src/output.rs:277-280`:

```rust
/// Cell width as the TS computes it: ANSI stripped, then `.length`.
fn display_width(cell: &str) -> usize {
    strip_ansi(cell).chars().count()
}
```

The doc comment claims `.length`; `chars().count()` is not `.length`. JS `.length` counts **UTF-16 code units**, so an emoji is 2 there and 1 here. `display_width` drives both the column-width computation (`:212`) and the pad arithmetic (`:234`), so **every table row containing an astral character emits a different number of spaces than the TS binary**.

Table padding is parity-governed: ARCHITECTURE §4.8 lists "the table renderer's exact padding" among the mandated hand-rolls, and `crates/orch-cli/tests/routing.rs:62` already treats CLI output as byte-contractual. Task titles and agent names are user data; emoji in them are ordinary, not exotic.

What makes this a defect rather than a judgment call: **the same crate already does it correctly in five other places** — `logs.rs:460`, `commands/context.rs:130`, `commands/run.rs:377`, `commands/goal.rs:303`, `serve/logger.rs:484` all use `ch.len_utf16()`. The idiom is understood and applied everywhere *except* the one parity-governed path. There is no astral-character table test.

Two things that are **not** findings, to save the author time:
- `pad_end` (`:267-275`) is fine — its comment correctly scopes itself to the ASCII headers and labels in that module.
- `messages/format.rs:11-15` acknowledges the same `char`-vs-UTF-16 gap, but its argument ("can only shift where an already-truncated line ends, never which branch is taken") is valid for *truncation* and invalid for *padding*, where the count **is** the output.

**Fix:** `strip_ansi(cell).chars().map(char::len_utf16).sum()`, plus a table test with an emoji cell.

### M4 — major. `tracing` is wired up but no subscriber is ever installed

41 `tracing::{warn,debug,info,error}!` call sites across orch-cli, orch-engine, orch-infra and orch-tui, and **zero** subscriber initialization anywhere in the workspace — no `tracing_subscriber`, no `EnvFilter`, no `set_global_default`. Every diagnostic is discarded at runtime.

`tracing-subscriber` is declared at `crates/orch-cli/Cargo.toml:64` and never referenced. ARCHITECTURE §4.5's documented behaviour — "Must not write to stdout… Default to stderr at `warn`, controlled by `ORCHESTRY_DEBUG`/`RUST_LOG`" — is unimplemented; `orch-cli/src/lib.rs:126` uses a bare `eprintln!` behind an `ORCHESTRY_DEBUG` check instead.

For a daemon that spawns child processes and deliberately swallows some errors for parity, having no diagnostic channel at all is an operability gap, and it leaves 41 call sites that read as working instrumentation but produce nothing.

**Fix:** install a subscriber honouring `ORCHESTRY_DEBUG`/`RUST_LOG` on stderr per §4.5, or drop the dependency and the call sites.

### M5 — major. `cross_compat.rs` passes green when the oracle is missing

`crates/orch-infra/tests/cross_compat.rs:54-69`:

```rust
/// `None` when the vendored oracle has not been built.
fn new() -> Option<Fixture> {
    ...
    if !ts_root.join("node_modules/.bin/tsx").exists() {
        eprintln!("skipping: {} is missing — run parity/setup.sh ...", ...);
        return None;
    }
```

The four tests at `:160`, `:235`, `:343`, `:546` then `return` **green**. Cargo hides `eprintln!` for a passing test, so the file reports success while asserting nothing.

This is not an ordinary silent skip. These tests **are** V7/V8, and `ARCHITECTURE.md:1228` names the generalized js-yaml cross-parse "**the real mitigation**" for R1 — the highest-rated risk in the design. Combined with M2, the whole R1 mitigation chain has a quiet-failure mode *and* no safety valve. Nothing in the repo references `cross_compat` or pins the oracle's presence.

The convention already exists in-tree and this file violates it: the sibling `crates/orch-infra/tests/concurrent_index.rs:52-58` documents fixing this exact anti-pattern —

> "A missing binary used to `return None` with an `eprintln`, which cargo hides for a passing test — so the whole file reported green while asserting nothing. That is the one outcome a cross-binary test must not have, so a missing prerequisite now fails loudly and says what to run."

— and now hard-`assert!`s with the command to run (`:70-83`).

The oracle **is** present in this worktree, so these tests did run here; the risk is a clean machine or CI reporting green. **Fix:** convert the `None` return to an `assert!` mirroring `concurrent_index.rs:60-83`, and pin the oracle's presence in whatever runs the suite.

*Disclosure:* my first pass confirmed the 6 tests executed and recorded the suite as clean without reading the guard. The sub-audit caught it; I then verified it directly. Correcting that here.

---

### Minor findings and cleanups (not conditions)

| # | Finding | Location |
|---|---|---|
| m1 | **Four unused member-crate dependencies.** `shell-words` (used in **xtask only**, 6 sites; the orch-infra declaration has none) · `which` (zero `which::`/`use which`; §4.5 scoped it to the `cursor-agent → agent` probe, but `adapters/cursor.rs:23` hard-codes the fallback list and `adapters/util.rs:213` spawns `<bin> --version`) · `predicates` (dev-dep, zero uses) · `tracing-subscriber` (see M4). Cargo does not warn on these. | `orch-infra/Cargo.toml:31,37`; `orch-cli/Cargo.toml:64`; dev-deps in both |
| m2 | **`atomic_write` does not fsync before rename.** On power loss the rename can be durable while the data is not, yielding a zero-length entity file. TS does not fsync either, so this is parity-faithful — but fsync is *invisible at the byte boundary*, so closing it costs no parity. A decision, not a defect. | `storage/fs_utils.rs:41-52` |
| m3 | **Both `atomic_write` error branches are untested.** Nothing forces the write or the rename to fail, so "leaves no temp file on failure" is verified by reading only. Only the happy path is covered (`:838`). | `storage/fs_utils.rs:41-59` |
| m4 | **Weak assertion:** `assert!(found == lexically_resolve(&orphan) \|\| found == lexically_resolve(temp.path()))`. An OR that accepts two outcomes tolerates a wrong-ancestor walk. Pin the single expected value. | `storage/paths.rs:424` |
| m5 | **Test writes outside its tempdir and swallows the failure.** `std::fs::write(dir.path().join("..").join("secret.md"), "x").ok();` — the `!out.contains("secret")` half goes vacuous if the write fails, and it leaks an uncleaned `secret.md` into `/tmp`. Worth fixing given this project's documented history of test-hermeticity failures. | `orch-infra/src/skills.rs:275` |
| m6 | **Vacuous test.** `the_system_clipboard_never_errors_even_when_headless` — body is three `let _ = …;` and nothing else. Proves "no panic" while the name claims "never errors". Mild: clipboard is explicitly L2 degrade-to-empty. Rename or assert the degraded value. | `orch-infra/src/clipboard.rs:263` |
| m7 | **Four tests assert `is_ok()` and not the effect.** Each still passes if create/delete/update_status became a silent no-op returning `Ok`. Assert the goal exists / is gone / changed status. | `orch-engine/src/goal_service.rs:1028,1051,1070,1077` |
| m8 | **Latent suite disarm.** With `ORCH_BLESS_FIXTURES` set, `check()` rewrites the fixture and returns before `assert_eq!`, no-op'ing all 13 adapter wire-contract tests. Default off; nothing sets it. | `orch-infra/tests/adapter_fixtures.rs:118-126` |
| m9 | **Vacuous on an empty library.** Only assertions are inside `for name in available_skills()` (a `read_dir`). Mitigated by `:109` asserting `available.len() == advertised.len()` against a non-empty literal, so a sibling catches it. | `orch-domain/tests/shop_skill_references.rs:114,128` |
| m10 | **Nit:** `let _ = name;` to suppress an unused parameter. Rename the parameter `_name`; the comment above it is good and should stay. | `orch-tui/src/messages/format.rs:128` |
| m11 | **Stale manifest comments.** `Cargo.toml:95-96` "None is used yet" is now false (`tui-textarea-2`, `arboard`, `png` are used); `:117` still describes `semver` as being for an update check that PARITY §11.5 scope-cut. | root `Cargo.toml` |

---

## 3. Library-first compliance (ARCHITECTURE §4.8): **clean**

**No unjustified hand-rolls.** Every substantial hand-written implementation carries an in-code comment naming its §4.8 exemption — which is the policy working as designed, not merely being obeyed.

- **Process groups — justified hand-roll.** `process-wrap` is declared but unused, and `process/manager.rs:4-16` says why: the parity contract needs a *pid-keyed* API ("there is no registry of spawned processes, the PID in `state.running` is the only handle", PARITY §8.4), so `kill`/`is_alive`/`kill_with_grace` cannot go through a child handle at all; and `ProcessGroupChild::wait()` waits for the whole group to be reaped, which would delay `ExitSignal` past node's `close` event. It does **not** hand-roll `setsid`/`pre_exec` — the group comes from safe std `Command::process_group(0)` (`:144`), and `nix` is used only for `kill(pid,0)` liveness (`:86`) and `kill(-pid)` (`:101`). Exactly the split §4.8 prescribes.
- **TUI text editing — compliant + justified.** `tui-textarea-2` *is* used for the multi-line wizard (`wizard/engine.rs:23,279,937-943`); the crate exports its lib as `tui_textarea`, which is why a grep on the package name looks empty. The single-line editor is hand-written (`text/input.rs`, 501 lines), justified at `:3-9`: the parity surface is a kill ring (`Ctrl+K/U/W` → `Ctrl+Y`) plus an undo stack with a 500 ms snapshot debounce, a 50-entry cap and two *different* undo semantics for `Ctrl+Z`/`Cmd+Z` (docs/parity/tui.md §2.4), none of which `tui-input` models. Mirrored at `Cargo.toml:99-102`.
- **Unicode — compliant.** `text/cursor.rs:26-28` uses `unicode-segmentation`/`-width`/`-normalization`, with the mapping and a deliberate L2-scoped divergence recorded at `:3-5`. The grapheme/width work that *is* library work is delegated; only the parity-specific key semantics are hand-written.
- **Others compliant:** `arboard`+`png` (`clipboard.rs:12`), `similar` (`xtask/src/harness.rs:2090`), `regex`+`fancy-regex` (`models/parsers.rs:7-33`), `serde_yaml_ng` (24 sites), `liquid` (`template/engine.rs:31` — the §4.3 ~400-line fallback renderer was **not** built), `nanoid` (`ids.rs:19`), `jiff` (8 files). LRU is an `IndexMap` mirroring the TS `Map` (`messages/mod.rs:19,221`); `deep_merge` is 25 lines (`orch-domain/src/config.rs:102-126`). Nothing hand-rolled for version comparison — `orch update` is scope-cut with no network (`orch-cli/src/update.rs:1-19`).
- **PARITY-mandated hand-rolls all carry comments** (verified, correctly not flagged): PID lock format (`storage/lock.rs:1-5`), `scopesOverlap` (`orch-domain/src/scope.rs:4-8`), line reader 16 KB cap (`process/line_reader.rs:5-14`), retry backoff (`orch-domain/src/transitions.rs:136-141`), `--since` regex (`orch-cli/src/logs.rs:43`), table padding (`orch-cli/src/output.rs:195-196`), atomic-write temp naming (`storage/fs_utils.rs:30-39`). `strip_ansi` is a 20-line hand-roll with the TS regex quoted at `:282` — fine.

**Crates declared but unused:** `globset`, `process-wrap`, `tui-input`, `proptest`, `pretty_assertions`, `semver` are workspace-table-only declarations with no member crate depending on them — confirmed absent from `Cargo.lock`, zero build cost, and the manifest documents each as deliberate. Only the four in m1 are real unused member dependencies.

---

## 4. Test quality: strong, with one process-level hole (M5)

**Mechanical baseline across 2368 parsed test bodies:** 0 `#[ignore]`, 0 feature-gated tests, 0 `assert!(true)`, 0 `len() >= 0`, 0 tautological `assert_eq!(x, x)`, 0 tests with all assertions trapped inside an `if`/`if let`. Only 29 tests lack an assertion macro, and 28 of those assert via `#[should_panic]`, `.expect()` on a `tokio::time::timeout`, or `collected.expect_ok()`. All 19 bare `assert!(..is_ok())` sites were read in context; most are paired with negative cases. **Exactly one silent-skip mechanism exists in the whole suite — M5.**

**Failure-path density** (failure/edge-named vs total): storage 60/174 · adapters 20/52 · process 15/32 · orchestrator 71/187 (101/188 error-named by the sub-audit's stricter count) · CLI 45/195. Roughly a third everywhere — not golden-path-only. Names are behavioural sentences, not `test_foo`: `the_lock_is_released_even_when_the_critical_section_fails`, `a_seeding_failure_is_reported_per_agent_and_does_not_stop_the_pass`.

**Anti-vacuity is actively engineered**, which is rarer than merely passing:
- `fs_utils.rs:1258-1260` asserts the *unrepaired* lone-surrogate input really is unparseable — "so the repair is load-bearing rather than belt-and-braces."
- `tests/concurrent_index.rs:52-58` records the silent-green anti-pattern it fixed and now hard-asserts (the convention M5 violates).
- `prompt_golden.rs:52` `assert!(cases.len() >= 80, "only {} fixtures found")` is a fixture-count floor guarding against a glob matching zero — a vacuity guard, not a weak assertion.
- `orch-cli/src/update.rs:87` greps its own module source for `Command::new`/`reqwest`/`TcpStream`, turning a scope cut into an enforced test.

**Edge-case coverage is specific:** lone high/low surrogates and half-pairs repaired (`fs_utils.rs:1243,1264,1289`), TS-written lone surrogate tolerated on read (`:1329`), line cap counting UTF-16 units not bytes (`process/line_reader.rs:215`), invalid UTF-8 replaced per line (`:209`), CJK/emoji titles falling back to the id (`workspace/tests.rs:269`), zero-width terminals (`orch-tui/src/layout.rs:158`), 5000-event endurance (`live_updates.rs:612`), concurrent lock yielding exactly one winner (`lock.rs:456`), interleaved O_APPEND writes from two handles (`fs_utils.rs:1217`), 100 concurrent liveness checks (`process_endurance.rs:188`).

**Best tests in the suite:**
1. `orch-engine/src/message_service.rs:368` — property test over 12×6×16 alignments of a 2-unit char against the cap; asserts cap, prefix, and no mid-char split.
2. `orch-domain/src/scope.rs:607` — differential: `ScopeIndex` vs the pairwise function over all 22×22 corpus pairs, including emoji, absolute paths, bare `"/"` and `""`.
3. `orch-infra/tests/concurrent_index.rs:201` — a real cross-binary race that asserts invariants (index is a subset of files, never invents an entry, removing the index recovers everything) and *records* the lost-update count instead of asserting a flaky number.
4. `orch-infra/src/storage/lock.rs:287,302,321` — dead pid, live-but-recycled pid, touch-keeps-holder: three genuinely distinct hard cases.
5. `orch-cli/tests/routing.rs:62` — 12 guarded commands checked for exit 3 **and** exact stderr bytes; `:95` proves `--help` does not bypass the init guard.

---

## 5. The five conditions

1. **M1** — delete the no-op closure at `orch-tui/src/app.rs:357-366` and its false comment, or implement the timestamp it promises.
2. **M2** — implement `--allow-foreign-project`, or have the architect record in ARCHITECTURE §11 that V32-green supersedes it.
3. **M3** — make `display_width` count UTF-16 code units; add an emoji table-cell test.
4. **M4** — install a `tracing_subscriber` honouring `ORCHESTRY_DEBUG`/`RUST_LOG` per §4.5, or drop the dependency and the 41 call sites.
5. **M5** — convert `cross_compat.rs`'s `Fixture::new()` `None` return to an `assert!` mirroring `concurrent_index.rs:60-83`, and pin the oracle's presence in whatever runs the suite.

Everything in §2's minor table is a cleanup, not a condition.

---

## 6. Questions for the author

1. `tick.rs:235-258` `reconcile_stale_agents` uses `collect::<Result<Vec<_>>>()?`, so one agent's `set_status` failure aborts the whole reconcile — while `reconcile_orphaned_tasks` immediately below is deliberately per-item non-fatal. Is the asymmetry TS-faithful (`Promise.all` rejecting), or an oversight?
2. **M1:** was `PendingMessage` meant to carry a history timestamp, or is stamping `now` the TS behaviour and only the comment wrong? That decides delete-vs-implement.
3. **M3:** has an astral-character table cell ever gone through the differential harness? If the harness fixtures are all ASCII, this divergence is invisible to it by construction.
4. **M4:** is the absent subscriber init intentional (diagnostics deferred), or an oversight?
5. **m1:** `which` is named in §4.5 for the `cursor-agent → agent` fallback, but the adapter hard-codes the list — fix the doc or the probe?
6. **m2:** is the absent fsync a deliberate parity call, or not yet considered?

---

## 7. Scope caveat

I did **not** run the differential parity harness (task #4 owns it) and did not execute `xtask/probe-harness.sh` per instruction (it mutates sources). This verdict covers code quality, library-first compliance, and the in-tree test suite — **not** parity certification. M3 and M5 both bear on parity but were found by reading code and test guards, not by running the oracle; M3 in particular should be confirmed against the harness before or alongside the fix.
