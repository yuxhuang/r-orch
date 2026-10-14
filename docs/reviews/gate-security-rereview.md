# Gate security re-review — r-orch

**Reviewer:** security gate (read-only)
**Candidate:** `../r-orch-wt/gate-review` @ **`ed05007`**
**Prior review:** `docs/reviews/gate-security-review.md` (@ `0b67e4d`) — conditions C0–C5
**Date:** 2026-10-14

---

## Verdict

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
