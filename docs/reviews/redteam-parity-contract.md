# Red-team review — the parity contract and how parity is proven

**Reviewer:** red-team (adversarial, read-only)
**Date:** 2026-10-13
**Under attack:** `docs/PARITY.md` (rev. 2648 lines) §1, §6.4, §13, §14, §15 and
`docs/ARCHITECTURE.md` (`md5 3b9d9c0f…`, unrevised) §2, §4.2, §4.3, §5.6, §8, §9, §11.
**Reference:** `oxgeneral/ORCH` @ `c066dc0`, vendored at `parity/orch-ts`.
**Stance:** proposal sits at 0.5; attacked at the mirror of whichever direction each choice leaned.

Re-read after the revision that added §14.12, §14.13, the TUI goal-linking out-of-scope row and the
`--json`/exit-3 corrections. §1.4, §6.4, §11.5, §13's `update` row and §14.4 are byte-identical to the
version first reviewed; §15 still ends at V30. O1–O8 therefore stand as originally filed. O9 is new.

---

## 1. Steelman

A 26k-line TypeScript orchestrator is being reimplemented in Rust against a pinned commit. Rather than
"rewrite and hope," the architect graded every observable into L0 (wire contract) / L1 (behavioural) /
L2 (best effort), wrote the wire contract down field by field, and made the primary gate a differential
harness that runs the *same argv* against both binaries in parallel tmpdirs and deep-compares stdout,
stderr, exit code and the whole `.orchestry/` tree — plus cross-compat scenarios where the two binaries
take turns in one directory, which is the only thing that can catch "Rust writes bytes TS misreads."
The riskiest unknowns (Liquid byte-exactness, YAML timestamp typing, baseline suite flakiness) were
*measured* rather than assumed, each with a named fallback and an escalation trigger. This is a
materially stronger plan than most rewrites of this size, which have no parity contract at all.

---

## 2. Objections

### O1 — The differential harness is not hermetic, and the TS oracle emits nondeterministic stderr

**Claim under attack.** ARCHITECTURE §8.2: "`ORCHESTRY_GLOBAL_CONFIG_PATH` is pointed into each tmpdir
so the user's real `~/.orchestry/` is never touched and global config is part of the comparison."

**Why it's wrong.** `src/bin/cli.ts:211-231` runs `checkForUpdateSWR` after *every* command except
`tui`, `update` and `serve`, then calls `printUpdateNotification` — which writes to **stderr** — and
`backgroundInstall`, which runs `npm install -g @oxgeneral/orch@latest`. In `src/cli/update-check.ts`,
`CACHE_DIR` is hard-coded to `path.join(os.homedir(), '.orchestry')` with **no environment override**;
`ORCHESTRY_GLOBAL_CONFIG_PATH` is consumed only by `global-config-store.ts:18-19` and redirects
`global.yml` alone. Neither `update-check.json` nor `update-installed.json` appears in PARITY §6.1's
file map, so neither is modelled as shared state between the two binaries.

**Concrete failure.** `cargo xtask parity` compares stderr and is a required merge gate. On a cold cache
the oracle spawns `npm view @oxgeneral/orch version` with a 5 s timeout on every step. Once any version
above 1.0.34 is published, every TS step appends two stderr lines (`Update available: 1.0.34 → …` /
`Run: npm install -g @oxgeneral/orch`) whose presence depends on a 4-hour cache written to the
developer's real home directory — so the gate is green in an offline CI container and red on a laptop,
or the reverse, with no code change. Separately, the required gate silently installs a global npm
package on whatever machine runs it. A third-order effect: `isRestartNeeded`/`update-installed.json` is
read by the TUI, so a parity run can leave the developer's interactive TUI claiming a restart is needed.

**What would change my mind.** The harness sets `HOME` (and `USERPROFILE`) per tmpdir **and** pre-seeds
a fresh `update-check.json` with `checked_at: Date.now()`, plus a scenario asserting the oracle made no
network call and spawned no `npm`. Add `~/.orchestry/update-check.json` and `update-installed.json` to
§6.1 as shared cross-binary state, or state explicitly that r-orch must not write them.

**Severity: blocking.** The acceptance gate for the entire project is non-deterministic and has a side
effect on the host.

---

### O2 — YAML cross-compat is verified almost entirely through the Rust parser

**Claim under attack.** §1.4 declares YAML emitted bytes "not contract — compare parsed values," and
ARCHITECTURE §8.2 parses each artifact "according to its extension." V8 is the only check that runs a
Rust-written artifact through the TypeScript reader.

**Why it's wrong.** If both trees are parsed with `serde_yaml_ng`, every bug of the form *js-yaml reads
Rust's bytes differently* is invisible by construction — the two trees agree under a parser that shares
the writer's assumptions. V8 narrows even further: "assert `typeof === 'string'` for every timestamp
field." That covers exactly one scalar class on a known field list. The §6.4 safe-list (`yes`, `no`,
`on`, `off`, `null`, `true`, `false`, numeric-looking strings) is a *scalar* whitelist and says nothing
about schema-level deltas between js-yaml's `DEFAULT_SCHEMA` and `serde_yaml_ng`'s YAML-1.2 core:
merge keys (`<<`), `!!binary`, `!!omap`/`!!pairs`/`!!set`, explicit tags, complex (`?`) keys, and
duplicate-key handling. `serde_yaml`-family parsers do not apply merge keys unless `apply_merge()` is
called explicitly; js-yaml resolves them by default. §1.4's parenthetical that folded `>-` and literal
`|-` "both round-trip to the same string" is asserted from one sample (the 3 KB `agt_creator` role) and
generalized to all user content.

**Concrete failure.** `config.yml` is the live target, because §6.5 reads it as a free-form value tree
deep-merged over `DEFAULT_CONFIG` and `config set` writes arbitrary user values at arbitrary dot paths.
A user hand-authors `config.yml` with `defaults: &d {…}` / `scheduling: {<<: *d, …}`. The TS binary
resolves the merge and sees the inherited keys; r-orch sees a literal `<<` key and falls back to
`DEFAULT_CONFIG` for everything the merge would have supplied — so the two binaries run the same project
with different `max_concurrent_agents` and different adapter defaults. Same shape for a multiline role
prompt containing a line of only spaces or a trailing-space line, where `|-` cannot round-trip: r-orch
writes it, js-yaml reads a different string, and that string is injected into prompts (§9.1, L0,
byte-exact). In every case both trees compare equal under the Rust parser, the harness is green, and the
user's project is silently bifurcated — which is precisely the failure ARCHITECTURE §11 R1 and the
`--allow-foreign-project` valve exist to prevent.

**What would change my mind.** Generalize V8: *every* YAML artifact in both trees is re-parsed **in the
TS process** with js-yaml and deep-compared against the Rust parse, as a harness step rather than a
hand-written test. It is roughly fifty lines of Node glue, it subsumes the timestamp assertion as a
special case, and it is the only check that actually tests the claim §6.4 calls "the cross-compat crux."
Separately: state which parser §1.4's "compare parsed values" means, and extend §6.4's verified list
from scalars to schema constructs.

**Severity: blocking.** This is the single highest-value missing check in the plan.

---

### O3 — The acceptance gate only begins working after roughly 20k lines are written

**Claim under attack.** ARCHITECTURE §9 orders WP-5 (CLI) after WP-4 and states "the differential
harness becomes meaningful the moment this package lands — it is the integration point." §11 R6 names
the opposite as a risk: "Differential harness built late, so parity is asserted only by prose,"
mitigated by "build the skeleton during WP-0/WP-1."

**Why it's wrong.** The mitigation builds the *harness*, not the thing the harness measures. A scenario
step is an argv line executed against a binary; until `orch init` and `orch task list` exist end to end,
every scenario fails at step 1 and zero rows of V1–V8, V20, V21 can run. So the plan builds the
measuring instrument early and the measurable surface last, which satisfies R6's letter and none of its
intent. Nothing in §9 produces a vertical slice; WP-1/2/3/4/6 are all defined as horizontal layers with
acceptance against ported unit tests and `test_support` fakes — i.e. against each engineer's own reading
of PARITY.md.

**Concrete failure.** Six packages ship individually green. At integration the prompt golden fixtures
are off by a blank-line run, `print_table`'s trailing padding differs by one space, `_index.json` entity
field order differs, and the `Option`/`skip_serializing_if` discipline was applied inconsistently across
three crates — all surfacing in the same week, through a harness that has never been green, with no
last-known-good scenario to bisect against. The schedule response is to relax the normalizers until
green, which is how an L0 contract quietly becomes an L1 one.

**What would change my mind.** A hard gate before WP-2/3/4/6 begin: a walking-skeleton `orch` binary
supporting `init`, `agent add`, `task add`, `task list` only, with at least one green differential
scenario and one green `rust_then_ts` cross-compat scenario. Name it WP-0b with an owner. It costs days
and it converts the harness from a final exam into a ratchet.

**Severity: blocking.**

---

### O4 — §14.4's justification for fixing the `findBestAgent` tiebreaker is backwards

**Claim under attack.** §14.4: sorting by `(score desc, agent.id asc)` "changes which agent is picked in
a tie, which is unobservable to any correctness property, and makes the differential harness able to
assert agent selection at all."

**Why it's wrong.** The harness compares Rust *against the unfixed oracle*. The fix makes Rust
deterministic; it does not make Rust agree with TS, which still resolves ties by `_index.json`
last-write order. Determinism on one side of a comparison buys nothing.

**Concrete failure.** A scenario with two equal-scoring agents dispatches `agt_alpha` under Rust and
whichever agent was written last under TS. `task.assignee`, the run record, `agents/<id>.yml` statistics
and `task list --json` all differ, so V3 and V6 go red on an intended change. The only way to keep the
harness green is to construct every scenario so ties never occur — which leaves tie behaviour entirely
unasserted and makes the stated benefit exactly inverted. Second-order: because agent selection
determines which prompt gets rendered and which workspace gets prepared, tie-free scenario construction
quietly constrains the whole scenario corpus.

**What would change my mind.** A worked scenario in which both binaries face a genuine tie and the
comparison passes. Failing that: keep the fix (it is defensible on its own merits) but state honestly
that it makes tie behaviour *unverifiable* against the oracle, and record which scenarios must avoid
ties.

**Severity: major.**

---

### O5 — The `update` reimplementation invents a network self-update nobody asked for

**Claim under attack.** §13 ("npm-based `update` → reimplemented against the r-orch release channel,
same command, flags and exit codes") and §11.5, which keeps "a background, cache-only check [that] runs
after every command except `tui`, `update` and `serve` … when an update is available it also kicks off
a detached background install."

**Why it's wrong.** There is no r-orch release channel; it is being invented as a parity requirement.
And the behaviour being ported is not a neutral mechanism swap: in the original, `npm install -g`
replaces a package directory managed by a package manager the user installed deliberately. In a Rust
port it means a CLI silently downloading and replacing **its own executable** as a side effect of
`orch task list`. That is a new supply-chain and integrity surface (where is the channel, who signs it,
what happens when the running binary is overwritten mid-process), it requires network access from a
command that otherwise needs none, and it is unverifiable by the harness by construction — the TS side
hits npm, the Rust side hits a nonexistent endpoint, so no scenario can compare them. "Functional
parity" on a *distribution* mechanism is the one place where replicating faithfully is strictly worse
than not replicating. The stance dial does not reach this: ARCHITECTURE §10 claims every decision buys
external compatibility or test determinism, and this one buys neither.

**Concrete failure.** r-orch ships; a user runs `orch status` in CI; the binary reaches out to a release
channel and replaces itself mid-pipeline, or the channel 404s and the user gets a stderr warning on
every command forever. Neither outcome is parity with anything, and neither is testable.

**What would change my mind.** Nothing. Scope `update` to `--check` reporting against a channel if one
ever exists, or print the install command and exit 0; drop the post-command background install entirely
and record it in §13 as *out of scope* rather than *reimplemented*. If the background check is kept,
PARITY must specify the channel, the signature verification and the atomic-replace semantics — at which
point it is obviously a feature, not parity.

**Severity: major.**

---

### O6 — "TUI is L2, risk: Low" is not an honest reading of "functionally parity"

**Claim under attack.** §12 grades the TUI L2 overall (keymap, command-bar verbs and wizard steps L1);
ARCHITECTURE §9 rates WP-6 the **lowest**-risk package — "it is L2, has no on-disk or wire contract, and
failures are visible immediately."

**Why it's wrong.** The TUI is ~10k of ~26k lines and it is the **default command**: §2.3 and
`cli.ts:206-208` push `tui` when `orch` is run bare, so it is most users' entire experience of the
product. L2 is defined in §1.3 as "same feature and intent; presentation may differ," verified by
"manual checklist, smoke tests." But §12's own checklist enumerates wizard step sequencing, skip
predicates, validation messages, command-bar completion vectors, filter pickers and soft-delete/undo —
all *functional* behaviour, not presentation. V27 covers "TUI keymap and command-bar verbs" only. So the
largest package, and the default entry point, is gated by a human reading a checklist.

"Failures are visible immediately" is the inverted claim: they are visible to a *user*, which is
precisely why they must be caught before one. And "no wire contract" is not the same as "no contract" —
a wizard that writes a task with the wrong default priority corrupts L0 data through an L2 surface.

**Concrete failure.** r-orch ships with the agent wizard's "skip model step when adapter is shell"
predicate inverted, or the logs type-picker filtering nothing. `cargo test`, `cargo xtask parity` and
every V-row are green, because none of them look. The first person to find it is the user, on the
default command.

**What would change my mind.** Convert `docs/parity/tui.md` into enumerated `TestBackend` cases — one
per wizard field, one per validation message, one per command-bar verb, one per filter predicate — with
a counted total tracked like V1's generated command-tree test, and re-rate WP-6's risk as medium on
*schedule* grounds (7k LOC) with an explicit note that its correctness risk is unmeasured rather than
low. The new §13 row excluding a TUI goal-linking editor is a correct scope cut and does not affect
this.

**Severity: major.**

---

### O7 — Every long-running and streaming path escapes automated comparison

**Claim under attack.** §15 presents V1–V30 as covering the contract, with ARCHITECTURE §8.2's harness
as "the primary guarantee."

**Why it's wrong.** A scenario step is an argv line run to completion. Four surfaces cannot be expressed
that way: `tui`, `serve` without `--once`, `run --watch`, and `logs --follow` (confirmed present —
`src/cli/commands/logs.ts:22,34-35,204-207`, which subscribes to the EventBus and streams). V20 and V21
cover only `serve --json` log *fields* and `serve --once` exit codes. So the orchestrator's live
behaviour — tick phase interleaving, retry/backoff timing, stall and zombie detection, concurrency
capping under load, claim/release races, graceful shutdown — is verified **only** by V22–V25, which are
Rust-side integration tests asserting against PARITY.md's prose. There is no oracle in that loop. The
spec is the architect's reading of `orchestrator.ts`, and §11 R3 concedes that file is 1949 lines with a
long bug-fix history (`-bugfixes-2026-03`, `-resilience`, `-retry-race`, `-parallel-race`,
`-perf-opts`, `-endurance`).

**Concrete failure.** The spec says stall detection fires at `stall_timeout_ms` since the last event.
Suppose TS measures from last *stdout line* and Rust from last *parsed AgentEvent*; a chatty agent
emitting unparseable lines is reaped by one and not the other. V24 passes — it was written from the same
sentence that produced the bug. Nothing else looks. Multiply by every timing constant in §7.

**What would change my mind.** Three cross-compat `serve --once` scenarios with fake agents — one that
stalls past the timeout, one that is killed mid-run, one that saturates `max_concurrent_agents` — run
against both binaries with the injected clock on the Rust side and compared on the resulting
`.orchestry/` tree and `--json` log records. That converts the highest-density behaviour in the system
from prose-verified to oracle-verified. Also: state in §15 that `tui`, `logs --follow` and watch-mode
`serve` are deliberately outside the harness, so the gap is visible rather than implied.

**Severity: major.**

---

### O8 — WP-0's trait freeze has no scheduled integration checkpoint

**Claim under attack.** §9 WP-0: "The trait surface is then frozen — later changes need an explicit
amendment, because five packages compile against it." §11 R5's mitigation: "Budget one re-sync
checkpoint after WP-1/2/3 each have a vertical slice."

**Why it's wrong.** That checkpoint appears in the risk table and nowhere in §9's "Integration order and
parallelism," which has no owner, no slot and no acceptance criterion for it. A mitigation that exists
only in a risk register is not scheduled. More importantly, the attack on a freeze is not "the traits
will be wrong" — §5 is detailed enough that they will be roughly right. It is that with six agents
working in parallel against a *frozen* surface and an "explicit amendment" process, the cheap local move
is always to work around the trait rather than amend it.

**Concrete failure.** The `Storage` traits carry no cache-invalidation hook, because WP-0 cannot know
that the tick-scoped cached wrappers (WP-1) and the orchestrator's per-tick reads (WP-4) need to agree
on when a cache dies. WP-4 adds its own memoization inside the orchestrator rather than filing an
amendment. Now there are two caches with different lifetimes over the same store, the orchestrator reads
a task it already mutated, and the symptom is an intermittent invalid-transition error that no unit test
reproduces because `test_support`'s in-memory fakes clone on every read.

**What would change my mind.** A named checkpoint in §9's integration order — "after WP-1, WP-2 and WP-3
each have one vertical slice, re-open the trait surface for one amendment round, owner: WP-0 engineer" —
plus a standing rule that any package working around a trait must file the amendment instead.

**Severity: minor** on its own; **major** if O3 is rejected, since without a walking skeleton this is the
only integration feedback before the end.

---

### O9 — §14.12's "cannot change any behaviour that currently works" is false, and the matrix has no row for the consequence

**Claim under attack.** §14.12, on fixing `grok` and `antigravity` never attaching a stderr consumer:
"the fix cannot change any behaviour that currently works (an unread pipe produces no events either
way)."

**Why it's wrong.** The diagnosis is correct and the fix is right — `src/infrastructure/adapters/grok.ts`
and `antigravity.ts` contain **no occurrence of the string `stderr`**, so they never attach
`createStderrTailCapture` (`src/infrastructure/adapters/utils.ts:116`), and a child writing past one pipe
buffer blocks forever. But the fix is not observationally neutral, because the tail is appended to the
error *message* and the classifier is then run on that string:

```ts
// src/infrastructure/adapters/utils.ts:152-158
const msg = appendStderrTail(
  `${adapterName} process exited with code ${exitCode}`, adapterName, stderrTail(),
);
const classified = classifyAdapterError(msg, exitCode);
```

`appendStderrTail` (`utils.ts:92`) injects `\n--- <adapter> stderr (tail) ---\n<tail>`. Attaching the
missing consumer therefore changes three things PARITY declares L0: the error **message text** (§3.1),
the **error kind** from `classifyAdapterError` (§8.8, V13 — the classifier pattern-matches a string that
now contains the child's stderr), and the **persisted** `agent.last_error`, written into
`agents/<id>.yml` by `src/application/orchestrator.ts:1583` (§6.1). §14.12 concedes the third in its own
next sentence — "the tail then also enriches their error messages" — directly contradicting the sentence
before it.

**Concrete failure.** A differential scenario runs a fake `grok` that writes 200 bytes to stderr and
exits 1. TS persists `last_error.message = "grok process exited with code 1"` and kind `unknown`; Rust
persists that plus the tail block, and if the stderr text contains a rate-limit or `ENOENT` marker the
kind differs too — which changes the hint printed (§8.9) and the retry decision. V5, V6, V13 and V15 go
red on a change the author intended. Under schedule pressure the resolution is to stop exercising
grok/antigravity stderr in scenarios, removing the only coverage of the thing just fixed.

**What would change my mind.** Either (a) capture and **discard** the tail for these two adapters —
preventing the deadlock while keeping output byte-identical to the oracle, which costs nothing and is
the choice I would make at stance 0.5; or (b) keep the enrichment and state in §14.12 that it diverges
in L0 message text, L0 error kind and the persisted `last_error`, with a named harness exclusion.

**Severity: major.**

---

### The pattern behind O4 and O9 — a structural defect in §14

Both are the same error: a `FIX` is adopted, its benefit is asserted in terms of the differential
harness, and nobody traces that the harness compares Rust against the **unfixed** oracle. Every such fix
converts a verified row into an excluded one, silently shrinking the gate. §14's default policy of "fix,
and document" is missing a third obligation.

**What would change my mind.** §14 gains a required column — *effect on the differential comparison* —
with one of three values per entry: `none`, `scenario exclusion required (named)`, or `oracle patched`.
Any `FIX` without that field is incomplete. Applied retroactively, §14.4 and §14.12 both become
`scenario exclusion required`, which is the honest accounting, and §14.8 (TUI type picker) becomes
`none` because the TUI is outside the harness.

**Severity: blocking**, as a contract-level defect in §14 distinct from either instance.

---

## 3. Load-bearing assumption

**ARCHITECTURE §2 A2 — "Single writer. The PID lock means exactly one process mutates orchestrator
state."** This is the one most likely to be wrong, and it is wrong in the source, not hypothetically:

1. `src/infrastructure/storage/lock.ts`'s own header states "Only one `orch run --watch` can run at a
   time. **One-shot commands do not acquire the lock.**" So `orch task add` while the orchestrator runs
   is a second mutator, and that is the *normal* workflow, not an edge case.
2. `IndexManager.readIndex` falls through to `rebuildIndex` on a missing or corrupt index, and
   `rebuildIndex` calls `ensureDir` and **writes** `_index.json`
   (`src/infrastructure/storage/index-manager.ts:79-124`). So `orch task list` — a read — creates
   directories and writes a file. §6.1 states this outright ("A first `task list` on a fresh project
   therefore rebuilds the index from an empty directory") without connecting it to A2.
3. The mutex protecting the read-modify-write is a JavaScript promise chain — in-process only, and
   invisible to a second node process, let alone a Rust one.

A2's "if wrong" cell calls this "a pre-existing property, not a regression." That is the move I am
attacking. §1.1 now **promises** that "both binaries must operate on the same project directory
interchangeably, in either order," which converts an accident into an advertised feature and multiplies
the exposure: the realistic user has the TS TUI open in one terminal and the Rust CLI in another, which
is exactly what the parity contract invites them to do.

**Cost if wrong.** Interleaved `_index.json` read-modify-write between a node process and a Rust process
drops entries: `tasks/tsk_X.yml` exists on disk but is absent from the index, so `task list` and
`status` do not show the task while `task show tsk_X` does, and the orchestrator never dispatches it.
It self-heals only when the index is deleted or corrupted into a rebuild. V22 tests the PID lock, which
none of these paths take, so nothing in V1–V30 looks at this.

**Minimum honest fix.** Either restate A2 as "single *orchestrator*, multiple concurrent mutators of
`_index.json`" and add a verification row for concurrent cross-binary index mutation, or make r-orch's
index writes use `O_CREAT|O_EXCL` lock-file serialization on a path the TS binary also respects — which
it cannot, so the first option is the real one, and it should be stated rather than assumed away.

---

## 4. What survives

Attacked and could not break:

- **§1.4's normalization rules.** The JSON-key-order exclusion is correctly reasoned (JS insertion order
  — `Run` assigning `pid`/`finished_at` late, `Message` spreading `id`/`to_agent_id` last — versus serde
  declaration order, with RFC 8259 as the authority), as is the YAML-bytes exclusion (`>-` folding at
  `lineWidth: 120` versus `|-`). Keeping trailing whitespace on table rows **significant** is right and
  easy to get wrong: `print_table` pads the final cell, so it is real output a script could depend on.
  O2 attacks which *parser* interprets "parsed values," not these rules.
- **§14.9 — the flakiness measurement.** Four runs, a diagnosed mechanism (Ink `sleep`-then-assert
  reading a stale `lastFrame()` under thread contention), the discovery that `vitest.config.ts` caps
  `maxWorkers` at 2 under `CI`, a corrected canonical baseline that supersedes the project brief, and
  the consequence drawn for the port (drive `TestBackend` frames synchronously; port the two
  F1 tests `ink-testing-library` could not express). This is the strongest work in either document and
  I found nothing wrong with it.
- **§4.3 / §9.5 — the Liquid analysis.** Measured against liquidjs 10.27.2 on the constructs actually
  used, divergences pushed into the *context builder* (always-present globals with explicit nils;
  `shared_context` as an ordered `[k,v]` array) rather than the engine, gated on golden fixtures with
  "do this first and escalate early," and a bounded named fallback. Correctly identified as the top
  dependency risk, and the `kstring = "2.0.2"` MSRV pin is the kind of detail that is only ever found by
  actually building.
- **§4.7 — shelling out to `git` instead of `git2`/libgit2.** Right for the stated reason (PARITY §10
  specifies exact command lines and exit codes, including `worktree prune` semantics libgit2 models
  differently), and the author flagged it as looking backwards, which is the correct instinct.
- **ARCHITECTURE §5.2's `serde_json::json!` finding.** `serde_json::Map` defaulting to `BTreeMap`
  alphabetizes keys, so a JSONL event emitted via `json!` comes out `{"data",…,"timestamp","type"}`.
  Non-obvious, measured, and exactly the class of bug that would have shipped.
- **§5.6 — the injected `Clock`/`IdGenerator` seams.** The one deliberate non-transliteration, and it
  pays for itself three times over (exact-value assertions in the harness, instant retry/stall tests,
  a whole class of flake removed). Correctly labelled as a deviation rather than smuggled in.
- **§6.2's nanoid verification.** Checking that JS `urlAlphabet` and Rust `nanoid::alphabet::SAFE`
  contain the same 64 characters, rather than assuming it, is the right reflex.
- **The `--allow-foreign-project` valve** (ARCHITECTURE §11, Rollback). The correct answer to R1, it
  costs almost nothing, and it is the only real safety mechanism in a project with nothing to roll back
  to. Keep it default-on until O2's generalized cross-parse check is green, not merely until V8 is.
- **§13's dead-code exclusions.** Spot-checked: declining to port never-mounted components, the
  unreachable `'new_task'` input mode, the advertised-but-unhandled `W` key and the never-emitted
  `team:task_claimed` is correct, and documenting them beats silently omitting them. The new row
  excluding a TUI goal-linking editor is the same discipline applied again.
- **§14.13 — `pi`'s divergence from the shared line reader.** Replicating the `\r`-stripping (newline-
  delimited RPC frames would break otherwise) and the missing 16384 cap is right, and recording the
  asymmetry as intentional is what stops the next engineer "fixing" it.
- **§14.12's diagnosis** (as distinct from its neutrality claim, see O9). Finding an unread-pipe deadlock
  by reading the adapter set against `utils.ts` rather than by testing is exactly what a transliteration
  misses, and the 64 KB / stall-detection-clears-it-in-10-minutes mechanism is stated correctly.

One territory note, not mine to pursue: §6.2's `sanitizeId` (`^[A-Za-z0-9._-]+$`, rejecting rather than
sanitizing, applied to context keys as well as entity ids) is the path-traversal defence, and V30 is the
row that protects it. `security-reviewer` should own whether the Rust port applies it on every path
construction, not just at the store boundary.

---

## 5. Severity tally

| Severity | Count | IDs |
|---|---|---|
| blocking | 4 | O1, O2, O3, §14 structural defect |
| major | 5 | O4, O5, O6, O7, O9 |
| minor | 1 | O8 |

---

## 6. Evidence index

All paths absolute.

| Claim | Evidence |
|---|---|
| Post-command update check, stderr notification, background `npm install -g` | `parity/orch-ts/src/bin/cli.ts:211-231` |
| `CACHE_DIR` hard-coded to the real home; no env override | `parity/orch-ts/src/cli/update-check.ts` |
| `ORCHESTRY_GLOBAL_CONFIG_PATH` redirects `global.yml` only | `parity/orch-ts/src/infrastructure/storage/global-config-store.ts:18-19` |
| One-shot commands never take the lock; mtime staleness; `kill(pid,0)`/EPERM liveness | `parity/orch-ts/src/infrastructure/storage/lock.ts` |
| A read (`readIndex`) writes `_index.json` and creates directories | `parity/orch-ts/src/infrastructure/storage/index-manager.ts:79-124` |
| `logs --follow` streams from the EventBus | `parity/orch-ts/src/cli/commands/logs.ts:22,34-35,204-207` |
| `grok`/`antigravity` never reference stderr | `parity/orch-ts/src/infrastructure/adapters/{grok,antigravity}.ts` |
| Stderr tail appended to message, then classified | `parity/orch-ts/src/infrastructure/adapters/utils.ts:92,116,147,152-158` |
| `last_error` persisted to the agent entity | `parity/orch-ts/src/application/orchestrator.ts:1583` |
| Bare `orch` defaults to the TUI | `parity/orch-ts/src/bin/cli.ts:206-208`; PARITY.md §2.3 |
| Documents under review | `docs/PARITY.md` §1.4, §6.4, §11.5, §13, §14.4, §14.12, §15; `docs/ARCHITECTURE.md` §2, §5.2, §5.3, §8.2, §9, §11 |
