# Gate re-review — code quality (Modern + Robust)

**Reviewer:** code-quality gate (read-only; did not write this code)
**Candidate:** commit `ed05007` (final release candidate), worktree `../r-orch-wt/gate-review`
**Prior review:** [gate-code-review.md](gate-code-review.md) at `0b67e4d` — PASS WITH CONDITIONS (5)
**Date:** 2026-10-14

---

## Verdict: **PASS** — unconditional

All five conditions (M1–M5) are met, and all ten minor findings are closed. Three of the five were resolved more thoroughly than the condition required, and two fixes went beyond what I asked for. I found **no new findings** and no regressions. Nothing is outstanding.

**Baseline re-verified independently on `ed05007`** (not taken on report):

| Check | Result |
|---|---|
| `cargo clippy --workspace --all-targets -- -D warnings` | **exit 0, zero warnings** |
| `cargo test --workspace --no-fail-fast` | **2581 passed, 0 failed, 0 ignored**, 53 binaries |

Both match the lead's reported figures exactly (2581 tests, +36 since `0b67e4d`). Worktree clean, no uncommitted changes.

---

## 1. Conditions

### M1 — TUI history timestamps · **MET**

`PendingMessage` gained `ts_ms: Option<i64>` (`orch-tui/src/messages/mod.rs:278`). `app.rs:286` parses each entry's timestamp **once** (`let ts_ms = parse_timestamp_ms(&entry.timestamp);`) and threads it into all six `PendingMessage` variants (`:304, :314, :325, :334, :346, :362`). `state.rs:853,882` consume it as `p.ts_ms.unwrap_or(now_ms)`.

The dead no-op closure is gone — `ingest_history` now ends `messages.push(pending)` with no `.map()` — and the comment is accurate: *"`now` only stamps entries whose own timestamp did not parse."* The behaviour now matches what the old comment falsely claimed.

**Tested, and non-vacuously.** `orch-tui/tests/live_updates.rs:742-745` asserts `message.ts_ms != 0` **and** `assert_ne!(message.ts_ms, harness.now())` — the second assertion is the one that matters, because it proves the preserved history timestamp is *not* the ingest clock, which is precisely the bug. `:772,:782` assert that live (non-history) messages *do* get `now`. Both branches pinned.

**Bonus fix beyond the condition.** The `tool_call` branch previously rendered `format!("⚙ {}()", entry.data)` — interpolating the entire JSON payload. It is now `tool_call_name(&entry.data)` (`app.rs:340`) implementing TS `⚙ ${d?.name ?? 'tool'}()`, with a comment noting the payload is an object for adapters that record one and a bare name for those that do not. I did not flag this; they found it while in the area. It was a real output defect.

### M2 — `--allow-foreign-project` · **MET** (retired, with better reasoning than my condition asked for)

My condition offered implement-or-document. They chose the second and made a stronger argument than "V32 is green": **a default-on refusal would contradict the product's central promise.** `ARCHITECTURE.md:1308-1325` and `:66-69` now record the valve as *"retired, not deferred. It was never built, and it should not be."* The reasoning:

- PARITY §1.1 and the README promise both binaries operate on the same project interchangeably. A flag refusing to write to a `.orchestry/` r-orch did not create would make the common case — pointing r-orch at a project the npm package created — fail by default. **The valve would break the thing it was meant to protect.**
- V7 (`ts_then_rust`/`rust_then_ts`/interleaved), V8 and V32 exercise the real failure mode on every commit, rather than asking the user to opt into safety at the point of use.
- The retirement is made **explicitly conditional on M5** and on nothing else: *"`cross_compat.rs` passed green when the oracle was absent (code M5), which would have let the guarantee lapse silently… The valve's retirement depends on that."*

I accept this. It is the right call, and I was wrong to frame implementation as the default option — I had treated the valve as a free safety net without weighing it against the interchangeability guarantee it sits on top of. The residual hazard is documented rather than flagged: `docs/RUNBOOK.md:109` *"Rebuilding an index after a lost update"* exists, and both doc cross-references resolve (I checked the anchor, not just the link).

Because this condition is load-bearing on M5, I verified M5 independently rather than taking it on report — see below.

### M3 — L0 table padding on astral characters · **MET**

A single `utf16_len` helper (`orch-cli/src/output.rs:266-268`):

```rust
fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}
```

Applied at every site that previously counted `char`s: `display_width:282`, `pad_end:272`, the column-width max `:213`, and the key/value renderer `:250`. The misleading doc comment is gone. Unit tests at `:526-528` pin `utf16_len("ship 🚀") == 7` and `utf16_len("○ todo") == 6` — the second guards the other direction, that a BMP symbol still counts 1.

**The harness gap I asked about in my question 3 is closed.** `fixtures/scenarios/cmd-astral-column-widths.toml` is a new differential scenario, and it is better designed than my condition required. It discriminates **three** implementations, not two: a rocket (U+1F680, 2 UTF-16 units, 2 terminal columns) catches a `char`-counting implementation, and a ZWJ family (U+1F468 ZWJ U+1F469 ZWJ U+1F467, **8** units, 2 columns) additionally catches a *display-width*-counting one, which would pad six short. Each table carries one astral cell beside a plain one so the padding of the whole column records the count. Coverage spans `task list/show`, `agent list`, `goal list` and `context list`, in both human and `--json` form. It even notes that a context *key* cannot carry astral characters (`sanitizeId` is `[A-Za-z0-9._-]`), so the value column has to be the widest cell to drive the column — a detail that would have made a naive version of this scenario vacuous.

The scenario's own description states the failure mode plainly: *"both look right on every ASCII fixture and wrong the first time anyone names a task with an emoji."* That was exactly the concern.

### M4 — tracing subscriber · **MET**

`init_tracing()` (`orch-cli/src/lib.rs:712-731`), called at `lib.rs:48`. It satisfies ARCHITECTURE §4.5 on all three counts, and the doc comment ranks them by importance:

1. **stderr, never stdout** (`.with_writer(std::io::stderr)`) — because stdout is the byte-compared contract and `--json` must stay parseable.
2. **Silent by default** — the filter is `EnvFilter::new("off")` absent configuration, *"installed but records nothing, which costs a filter check per call site and nothing else"*, so no invocation's output changes.
3. `RUST_LOG` takes a full `EnvFilter`; `ORCHESTRY_DEBUG` turns on `debug` without one.

`try_init()` rather than `init()`, with the reason given: a test binary may install its own and a second attempt must not abort the process over logging. The `let _ =` on it is correct and justified.

**Tested, and the test refuses to be vacuous.** `orch-cli/tests/tracing_output.rs:41` asserts `stdout_is_byte_identical_whether_or_not_tracing_is_on`. The companion test compares the same command in the same tree with one environment variable changed, and guards its own premise: *"ORCHESTRY_DEBUG logged nothing, so the silence above proves nothing"* — i.e. it fails if the positive case produces no output, so the default-silent assertion cannot pass trivially. That is the right shape for this test and I would not have thought to demand it.

### M5 — `cross_compat.rs` silent skip · **MET**

`crates/orch-infra/tests/cross_compat.rs:52-80`. The `None`-on-missing-oracle return is replaced by a hard `assert!` naming the fix command, with `ORCH_SKIP_CROSS_BINARY` as the only opt-out — mirroring `concurrent_index.rs` exactly, as the condition asked. The comment records the reasoning and cites the finding code:

> "A missing oracle used to `return None` with an `eprintln`, which cargo hides for a passing test — so this whole file reported green while asserting nothing, and the cross-compat crux (§6.4) is exactly the thing that cannot afford to be quietly skipped. Mirrors `concurrent_index.rs`: fail loudly, with `ORCH_SKIP_CROSS_BINARY` as the only way to skip (code M5)."

`assert_hermetic` is still called on the fixture path. The V7/V8/V32 guarantee can no longer lapse silently, which is the precondition M2's retirement rests on. Both halves of that dependency now hold.

---

## 2. Minor findings — all ten closed

| # | Finding | Status | Evidence |
|---|---|---|---|
| m1 | Four unused member deps | **MET** | `which` and `predicates` removed from every manifest (0 refs, 0 declarations). `shell-words` now declared **only** in `xtask/Cargo.toml`, where it has 6 real uses; the dead `orch-infra` declaration is gone. `tracing-subscriber` is now declared *and* used (M4). |
| m2 | `atomic_write` no fsync | **MET, exceeded** | `fs_utils.rs:96` `file.sync_all().await?` before the rename, with the reason — *"Without this the rename can be durable while the contents are…"*. They also added a **directory** fsync after the rename (`:119-125`), noting the entry is only durable once the directory's own metadata is, correctly best-effort (`let _ =`). I had filed this as informational; they closed it properly, including the half I did not raise. |
| m3 | `atomic_write` error branches untested | **MET** | Two new tests: `a_failed_write_leaves_no_temp_file_and_no_destination` (`:1413`) and `a_failed_rename_leaves_no_temp_file` (`:1440`), the latter forcing a genuine rename failure by making the destination a non-empty directory. Both error branches I had verified by reading only are now executed. |
| m4 | Weak OR assertion in `find_project_root` test | **MET** | Previously flagged at `paths.rs:424`; the two-outcome OR is gone. |
| m5 | Test wrote outside its tempdir | **MET** | `skills.rs:279-281`: the decoy now goes in `dir.path().join("outside")` — a sibling **inside** the tempdir — and uses `.unwrap()` instead of `.ok()`, so setup failure can no longer make the `!out.contains("secret")` half vacuous. Comment cites code m5 and explains that `dir/..` was `/tmp`. |
| m6 | Vacuous clipboard test | **MET** | `clipboard.rs:262+` renamed to `the_system_clipboard_degrades_rather_than_erroring_when_headless` and now asserts the degradation contract. Comment: *"This used to bind every result to `_`, which asserts nothing at all (code m6)."* Both outcomes are named rather than one pinned, so it is honest on a developer desktop as well as headless CI. |
| m7 | Four `is_ok()`-only tests | **MET** | `goal_service.rs` now asserts effects: stored title, `saved().len()`, emitted event type, assignee. No bare `assert!(…is_ok())` remains in the file. |
| m8 | `ORCH_BLESS_FIXTURES` disarmed 13 tests | **MET, exceeded** | `adapter_fixtures.rs:101-125` now writes the fixture and then **panics**, so a blessing run cannot report green: *"The one thing a fixture suite must never do is pass without comparing."* It also notes each case is its own test, so all fixtures still regenerate in one run — they simply all fail. Stronger than the `return`-removal I suggested. |
| m9 | Loops vacuous on an empty skill library | **MET** | `shop_skill_references.rs:37` `assert_eq!(available_skills().len(), 26);` — a hard count floor in a sibling test, so no loop over `available_skills()` can be vacuous. |
| m10 | `let _ = name;` unused-param suppression | **MET** | `format.rs:127` is now `pub fn format_tool_input(_name: &str, input: &Value)`; the `let _ = name;` line is gone and the signature-parity comment is retained. |
| m11 | Stale manifest comments | **MET** | The "None is used yet" claim and the `semver`/update-check description are both gone from the root `Cargo.toml`. |

---

## 3. Assessment

Every fix addresses the underlying cause rather than the symptom I reported. Four are worth calling out as better than the condition:

- **M2** rejected my framing instead of complying with it, and was right to. A reviewer's suggested remedy is not privileged over the design argument against it, and the response engaged the argument rather than deferring to the finding.
- **M3**'s scenario discriminates three implementations where I had only asked it to catch one.
- **m2** closed the directory-fsync half of the durability gap, which I had not identified.
- **m8** converted a latent disarm into a hard failure rather than merely removing the early return.

The pattern I noted in the first review — that anti-vacuity is actively engineered here, not incidental — held under re-review and extended to the new tests. Four of the fixes (M1, M4, m6, m8) ship with a test or comment whose explicit purpose is to prevent the fix from passing trivially, and three comments cite their finding code (`m5`, `m6`, `m8`, `M5`) so the reasoning survives the next refactor. The `ORCHESTRY_DEBUG` test guarding its own premise and the `assert_ne!(ts_ms, now())` assertion are both the kind of thing that only gets written by someone thinking about how the test could lie.

No regressions: clippy stayed at zero warnings, the suite grew by 36 tests with none ignored, and the properties I verified clean at `0b67e4d` — zero `unsafe`, zero production `panic!`/`todo!`/`unimplemented!`, the fused state mutex with `&mut OrchestratorState` phases, the unconditional YAML guard, structural stderr draining, the PID `0`/`> i32::MAX` guard — are untouched.

---

## 4. Scope caveat

I verified clippy and the full `cargo test` suite myself on `ed05007`. I did **not** execute the differential parity harness (58/58), the cold gate (12/12) or the probe harness (11/11) — task #4 owns those, and `xtask/probe-harness.sh` mutates sources. For M3 I verified the scenario **definition** is present and well-constructed (`fixtures/scenarios/cmd-astral-column-widths.toml`) and that `utf16_len` is applied at every padding site with unit tests, but the 58/58 harness result is taken on report, not independently reproduced. This verdict covers code quality, library-first compliance and the in-tree test suite — not parity certification.

**Final verdict: PASS. All five conditions met, all ten minors closed, no new findings, no regressions. No conditions attached.**
