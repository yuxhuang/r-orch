# Gate security re-review — r-orch

**Reviewer:** security gate (read-only)
**Candidate:** `../r-orch-wt/gate-review` @ **`ed05007`**
**Prior review:** `docs/reviews/gate-security-review.md` (@ `0b67e4d`) — conditions C0–C5
**Date:** 2026-10-14

---

## Verdict — Round 2 (`f5d5abf`, branch `c0-exact-count`): **PASS**

> **All conditions met. C0 is closed, and the deviation (removing the deadline) is accepted on
> evidence.** Details in §11, which supersedes the Round 1 verdict below.
>
> Headline: every bomb I could construct is now refused in **21–28 ms** with exit 1, including the
> `shapeD` shape that caused the block. The fix also closed a **second DoS neither of us had
> found** — 300 KB of non-alias nesting took `ed05007` **58 seconds**; it now takes **23 ms**.
> Worst legitimate 1,033,805-byte config parses in **0.099 s**.

---

## Verdict — Round 1 (`ed05007`): BLOCK *(superseded by §11)*

> **BLOCK — on C0 alone.**
>
> C1, C2, C3, C4 and C5 are **met**, several of them better than I specified. **C0 is not met.**
> The three pre-parse caps kill both attack shapes from the first review outright (5 min 13 s →
> **30 ms**, a genuine fix). But the guard set is bypassed by a third shape I constructed against
> it, and the `spawn_blocking` deadline — the designed backstop — reports the problem without
> containing it. Measured: an **84 KB** `.orchestry/config.yml` that passes all three caps prints
> the correct timeout error at 2 s, then **continues at 100 % CPU allocating ~0.55 GiB/s and never
> exits** (19.3 GiB at 28 s; 180 GiB at 3 min 14 s, still climbing, when I let one run).
>
> On a 16–32 GB developer machine that is an OOM in well under a minute, and the OOM killer's
> victim is not necessarily `orch`. The 1 MiB document cap does **not** bound this, because the
> cost scales with the *expanded* size, not the file length.

This is one defect with a cheap fix, not a broad failure. I am blocking rather than conditioning
because the first review's remediation listed alias-**nesting** detection as one of three required
parts, the implementation shipped the other two, and the omitted part is precisely the bypass.
Re-run the Shape D probe below and this clears.

---

## Condition-by-condition

| # | Condition | Status | Evidence |
|---|---|---|---|
| **C0** | YAML DoS guards | **NOT MET** | §1 |
| C1 | lock pid guard | **MET** | §2 |
| C2 | run-events read guard | **MET** | §3 |
| C3 | `.`/`..` in extension-less paths | **MET** | §4 |
| C4 | token `saturating_add` | **MET** | §5 |
| C5 | cargo-deny in-tree | **MET** (one residual) | §6 |
| — | temp-file `create_new` | **MET** | §7 |
| — | 0600 on `config.yml` / `agents/*.yml` | **PARTIALLY MET** | §7 |

---

## 1. C0 — NOT MET

### What was built, and what it fixes

`crates/orch-infra/src/storage/fs_utils.rs:402-470` now runs three checks *before* the parser sees
the document — size (`MAX_YAML_READ_SIZE` 1 MiB), expansion estimate
(`len × (1 + aliases) ≤ MAX_YAML_EXPANDED_SIZE` 8 MiB), alias count (`MAX_YAML_ALIASES` 64) — then
parses in `spawn_blocking` under a 2 s `tokio::time::timeout`. The constants carry genuinely good
documentation, including the measured observation that **a large *collection* anchor is the
amplifier** — the exact reconciliation that cost the first review a round trip.

Both of my original attack shapes are now rejected, and rejected *fast*. Release build,
`orch status`, same files as the first review:

| shape | bytes | before | **after** | verdict |
|---|---|---|---|---|
| A (nested, sequence leaf, W=1200) | 46,854 | 5 min 13 s | **0.030 s** | rejected: "could expand to ~268 MiB" |
| B (flat wide anchor, 2000 aliases) | 322,901 | 35.16 s | **0.025 s** | rejected: "could expand to ~616 MiB" |

That is a ~10,000× improvement on the reported attack and the error messages are clear and
actionable. Credit where due.

### The bypass

The `len × (1 + aliases)` estimate assumes each alias expands to at most the whole document. For
**nested** aliases that is wildly wrong: expansion is exponential in depth while both factors stay
small. The code comments acknowledge this ("chiefly *nested* aliases, where the product of length
and count stays small but expansion is exponential") and delegate the defence to two things — the
alias cap and `serde_yaml_ng`'s own repetition limit. Neither holds:

- **Doubling needs only 2 aliases per level.** 30 levels = 60 aliases, under the cap of 64, for
  2³⁰ expansion.
- With 60 aliases the estimate budget permits a document up to 8 MiB / 61 ≈ **137 KB**, so the
  leaf can be made large — and leaf size is the per-jump deep-copy cost.

*Shape C* — minimal probe, 3,461 bytes, 60 aliases, 200-element leaf: slips all three caps, and
`serde_yaml_ng`'s repetition limit does catch it, in 0.66 s. That is the case the comments
anticipated.

*Shape D* — the same structure with the leaf enlarged to stay just inside the estimate:

```
shapeD_6000_30.yml   84,661 bytes   60 aliases   estimate 4.93 MiB   -> PASSES all three caps
```

```yaml
b0: &b0 ["yyyyyyyyyy", ... 6000 times]   # large leaf = per-jump cost
b1: &b1 [*b0, *b0]                        # 2 aliases per level...
b2: &b2 [*b1, *b1]
...
b30: &b30 [*b29, *b29]                    # ...x30 levels = 60 aliases, 2^30 expansion
```

Measured, release build, `orch status`, sampled from `/proc/<pid>/status`:

| t | RSS | CPU |
|---|---|---|
| 2 s | 4.56 GiB | 101 % |
| 6 s | 8.14 GiB | 101 % |
| 12 s | 11.90 GiB | 100 % |
| 20 s | 15.57 GiB | 100 % |
| 28 s | 19.30 GiB | 100 % |

And in an earlier unbounded run: **180 GiB RSS at 3 min 14 s, still growing**, killed manually.

The 2 s deadline **does** fire — stdout carries the right message:

```
✕ parsing .../config.yml exceeded 2s: a YAML document whose aliases expand to far
  more than its own size. Nothing ORCH writes uses aliases at all.
```

**But the process does not exit.** Two reasons compound:

1. `spawn_blocking` work cannot be cancelled — the comment at `fs_utils.rs:311` says so.
2. Tokio's runtime waits for outstanding blocking tasks at shutdown, so `main` returning after the
   error cannot terminate the process either.

So the deadline converts a *slow failure that terminated* into a *fast error message followed by an
unbounded, non-terminating allocation loop*. In the one dimension that matters for a DoS — does
the machine recover on its own — the mitigated build is **worse** than `0b67e4d`, which at least
finished in 5 minutes and exited. The comment's claim that this is "bounded, because the document
is already capped at 1 MiB" is the specific thing that is not true: the bound that matters is on
expansion, not on length.

### Remediation

The simplest complete fix is already written in the code's own error strings: **"Nothing ORCH
writes uses aliases at all."** If that is true — and I verified it is; a fresh `orch init` emits
`config.yml` and `agt_creator.yml` with zero aliases — then:

1. **Set `MAX_YAML_ALIASES` to 0, or 1.** The entire amplification class disappears, no nesting
   detection needed, no estimate heuristic to tune. The only cost is rejecting hand-authored YAML
   that uses aliases, which is a documented divergence you are already taking, and the error
   message for it is already written. **This alone closes C0.**
2. If a nonzero alias budget must be kept for hand-authored config, then detect **alias nesting**
   (an alias occurring inside an anchored collection that is itself aliased) in the existing
   single-pass scan — the third clause of the first review's remediation, and the one omitted.
3. Independently, and regardless of 1 or 2: the deadline needs to bound *something*. Either parse
   in a **killable child process**, or bound the allocator for that work, or accept that the
   timeout is a diagnostic rather than a control and say so in the comment. A backstop that cannot
   stop anything should not be documented as the thing that makes the other caps safe.

Verify any fix against **`shapeD_6000_30`**, not against Shape A/B or a scalar-leaf bomb — all
three pass a build that is still vulnerable.

---

## 2. C1 — MET

`crates/orch-infra/src/storage/lock.rs:205-225` now routes through the shared guard:

```rust
let Some(target) = crate::process::manager::signal_target(pid) else { return false; };
```

The `pid as i32` cast is gone. The comment states the hazard (negative wrap addresses a process
group; pid 0 is the caller's own group), states that treating both as dead is a deliberate narrow
divergence from `process.kill(0, 0)`, and flags that it needs a §14 entry. That is exactly the
disposition asked for.

## 3. C2 — MET

`crates/orch-infra/src/storage/run.rs:221-246` stats the file first and reads a bounded tail when
`size > MAX_EVENTS_READ_SIZE`, with a `tracing::warn!`. The comment correctly explains why it
cannot delegate to `read_jsonl` (whose `lines()` strips `\r`, violating §8.5) and so applies the
same threshold locally. Memory is no longer a function of attacker-chosen file size.

## 4. C3 — MET

`sanitize_path_segment` (`paths.rs:187`) wraps `sanitize_id` and rejects exactly `.` and `..`, and
it is wired into **both** extension-less builders: `paths.rs:142` (`task_attachments_dir`) and
`workspace/manager.rs:53` (`workspace_path`, the one feeding `remove_dir_all`). `sanitize_id`
itself is unchanged, so the L0 character class and its error string are byte-compatible — the
additive approach I asked for. Minor, non-blocking: the helper is duplicated in both files rather
than shared.

## 5. C4 — MET

`crates/orch-domain/src/run.rs:114` — `input.saturating_add(output).saturating_add(reasoning)`.
The child-supplied `u64::MAX` panic is gone.

## 6. C5 — MET, with one residual

`deny.toml` (4,418 bytes) exists, with `unmaintained = "all"` scope and two documented
per-advisory ignores:

- `RUSTSEC-2024-0449` — `unsafe-libyaml` via `serde_yaml_ng`, reasoned as "no API-compatible
  replacement, DoS bounded by `read_yaml_value`'s caps"
- `RUSTSEC-2026-0319` — `anymap2` via `liquid-core`, "no alternative exists, and input is
  first-party template text"

Both are per-ID, so a *new* advisory against either crate still fires. Correct design. I ran it
independently: `cargo-deny --manifest-path Cargo.toml check advisories licenses bans sources` →
exit **0**, `advisories ok, bans ok, licenses ok, sources ok`, with only four `warning[duplicate]`
lines (`getrandom`, `hashbrown`, `miniz_oxide`, `syn`) — the same benign duplicates I characterised
in the first review. The step is wired into `xtask/gate.sh:323` ahead of the parity job.

**One correction to the risk acceptance:** the stated justification for ignoring
`RUSTSEC-2024-0449` is that the DoS is "bounded by `read_yaml_value`'s caps". Per §1 it is not —
so that acceptance currently rests on a premise that does not hold. The acceptance is still
defensible on the other grounds (no API-compatible replacement), but the reason string should be
corrected when C0 is fixed, not left asserting a bound that the probe disproves.

**Residual (not blocking):** `advisories_check` returns 0 with a "skipping" line when cargo-deny is
absent, and there is still no `.github/`. So the control fails **open** on any machine without the
tool, and nothing enforces it outside a developer's local gate. The in-file comment argues a gate
that cannot run on a fresh clone teaches people to bypass it — reasonable — but the team has been
fixing exactly this vacuous-green class elsewhere (per the dissent log), so this one deserves the
same treatment once CI exists.

## 7. Temp-file `create_new` — MET · 0600 — PARTIALLY MET

**`create_new`: met.** `atomic_write_with_mode` (`fs_utils.rs:68-86`) opens the temp file with
`options.write(true).create_new(true)` and an optional `.mode(mode)`, with the comment "`create_new`
so the open fails rather than following a symlink". The symlink-on-temp-file gap (first review R2)
is closed, and there is a regression test at `fs_utils.rs:1457`.

**0600: partially met.** `OWNER_ONLY = 0o600` exists (`fs_utils.rs:38`) and the *store* write paths
apply it. But `orch init` does not. Verified end-to-end on a fresh project (my umask 002):

```
after `orch init`:
  664  .orchestry/config.yml                <- not owner-only
  664  .orchestry/agents/agt_creator.yml    <- not owner-only
after `orch config set ...`:
  600  .orchestry/config.yml                <- store path is correct
after `orch agent add probe --adapter claude`:
  600  .orchestry/agents/agt_XjyXEu2.yml    <- store path is correct
```

So every file written through `ConfigStore`/`AgentStore` is 0600, and the two files `init` creates
are 0666 & ~umask. `config.yml` self-corrects on first write; **`agt_creator.yml` plausibly never
gets rewritten and stays group/world-readable for the life of the project.** Fix: route `init`'s
two writes through `atomic_write_with_mode(..., Some(OWNER_ONLY))`. Low severity — see the
correction below — but it is a one-line gap in a condition reported as done, and a unit test
asserting 0600 on the store path (`config.rs:116`, `agent.rs:98`) is what let it hide.

Also unchanged: `.orchestry/` and its subdirectories are **0775**, not the 0700 I recommended
(R1). Since 775 is group-writable, a member of the owning group can create, replace or delete
entries in `.orchestry/agents/`. On a single-user machine this is nothing; on a shared host with a
shared primary group it matters more than the file modes do. R1 was a recommendation, not a
condition, so this is "not taken", not "not met".

### Correction to my own first review

My first review's R1 claimed agent secrets "land in a public repo on the next commit" because
`init`'s `.orchestry/.gitignore` does not list `agents/` or `config.yml`. **That was wrong.** I
looked only at the inner ignore file. `init` also appends `.orchestry` to the *root* `.gitignore`,
which excludes the whole tree — confirmed:

```
$ git check-ignore -v .orchestry/agents/agt_creator.yml
.gitignore:2:.orchestry    .orchestry/agents/agt_creator.yml
```

So the git-exposure half of R1 never existed, and the inner ignore file is belt-and-braces for a
force-add. Only the local-permissions half was real, and that is what §7 covers. The severity I
assigned R1 (Medium) was too high on the strength of a claim I had not checked end-to-end.

---

## 8. Re-checked clean

Spot-checked that the C0–C4 changes did not regress what passed the first review:

- `sanitize_id`'s character class and `Invalid identifier: "<id>"` message unchanged (`paths.rs:164`),
  so the L0 surface is intact while C3's guard sits above it.
- `signal_target` is now `pub(crate)`-reachable from the storage module and still rejects `0` and
  `> i32::MAX`; the `ProcessManager` call sites are unchanged.
- `apply_merge()` is still unconditional after the parse, now on the deadline's result path.
- The three new YAML rejections return through the ordinary `Error::other` boundary → exit 1 with a
  printed message, not a panic; confirmed by the live runs in §1.
- `cargo-deny` `bans`/`sources` clean means no new git, vendored or alternate-registry dependency
  entered with the fix.

## 9. Scope and method

Read-only review of `ed05007` plus five live probes (`orch status`, `orch init`, `orch config set`,
`orch agent add`, `cargo-deny`) against a release build I compiled in the shared target dir
(`cargo build --release --bin orch`, 16.5 s). Probe projects and bomb files were created under
`$TMPDIR/rr/` — outside both repos — and deleted afterwards.

RSS figures come from `/proc/<pid>/status` `VmRSS` sampled at 4 s intervals in a bounded run I
killed at 28 s; the 180 GiB/3 min 14 s figure is from one unbounded run which I killed manually on
noticing the growth, to avoid pressuring a machine shared with the other gate reviewers. Single
runs, so treat the absolute seconds as indicative — the 10,000× improvement on Shapes A/B and the
unbounded growth on Shape D are both far outside any noise band.

No file in the candidate worktree was modified; no commits; no state-changing git commands;
`xtask/probe-harness.sh` not run. This report is the only file written.

---

## 10. Bottom line

Five of six conditions met, two of them (C1's documentation, C5's per-ID ignores) better than
specified, and the headline attack is 10,000× faster to reject. C0's remaining gap is a single
omitted clause whose fix is plausibly one constant: **`MAX_YAML_ALIASES = 0`**, justified by the
code's own repeated assertion that nothing ORCH writes uses aliases.

Fix C0, correct the `RUSTSEC-2024-0449` reason string, and route `init`'s two writes through
`OWNER_ONLY`. I will re-run `shapeD_6000_30` and clear this on the next pass.

---

# 11. Round 2 — verification of the C0 fix (`f5d5abf`)

**Candidate as tested:** branch `c0-exact-count` @ **`be7df28`** (worktree
`../r-orch-wt/gfix-storage`), off master `c88d3ed`.
**Verdict: PASS — all conditions met.** This supersedes the Round 1 BLOCK.

> **Commit note.** I was asked to verify `f5d5abf`; by the time I read the worktree, wp0 had
> **amended** it to `be7df28` (same subject, same parent `c88d3ed`, not an ancestor —
> `git merge-base --is-ancestor f5d5abf HEAD` is false). I verified `be7df28`. The delta is
> `git diff f5d5abf be7df28` = **+21 / −2 lines in `fs_utils.rs`, all comments** — specifically the
> "sole load-bearing defence" warning the lead required as a condition of accepting the deadline
> removal. **No logic differs**, so these results apply to both commits, and the required comment is
> satisfied by the amendment itself. Whatever lands on master should be checked to be this tree:
> `git diff be7df28 <master> -- crates/orch-infra/src/storage/fs_utils.rs` should be empty.

## 11.1 Ruling on the deviation (deadline removed, no boundary-exit backstop)

wp0 removed the 2 s deadline instead of building the lock-safe boundary exit I asked for. In Round 1
I named the condition that would change my mind: *"evidence that the deadline cannot fire while the
lock is held or the TUI is up — e.g. if the exact pre-pass makes the deadline formally unreachable
and you delete it rather than keeping it as a guard."* **That condition is satisfied. I accept the
deviation.**

The reason is that the three independent ways to make a YAML parse expensive are each bounded by a
*different* mechanism, so there is no residual class for a clock to catch:

| Cost source | Bound | Measured |
|---|---|---|
| Alias expansion | exact memoised node count, 200,000 budget | every bomb refused in 21–28 ms |
| Nesting depth | `saphyr-parser`'s own recursion limit | 150,000-deep refused in 23 ms |
| Scalar/key size | 1 MiB document cap | 1 MiB scalar parses in 0.092 s |

Two further structural points, both of which I verified rather than took on trust:

- **The u64 overflow trap is unreachable, not merely handled.** With the alias cap at 64, the
  maximum achievable expansion is ~4.3 × 10⁹ (64 aliases arranged as 2-per-level × 32 levels), which
  is nine orders of magnitude below `u64::MAX`. My overflow probe (3× fan-out × 45 levels, 3⁴⁵ ≈
  2.9 × 10²¹) was rejected by the **alias cap at 135 aliases** before the arithmetic mattered. The
  `checked_add` calls are correct belt-and-braces; the cap is what makes overflow impossible.
- **The pre-pass is iterative, not recursive** — one frame per open collection in a `Vec`
  (`fs_utils.rs:353`), so the pre-pass cannot itself be stack-overflowed by deep nesting. This was
  my main worry about making it the sole defence.

The margin is also large in the direction that matters for a *false* positive: the worst legitimate
file (1,033,805 bytes, no aliases) parses in 0.099 s, two orders of magnitude inside any deadline
anyone would have set.

**And the premise of my Round 1 dissent turned out not to apply at all.** I planted a bomb in a task
file and ran `serve --once`: the orchestrator logs `orchestrator:error` with the budget message and
**keeps ticking** — the failure is an ordinary per-tick error, not a process-fatal one. There is no
hard exit from `orch-infra` to strand anything. On SIGTERM it wound down cleanly and the lock was
gone afterwards (verified live, and by `refusing_a_bomb_leaves_no_lock_behind`).

## 11.2 Measurements

Release build of `f5d5abf`, compiled into an isolated target dir, `orch status` end-to-end, bomb as
`.orchestry/config.yml` unless noted.

| probe | bytes | rc | elapsed | outcome |
|---|---|---|---|---|
| **shapeD_6000_30** (the Round 1 bypass) | 84,661 | 1 | **0.026 s** | "expands to at least … nodes" |
| shapeA (nested, seq leaf, W=1200) | 46,854 | 1 | 0.024 s | refused, 6000 aliases |
| shapeB (flat anchor ×2000) | 322,901 | 1 | 0.021 s | refused, 2000 aliases |
| overflow probe (3⁴⁵) | 1,269 | 1 | 0.025 s | refused, 135 aliases |
| merge-key bomb (`<<: *base` ×60) | 37,803 | 1 | 0.025 s | "expands to at least …" — merge keys counted |
| deep non-alias nesting (150,000 deep) | 300,008 | 1 | **0.023 s** | "recursion limit exceeded" |
| 1 MiB single scalar | 1,000,009 | 0 | 0.092 s | parses |
| 900 KB single key | 900,006 | 1 | 0.030 s | clean "not valid YAML" |
| bomb planted in `tasks/<id>.yml` | 84,661 | 1 | 0.028 s | refused via `task show` |
| bomb planted in `agents/agt_creator.yml` | 84,661 | 1 | 0.028 s | refused via `agent list` |

**The second DoS, found by comparing against the old binary.** Deep non-alias nesting on `ed05007`:
**58.143 s**, exit 1. On `f5d5abf`: **0.023 s**, exit 1. Neither of us had found this; the lead
hypothesised it as something worth probing and it was real. Note the shape of the old failure — it
returned exit 1 *after 58 s*, i.e. the 2 s deadline fired and the process still could not exit until
the parse finished. That is the containment failure of Round 1, reproduced independently on an input
with no aliases in it at all.

**Positives — all `rc=0`:** `init`, `status`, `task add`, `agent add`, `config set`, `task list`,
`agent list`, `doctor`; a hand-authored merge-key `config.yml` using `&defaults` + `<<:` (the js-yaml
parity case the alias budget exists to preserve); and the 1,033,805-byte legitimate config in 0.099 s.

**wp0's own suite:** `cargo test --release -p orch-cli --test yaml_bomb` → **4 passed**, 0 failed, in
0.09 s. It covers config, task and agent plants, asserts process exit and a peak-RSS ceiling via
`run_sampling_rss`, and asserts lock removal — the RSS ceiling is the check I asked for in Round 1
so that a slow-wind-down regression cannot pass on a large CI box and OOM on a laptop.

## 11.3 Design review of the pre-pass

`expanded_node_count` (`crates/orch-infra/src/storage/fs_utils.rs:338`) walks `saphyr-parser`
events, never resolving an alias, memoising expanded counts in a `HashMap<usize, u64>` keyed by the
parser's **numeric anchor id** rather than by name — which is what makes anchor redefinition safe,
since a redefinition gets a fresh id (trap 4). An `Alias` whose id is absent from the map means the
anchor is not yet closed, i.e. a cycle, and is refused (`fs_utils.rs:414-421`). Merge keys arrive as
ordinary `Alias` events and so carry their full expanded cost (trap 3) — confirmed empirically by
the merge-key bomb. All five traps from Round 1 are addressed, four structurally.

## 11.4 Remaining condition items — all met

- **C5 deny.toml reason: corrected.** Now reads "…no API-compatible replacement (serde-saphyr has no
  `Value` model), and the amplification class is closed by the exact expanded-node pre-pass in
  `read_yaml_value`" (`deny.toml:54`). That is accurate as of this commit, and it names the actual
  blocker to replacement rather than asserting a bound that did not hold.
- **0600 on `config.yml` / `agents/*.yml`: met.** Fresh `orch init` now yields `600` for both
  (was `664`). Verified on a real project, umask 002.
- **"Sole load-bearing defence" comment: present** (`fs_utils.rs:281` and `:303`, "This is the sole
  DoS defence for YAML reads, and it is load-bearing alone").

## 11.5 New observations (none blocking, none a regression)

1. **`tasks/*.yml` is written 0664**, while `config.yml` and `agents/*.yml` are now 0600. Task files
   hold descriptions and prompts, not credentials, so the exposure is materially lower — but the
   inconsistency will read as an oversight later. Suggest routing the task store through
   `OWNER_ONLY` too, or recording why task files are deliberately different.
2. **A corrupt/planted task file makes `serve --once` never terminate.** The task can never be read,
   so it stays `queued` and `--once` never reaches "all done"; it held the lock for the full 120 s of
   my run and only ended on SIGTERM. It then reported `serve:finished result="all_done"
   exit_code=0`, which is the wrong result string for "I was killed while stuck". Same family as the
   known open resilience item (c). **Not a regression** — pre-fix this was worse (seconds of CPU per
   tick plus a leaked thread) — but the `all_done` / exit 0 on a SIGTERM-after-wedge is worth
   handing to SRE.
3. **Error wording for malformed YAML changed.** `ed05007` printed `parse <path>`; `f5d5abf` prints
   `is not valid YAML: <saphyr detail>`. Exit code is 1 in both, so §3 is intact, but if any harness
   scenario asserts that text it will have moved. The 58/58 parity run presumably covers this;
   flagging it so nobody is surprised.
4. **Operational, not about the code:** the shared box is at **97 % disk (15 GB free)**. A cold gate
   that starts with `cargo clean` has limited headroom. My own build wiped itself mid-run once
   because wp0's cold gate cleaned the shared `gfix-storage/target` underneath it; I moved to an
   isolated target dir, which is also why my numbers are from a clean build rather than theirs.

## 11.6 A correction to my own probing

My `surrogate_only.yml` probe (`s: "\ud800"`) is refused by `f5d5abf`, and I initially read that as
the fix breaking the §14.14 lone-surrogate tolerance. **It is not a regression:** I ran the same file
against the `ed05007` binary and it is refused there too. `repair_lone_surrogates` is called only
from the **JSON** read path (`fs_utils.rs:158`); `read_yaml_value` never invoked it, so lone-surrogate
tolerance was never a property of the YAML path and my probe asserted something that never held.
Checked before reporting rather than after.

## 11.7 Scope

Read-only review of `f5d5abf`. Built `--release --bin orch` and the `yaml_bomb` test target into
`$TMPDIR/vtarget` (isolated, deleted afterwards) so as not to collide
with wp0's concurrent cold gate. Probe projects and bomb files under
`$TMPDIR/v3/` — outside every repo — deleted afterwards. No file in any
candidate worktree modified; no commits; no state-changing git commands. Timings are single runs on a
shared, loaded machine; every margin reported here is at least two orders of magnitude, far outside
any noise band.

Positive-case exit codes were re-verified by direct per-command capture after an initial shell loop
reported spurious `rc=1` values — the loop was clobbering `$?`, not the binary failing.
