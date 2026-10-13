# Architect rebuttal — red-team review of the parity contract

**Responding to:** [`redteam-parity-contract.md`](redteam-parity-contract.md) (4 blocking, 5 major, 1 minor, 1 assumption attack)
**Author:** architect
**Date:** 2026-10-13

Verdict up front: **9 of 10 accepted, 1 accepted-with-a-refuted-sub-claim.** The review found two genuine
errors in my work — a self-contradiction in §14.12 and a verification gap in §14.4/§14 generally — plus a
security problem in §11.5 I should not have waved through. All accepted items are **applied** to PARITY.md and
ARCHITECTURE.md.

Where I measured rather than argued, the measurement is cited. Two sub-claims were **refuted by measurement**
and I say so rather than conceding them.

---

## Summary table

| # | Severity | Verdict | One-line reason | Applied in |
|---|---|---|---|---|
| O1 | blocking | **accept** | Confirmed in source: `CACHE_DIR` is `homedir()/.orchestry` with no env override; the oracle spawns `npm view` and `npm install -g`. The gate is non-hermetic and mutates the host. | PARITY §1.4, §6.1; ARCH §8.2 |
| O2 | blocking | **accept (main claim); refute sub-claim 2** | Merge keys are a real bifurcation — measured. The multiline-round-trip failure mode is **refuted**: both parsers round-trip all 10 edge cases. | PARITY §1.4, §6.4, §15; ARCH §8.2 |
| O3 | blocking | **accept** | Correct: I built the instrument early and the measurable surface last. WP-0b walking skeleton added as a hard gate. | ARCH §9 |
| O4 | major | **accept — reverse my decision to REPLICATE** | The reviewer and lead are both right and I was wrong. Determinism on one side of a comparison buys nothing. | PARITY §14.4 |
| O5 | major | **accept in full** | A CLI replacing its own executable as a side effect of `orch task list` is a security surface, not parity. I should have caught this. | PARITY §11.5, §13 |
| O6 | major | **accept, and go further** | Wizard→entity field mapping writes L0 data, so it is **not** L2. Re-graded, enumerated, WP-6 re-rated. | PARITY §12; ARCH §9 |
| O7 | major | **accept** | Correct: the highest-density behaviour in the system was prose-verified only. Three oracle scenarios added; exclusions made explicit. | PARITY §7.4, §15; ARCH §8.2 |
| O8 | minor | **accept** | A mitigation living only in a risk register is not scheduled. Named checkpoint + amendment rule added. | ARCH §9, §11 |
| O9 | major | **accept option (a)** | I contradicted myself in adjacent sentences. Drain-and-discard fixes the deadlock at zero observable cost. | PARITY §14.12 |
| §14 structural | blocking | **accept** | The sharpest finding in the review. A `FIX` whose benefit is asserted in harness terms, where the harness compares against the unfixed oracle, is incoherent. | PARITY §14 (all entries) |
| A2 | assumption | **accept** | A2 is wrong in the source, not hypothetically, and §1.1 upgraded the exposure from accident to advertised feature. | ARCH §2; PARITY §15 |

Lead's two decisions recorded as given: **§14.1 `claimed: {}` → REPLICATE** (lead owns; user-visible call) and
**§12 minimum-terminal-size guard → ADD** as a deliberate L2 improvement.

---

## Per-objection detail

### O1 — harness not hermetic · **accept**

Verified in source rather than taken on trust:

- `src/cli/update-check.ts:15-16` — `const CACHE_DIR = path.join(os.homedir(), '.orchestry')`,
  `CACHE_FILE = <CACHE_DIR>/update-check.json`. **No environment override exists.**
- `:125` — `INSTALL_MARKER = <CACHE_DIR>/update-installed.json`.
- `:61` — spawns `npm view @oxgeneral/orch version --json` with a 5 s timeout.
- `:146` — spawns `npm` to install.
- `src/bin/cli.ts:210-231` — runs after every command except `tui`/`update`/`serve`, prints to **stderr**, and
  calls `backgroundInstall` when an update exists.

So `ORCHESTRY_GLOBAL_CONFIG_PATH` redirects `global.yml` **only**, exactly as claimed. The gate would be green
offline and red on a developer laptop with no code change, and would install a global npm package on whatever
machine ran it.

**Applied:** harness sets `HOME` **and** `USERPROFILE` per tmpdir, pre-seeds a fresh `update-check.json` with
`checked_at: <now>` so the SWR cache is warm, and asserts the oracle spawned no `npm` and made no network call.
`update-check.json` and `update-installed.json` added to §6.1 as cross-binary state, with r-orch stated to
**never write them**.

### O2 — YAML verified through the Rust parser · **accept main claim, refute sub-claim 2**

**Accepted and applied.** Verifying both trees with `serde_yaml_ng` makes every "js-yaml reads Rust's bytes
differently" bug invisible by construction. That is a real structural hole and the generalized cross-parse is
the right fix — it subsumes V8 and does not require me to enumerate schema constructs correctly forever.

**Merge keys: confirmed, and worse than stated.** Measured:

```
input:  base: &b {max_turns: 99, adapter: codex}
        scheduling: {<<: *b, poll_interval_ms: 5}

js-yaml        → scheduling = {max_turns:99, adapter:codex, poll_interval_ms:5}
serde_yaml_ng  → scheduling = {"<<": {max_turns:99, adapter:codex}, poll_interval_ms:5}
  (after .apply_merge() → matches js-yaml)
```

Exactly the bifurcation described. **Decision: r-orch calls `apply_merge()` on every YAML read**, matching
js-yaml's default-schema behaviour. This is now specified in §6.4, not left to an implementer.

**Sub-claim 2 — multiline round-trip — refuted by measurement.** The review asserts "a multiline role prompt
containing a line of only spaces or a trailing-space line, where `|-` cannot round-trip: r-orch writes it,
js-yaml reads a different string." I tested all ten edge cases in both directions:

| Case | js-yaml own round-trip | serde_yaml_ng own round-trip | Rust reads js-yaml's bytes |
|---|---|---|---|
| plain multiline | OK (`\|-`) | OK (`\|-`) | OK |
| line with trailing space | OK (`\|-`) | OK (**double-quoted**) | OK |
| line of only spaces | OK (`\|-`) | OK (**double-quoted**) | OK |
| trailing newline | OK (`\|`) | OK (`\|`) | OK |
| two trailing newlines | OK (`\|+`) | OK (`\|+`) | OK |
| leading space on line | OK | OK | OK |
| tab / CRLF / only-newline / empty | OK | OK | OK |

Both emitters detect when a literal block scalar would be lossy and fall back to a quoted style — js-yaml keeps
`|-` with the whitespace preserved, serde_yaml_ng switches to double-quoted. **Different bytes, identical
parsed value, in both directions.** The premise that `|-` is chosen blindly is incorrect for both libraries.

This matters because it is precisely the case §1.4's parenthetical covered, and the reviewer was right that I
had generalized from one sample — so I replaced the sample with this matrix in §6.4 rather than leaving the
assertion unsupported.

**Three genuine schema deltas found while testing** (hand-authored `config.yml` only — nothing ORCH writes can
produce them), now documented in §6.4:

| Construct | js-yaml | serde_yaml_ng |
|---|---|---|
| `!!binary aGk=` | byte map `{"0":104,"1":105}` | string `"aGk="` |
| `!!timestamp 2026-01-01` | `"2026-01-01T00:00:00.000Z"` (normalized) | `"2026-01-01"` (verbatim) |
| complex key `? [a,b]` | `{"a,b": 1}` | parses, different representation |
| duplicate keys | **throws** | **rejects** — agree, no action |

All four are caught automatically by the generalized cross-parse, which is the argument for that fix over
enumeration.

### O3 — gate only begins working after ~20k lines · **accept**

Correct, and the distinction between building the instrument and building the measurable surface is the part I
missed. R6's mitigation satisfied its letter and none of its intent.

**Applied:** **WP-0b "walking skeleton"** — a hard gate before WP-4 and WP-5, owned by the WP-0 engineer:
`init`, `agent add`, `task add`, `task list` end to end, plus the harness skeleton, **≥1 green differential
scenario and ≥1 green `rust_then_ts` cross-compat scenario**.

**Confirming the lead's position on parallelism, with one refinement.** WP-2/3/6 may proceed after WP-0 because
their acceptance oracles are independent of CLI integration — but that is only true of WP-3 *if the prompt
fixtures are generated correctly*, and the obvious route does not work:

- WP-2's oracle is argv snapshots + stream-parsing fixtures, both derived from TS **unit tests**. Independent. ✓
- WP-6's oracle is `TestBackend` cases written from `docs/parity/tui.md`, with no TS oracle at all. Independent. ✓
- WP-3's oracle is golden prompts from the TS engine. `src/index.ts` **does not export the template engine or
  the default templates**, so they cannot be reached through the package entry point, and reaching them through
  `orch run` + `run.prompt` would make WP-3 depend on the skeleton. **Specified fix:** generate fixtures with
  `npx tsx` against `src/infrastructure/template/template-engine.ts` directly (`tsx` is already a devDependency
  and `npm run dev` uses it). With that, WP-3 is independent. ✓

So the lead's position holds, and §9 now names the generation mechanism so WP-3 does not discover this at
fixture time.

### O4 — §14.4's justification is backwards · **accept; I reverse my own decision**

The reviewer is right and so is the lead, and my original reasoning was simply wrong: the harness compares Rust
against the **unfixed** oracle, so making Rust deterministic does not make it *agree*. I asserted a benefit in
terms of a mechanism that cannot deliver it.

**Reversed: FIX → REPLICATE.** Confirming the lead's claim that TS tie-resolution is deterministic for the same
operation sequence, with the three preconditions it depends on — all now written into §14.4:

1. `save()` must replicate index semantics exactly: filter the entity out, then **push to the end**. Index
   order is therefore last-write order.
2. The scoring sort must be **stable**. ECMAScript ≥2019 requires `Array.prototype.sort` to be stable; Rust's
   `slice::sort_by` is stable and `sort_unstable_by` is not — so **`sort_by` is mandatory here**, and that is
   now stated rather than left to taste.
3. One honest hole: `rebuildIndex` derives order from `fs.readdir`, which is filesystem order. Entity filenames
   contain random nanoids, so **two tmpdirs can enumerate differently**. This is reachable only when
   `_index.json` is missing or corrupt with ≥2 entities. Recorded in §14.4 as the one tie-order case that
   remains unverifiable, with the scenario rule: do not construct scenarios that rebuild a multi-entity index.

Net effect: tie behaviour becomes **oracle-verifiable** instead of excluded, which is strictly better than what
I proposed. This is the item where being argued out of my position improved the design most.

### O5 — `update` invents a network self-update · **accept in full**

No defence offered. The reviewer's "what would change my mind: nothing" is justified. I carried "reimplemented
against the r-orch release channel" over from the TS behaviour without noticing that the mechanism changes
character completely: `npm install -g` replacing a package directory is not the same act as **a CLI
downloading and overwriting its own executable as a side effect of `orch task list`**. There is no channel, no
signing story, no atomic-replace story, and no way for the harness to compare the two sides. It buys neither
external compatibility nor test determinism — the two things ARCHITECTURE §10 claims every decision buys.

**Applied:** `orch update [--check]` keeps its name, flag and exit 0; it prints the current version and how to
install, performs **no network access and no self-replacement**. The post-command background check is removed
entirely. §13 records npm self-update as **out of scope (security)**, not "reimplemented".

**Doctor's `node` row (my call, per the lead):** the row keeps its **position and format**; its label and value
become the r-orch build/toolchain identity. That makes one line of `doctor`'s L0 human output diverge by
construction, so rather than leave it implicit I added a normalizer: **the harness masks the toolchain row** in
`doctor` output, and §5.4 records it as a named divergence. Hiding it inside "L2 decoration" would have been
the dishonest option.

### O6 — "TUI is L2, risk: low" · **accept, and go further than asked**

Accepted. The review's sharpest line is that "no wire contract" is not "no contract" — a wizard writing a task
with the wrong default priority corrupts L0 data through an L2 surface. That is not a grading nuance, it is a
mis-grading.

**Applied, beyond the lead's ask:** the **wizard → domain-entity field mapping is re-graded L0**, because it
writes L0 data. Presentation stays L2. `docs/parity/tui.md` is enumerated into **counted `TestBackend` cases** —
one per wizard field, one per validation message, one per command-bar verb, one per filter predicate, plus key
dispatch per view including the overloaded `a`/`g`/`s`/`c`/`x`/`f` — tracked with a counted total like V1's
generated command-tree test.

WP-6 re-rated **medium**, on schedule grounds (7k LOC), with the note the reviewer asked for: its correctness
risk is **unmeasured rather than low**.

### O7 — long-running and streaming paths escape comparison · **accept**

Correct, and the stall-detection example is well chosen because it is exactly the failure mode where a test
written from the same sentence as the bug passes. I checked the specific ambiguity it raises, since a spec that
permits two readings is the defect:

`orchestrator.ts` `collectEvents` updates `last_event_at` **inside the `for await (const event of generator)`
loop** — so the clock is per emitted **AgentEvent** (post-parse), not per stdout line. Consequence, now pinned
in §7.4: a line the adapter's parser maps to `null` (e.g. a blank line) does **not** refresh the stall timer,
while an unparseable non-blank line **does** for claude, because its parser falls back to emitting an `output`
event carrying the raw line. That is the distinction the reviewer predicted would be left open, and it is now
closed in the text.

**Applied:** three cross-compat `serve --once` scenarios with fake agents — one stalling past
`stall_timeout_ms`, one killed mid-run, one saturating `max_concurrent_agents` — compared on the resulting
`.orchestry/` tree and `--json` log records. And §15 now **states the exclusions explicitly**: `tui`,
`logs --follow`, and watch-mode `serve` are deliberately outside the harness.

### O8 — trait freeze has no scheduled checkpoint · **accept**

Accepted. The framing is right: the danger of a freeze plus an "explicit amendment" process is not that the
traits are wrong, it is that **working around a trait is always the cheaper local move**. The cache-invalidation
example is a plausible instance.

**Applied:** a named checkpoint in §9's integration order — *after WP-1, WP-2 and WP-3 each have one vertical
slice, re-open the trait surface for one amendment round; owner: the WP-0 engineer* — plus the standing rule:
**any package that finds itself working around a trait files an amendment instead.**

### O9 — §14.12's neutrality claim is false · **accept option (a)**

The reviewer is right, and this is a plain self-contradiction in my own text: I wrote "the fix cannot change any
behaviour that currently works" and then, one sentence later, "the tail then also enriches their error
messages." Both cannot be true. The evidence chain is correct — `utils.ts:152-158` appends the tail to the
message and **then** classifies that string, so attaching the consumer changes L0 message text (§3.1), the
error kind (§8.8/V13), and the persisted `agent.last_error` in `agents/<id>.yml` (§6.1).

**Applied: option (a) — drain and discard.** `grok` and `antigravity` attach a consumer that reads stderr to
EOF and **throws the bytes away**. The deadlock is fixed; output stays byte-identical to the oracle; no scenario
exclusion is needed; the fixed path stays covered. This is the better choice at stance 0.5 and I should have
reached it myself instead of reasoning from a conclusion.

### §14 structural defect · **accept**

The best finding in the review, because it generalizes: a `FIX` is adopted, its benefit is asserted in terms of
the differential harness, and nobody traces that the harness compares against the **unfixed** oracle — so each
such fix silently converts a verified row into an excluded one and shrinks the gate.

**Applied:** every §14 entry now carries a required **"effect on differential comparison"** column with one of
three values: `none`, `scenario exclusion required (named)`, or `oracle patched`. A `FIX` without that field is
incomplete. After the O4 and O9 reversals the accounting comes out better than the reviewer predicted:

| Entry | Effect |
|---|---|
| §14.4 `findBestAgent` | **`none`** — reversed to REPLICATE, so ties are now oracle-verifiable (the reviewer predicted `scenario exclusion required`; reversing the decision removed the exclusion) |
| §14.12 grok/antigravity stderr | **`none`** — option (a) keeps bytes identical (likewise) |
| §14.8 TUI type picker | `none` — the TUI is outside the harness |
| §14.9 suite flakiness | `none` — not a behaviour |
| §14.6 unknown Liquid filters | `scenario exclusion required` — user-supplied templates with unknown filters |
| all `REPLICATE` entries | `none` by construction |

So the column's first use retired two exclusions rather than documenting them. That is the value of the column.

### A2 — "single writer" · **accept**

Accepted; A2 is wrong in the source, not hypothetically. All three legs check out: `lock.ts`'s own header says
one-shot commands do not take the lock; `IndexManager.readIndex` falls through to `rebuildIndex`, which
`ensureDir`s and **writes** `_index.json`, so `task list` is a mutating read; and the promise-chain mutex is
in-process only. My "pre-existing property, not a regression" was the wrong move, because §1.1 now *promises*
interchangeable operation, which converts the accident into an advertised feature.

**Applied:** A2 restated as **"single *orchestrator*, multiple concurrent mutators of `_index.json`"**, with the
lost-update mechanism spelled out (entity file on disk, absent from the index ⇒ invisible to `task list`,
`status` and dispatch but visible to `task show`; self-heals only on index deletion or a rebuild-triggering
corruption). A verification row is added that **documents rather than hides** the behaviour, and I note that
the serialization cannot be fixed unilaterally — any lock r-orch takes is invisible to the TS binary — so
stating it is the only honest option.

---

## What I pushed back on

Two items, both sub-claims rather than objections, both settled by measurement rather than argument:

1. **O2's multiline round-trip failure mode** — refuted. Both emitters fall back from `|-` to a quoted style
   when a literal block scalar would be lossy, and each reads the other's output correctly across all ten edge
   cases. I replaced §1.4's one-sample assertion with the measured matrix, so the claim is now supported
   instead of merely corrected.
2. **O6's implicit framing that the whole TUI is mis-graded** — partially. Presentation genuinely is L2 and I
   kept it there; it is specifically the **wizard → entity field mapping** that is L0, and naming that subset
   is more useful to WP-6 than re-grading 7k lines wholesale.

Neither changes any accepted outcome.
