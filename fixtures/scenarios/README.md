# Parity scenarios

Each `*.toml` here is one scenario. `cargo xtask parity` runs them all against
the vendored TypeScript oracle.

```sh
# Prerequisite: parity/orch-ts/dist, built by parity/setup.sh. The harness
# builds and snapshots the Rust binary itself.
cargo xtask parity                       # all scenarios
cargo xtask parity --filter shell        # name substring
cargo xtask parity --verbose             # also print the id mapping
cargo xtask lint-scenarios               # check the scenarios themselves
```

A failing scenario prints a unified diff — `---` is the oracle, `+++` is
r-orch — plus the id mapping, which is the first thing to read when a diff
looks nonsensical: a wrong mapping means two entities were matched up wrongly
rather than that a value diverged.

Most of the corpus is written **ahead** of the commands it exercises and skips
itself until they exist, so a run normally reports a large skip count. See
[Capability gating](#capability-gating-and-why-most-scenarios-skip).

## Format

```toml
name = "walking-skeleton"
kind = "differential"       # differential | rust_then_ts | ts_then_rust | interleaved
split_at = 4                # required for rust_then_ts / ts_then_rust
requires = ["task show"]    # skip until the Rust binary has these subcommands
fake_agents = true          # fixtures/fake-agents first on PATH, both binaries
fake_editor = "orch-edit-title"   # fixtures/fake-editors on PATH + $EDITOR
timeout_secs = 90           # per step; default 30
env = { ORCH_FAKE_SCENARIO = "fail" }   # inline table — see the warning below
description = "..."         # printed on failure and under --verbose
steps = [
  "init --name proj --adapter claude",
  "task add 'A title with spaces' --quiet",
  "task show ${task.0} --json",
]
verify = ["task list --json"]   # required for the cross-compat kinds
```

Steps are argv lines, shell-quoted, run with the project directory as the
working directory.

> **`env` must be an inline table.** An `[env]` section header swallows every
> key that follows it, `steps` included, and TOML then reports
> `invalid type: sequence, expected a string` from somewhere unrelated. Write
> `env = { KEY = "value" }` on one line.

### `${task.N}` — referring to an entity the scenario created

The two binaries mint different ids, so an id cannot be written literally.
`${task.0}` is the **first task id this tree printed**, resolved per tree; the
prefixes are `task`/`tsk`, `agent`/`agt`, `goal`, `team`, `msg`, `run`.

Ids are learned from each step's stdout in order, which is why most steps that
create something pass `--quiet`: that prints the bare id. A placeholder with no
matching id is a hard error naming what the tree did produce, never a silent
pass-through of the literal text.

`agt_creator` is deliberately **not** learned or normalized — `init` writes it
as a fixed string, so masking it would hide a divergence rather than tolerate
one (PARITY.md §6.2).

### `${repeat:<text>:<count>}` — long arguments

`${repeat:a:3999}😀` expands at parse time to 3999 `a`s followed by the emoji.
The truncation limits are 200 characters for a subject and 4000 for a body, so
testing them needs arguments of that size; written out literally they would
make the scenario unreadable and bury the one character that matters — the
multi-byte one sitting astride the cut. Unlike `${task.N}` this depends on
nothing the tree produced, so the loader and the lint both see the real
argument. Expansions over 64 KiB are rejected as typos.

### Step directives

A step may carry leading `@directive` tokens:

- **`@kill-after:<ms>`** — run it and kill it after `<ms>`. There is no argv
  line meaning "a `serve --once` that dies mid-run", which V25/V35 need. Pair
  it with a **cross-compat** kind so the kill lands once in one shared tree; in
  two separate trees it lands at two different points and the trees diverge
  legitimately, making the scenario flaky rather than wrong.
- **`@exit-only`** / **`@exit-only:<reason>`** — compare the exit code and
  ignore stdout/stderr, for the few steps whose *output* is L2 while the exit
  code is L0: Commander's usage wording and help layout (§1.3, §3). Clap will
  never reproduce Commander's phrasing, so comparing the text would keep those
  steps red for a reason the contract says not to care about.

  Give a reason whenever the oracle actually *runs* the step. `lint-scenarios`
  objects to a bare `@exit-only` on a step that produced real output, because
  that is almost always someone silencing a diff rather than a genuine L2 case;
  the reason is the author overriding that, on the record.
- **`@unordered:<reason>`** — the step prints a JSON **array** whose element
  order is not contract, so the two sides are compared as multisets (every
  element, count and field still compared). Only valid on a `--json` step: a
  human table's row order *is* contract, and a marker there would hide a real
  difference. A bare `@unordered` is rejected — the reason is the point.

  The case that needs it is a `msg broadcast` fan-out. All of its messages
  share a `created_at`, `MessageStore.list` sorts on that field alone, and JS
  sort is stable, so their relative order is the `_index.json` tie-break — and
  the oracle fills the index from `Promise.all`, making the order a libuv
  scheduling artifact that varies between runs of the *same* binary. PARITY.md
  §6.10 says index order is not contract. The scenario-level
  `unordered_files = ["messages/_index.json"]` is the same exemption for the
  file the step reads; keep that list to the files a scenario genuinely cannot
  order, because element-by-element comparison of every other `_index.json` is
  what catches an implementation that appends on update instead of moving the
  entity to the end.

### Kinds

- **`differential`** — both binaries run **every** step, each in its own
  tmpdir, and the two `.orchestry/` trees plus every step's stdout, stderr and
  exit code are compared.
- **`rust_then_ts`** / **`ts_then_rust`** — one shared directory: the first
  binary runs steps `1..=split_at`, the second runs the rest. `rust_then_ts` is
  the direction that actually proves the shared-format claim, because a quoting
  bug in our YAML writer is invisible to a same-binary test.
- **`interleaved`** — alternate per step, starting with r-orch.

### `verify` is what gives a cross-compat scenario teeth

A cross-compat scenario has one tree, so there is nothing to compare *between*
trees. Without `verify` the only assertions would be "every step exited 0" and
"both YAML parsers agree" — neither of which notices that the second binary
*misread* a value it could parse.

`verify` steps run **once per binary over the finished shared tree**, and their
output must match exactly: ids and timestamps included, because there is only
one set of them.

They must therefore be **read-only**. The loader enforces this against an
allowlist in `scenario.rs` rather than trusting the author — a mutating step
would fail its own second run on a duplicate and report a divergence it caused
itself. Extend `READ_ONLY_VERIFY_STEPS` when a new read command lands.

## Capability gating, and why most scenarios skip

A scenario lists the Rust subcommands it needs in `requires`. The harness
probes `<cmd> --help` once per run and **skips** the scenario while any are
missing, naming them. The oracle has every command, so only our side gates.

That is what lets the corpus be written before the implementation: it is a red
bar WP-4/WP-5 land against, and each scenario goes live on its own the moment
its commands exist — nobody has to remember to un-ignore anything. A skip is
reported as a coverage gap on every run, not as a pass, so the number cannot
quietly grow.

Two rules follow from this:

- **Gate on everything a scenario touches, not just its subject.** An ungated
  scenario that uses a missing command goes red with "unrecognized subcommand",
  which is noise. Red should mean *behaviour differs*.
- **But do not gate what is testable today.** `cmd-exit-codes-uninitialized`
  is deliberately ungated: it needs only commands that already exist, and it is
  currently red for a real reason (see below).

## `lint-scenarios`: checking the scenarios, not the implementation

Because most of the corpus is skipped on any given day, and a skipped scenario
is never parsed past its `requires`, a misspelled flag could sit undetected for
weeks and then fail for the wrong reason. `cargo xtask lint-scenarios` replays
every step through the **oracle alone**, which has every command today, and
reports three classes of mistake:

1. an argv the oracle rejects at parse time (a misspelled flag or subcommand);
2. a `${task.N}` whose index the earlier steps never produced;
3. an `@exit-only` marker on a step that produces real output.

Steps are allowed to *fail* — many assert error paths — so a non-zero exit is
not itself a finding.

It paid for itself on first run: `task edit` and `agent edit` take **no flags
at all** (they round-trip the entity through `$EDITOR`), and four scenarios had
invented `--title`/`--description`/`--model` options that would have silently
asserted nothing.

## The same hazard inside a differential run

A step with a misspelled flag makes *both* binaries print a usage error and
exit the same way, so it passes while asserting nothing — silently vacuous,
which is worse than red. So the harness fails any step the **oracle** rejected
at parse time unless it is marked `@exit-only`. Commander writes `error: …` on
stderr while every ORCH domain error is rendered with `✕`, which makes the
prefix an unambiguous discriminator.

## The one named exclusion: `state.claimed` (§14.1)

PARITY.md §14.1 was decided **FIX**, so there is exactly one path the harness
does not compare. TS `status --json` serializes the in-memory state object, and
`JSON.stringify` of a JS `Set` is `{}` whatever it holds — so the oracle's
`state.claimed` is always empty while `state.json` on disk correctly holds an
array. r-orch emits the array, and §14.1 grants a **named** exclusion for that
path in `status --json` comparisons.

A masked value is an unasserted value, so the mask is paired with a replacement
assertion instead of being left as a hole: **r-orch's array must equal the
`claimed` array in the `state.json` it wrote.** The cross-implementation check
on the same data still happens too — `state.json` is an ordinary file in the
tree walk, where both sides agree on the array — so the mask drops a duplicate
comparison in a format that legitimately differs, not the only one.

It is scoped as narrowly as it can be, and the scope is tested:

- `status` with `--json`, and nothing else. `state.running` has the same JS-Map
  problem but §14.1 does not cover it, so it stays compared.
- Masking a path that is **absent** would assert nothing, so a `status --json`
  with no `state.claimed` is a failure on either side. If the *oracle* stops
  emitting it, the message says to re-read the contract rather than to adjust
  the mask.
- Comparing parsed documents could have lost the JSON framing the raw-text
  comparison was covering, so indent and trailing newline (§6.4) are compared
  separately. Key order survives because `serde_json` is built with
  `preserve_order`.
- Two mutation probes assert the hole is exactly its intended size: widening
  the mask to every command is caught, and deepening it to swallow the whole
  `state` object is caught. The second is `probe_pending` until `orch status`
  exists, since nothing exercises it before then.

If a second named exclusion is ever needed, it needs the same four things, and
a line in PARITY.md naming it. An unnamed exclusion is just a field nobody
compares any more.

## What is compared

Per step: stdout, stderr and the exit code. At the end: a recursive walk of
`.orchestry/`, every file parsed by extension (YAML, JSON, JSONL, else raw
text) and deep-compared.

A stdout that is exactly one JSON object or array is compared **parsed**, not
as text: PARITY.md §1.4 states that JSON object key order is not contract, and
it genuinely differs — TS emits JS insertion order, which depends on when a
field was first assigned (a goal that gains an `assignee` after creation emits
it last, after `updated_at`), while serde emits declaration order. Everything
else, scalars and human output included, is compared as text, which is the
stricter test.

Then two things the parsed comparison cannot see on its own:

- **JSON framing** — 2-space indent and a trailing newline are contract
  (PARITY.md §6.4) but invisible to a value comparison.
- **The js-yaml cross-parse (V32)** — every YAML artifact is re-parsed *in the
  node process with the vendored js-yaml* and compared against the Rust parse.
  Verifying both trees with the Rust parser alone would make every
  "js-yaml reads r-orch's bytes differently" bug invisible by construction.
  Values are **type-tagged** on the way back, because `JSON.stringify` turns a
  js-yaml `Date` straight back into an ISO string and would erase the exact
  difference this check exists to find.

Normalization follows PARITY.md §1.4: ANSI stripped, ids mapped consistently
per run, timestamps to `<TS>`, the tmpdir to `<ROOT>`, PIDs to `<PID>`,
measured durations to `<MS>`. Trailing whitespace on table rows is **not**
stripped — the TS table writer pads every cell including the last, and that is
contract.

## Hermeticity

The TypeScript binary writes `~/.orchestry/update-check.json` from almost every
command and spawns `npm view` on a cold cache, so each scenario gets its own
`HOME`/`USERPROFILE` inside the tmpdir with a **pre-seeded, fresh**
`update-check.json` whose `latest` equals the oracle's own version: the cache
is warm, no registry query happens, and no update notification is printed.
`ORCHESTRY_GLOBAL_CONFIG_PATH` points into the same place, `CI=1` and
`NO_COLOR=1` are set, and stdin is `/dev/null`.

The tmpdir must also have **no `.orchestry` ancestor**, because
`findProjectRoot` walks up and would otherwise find the developer's own project
— measured, not hypothetical. That is asserted at setup, so point `TMPDIR`
somewhere outside any ORCH project.

Finally, the harness **builds `orch-cli` from this tree and runs a private copy
of the binary**, rather than reading `target/debug/orch`.

This was observed, not anticipated. The work packages originally shared one
`CARGO_TARGET_DIR` across git worktrees, and artifacts from two trees hash
identically, so `target/debug/orch` was simply whichever tree built last: a run
reported eleven scenarios failing with rich, plausible diffs because it was
grading a worktree that had the full command surface while this tree had three
commands. The worktrees now have their own target directories, but the build
and the snapshot stay — building is what makes the binary *this source*, and
the snapshot is what stops a concurrent rebuild swapping it out mid-run.

Nothing in the harness hard-codes a target path: the binary comes from cargo's
own `--message-format=json` output, and `concurrent_index.rs` derives its from
`CARGO_TARGET_DIR` with a `<repo>/target` fallback.

One non-obvious thing the snapshot depends on: `resolve_library_dir()` probes
the executable's directory and its ancestors for `skills/library` and *then*
falls back to the repository the crate was built from. From a tempdir the probe
finds nothing, so the snapshot works only because of that fallback — removing
it would break every skill-dependent scenario in a way that looks like a parity
failure.

Where a scenario needs a program to exist, it gets a fixture rather than the
machine's own:

- `fake_agents = true` puts `fixtures/fake-agents/` first on `PATH`, so the
  adapters spawn stand-in CLIs. Set it for `doctor` too, or the probe results
  depend on what happens to be installed.
- `fake_editor = "<script>"` puts `fixtures/fake-editors/` on `PATH` and points
  `EDITOR` and `VISUAL` at `<script>`. This is the only way to reach `task
  edit` and `agent edit`, which take no flags.

## Proving the gate can fail

`xtask/probe-harness.sh` introduces a known divergence, asserts the harness
catches it, and reverts — one probe per numbered PARITY requirement. It found
two real blind spots when it was first written, so run it after changing the
harness.

## `serve --once` needs every task to reach a *terminal* status

`once-runner` polls until `tasks.every(isTerminal)`, and **`review` is not
terminal**. So a scenario that drives `serve --once` hangs forever — not fails,
hangs — if any task can end up parked in review. Two ways that happens, both
measured:

- an agent whose `approval_policy` is `manual` or `suggest` leaves a successful
  run in `review`;
- an **unassigned** task can be matched to `agt_creator`, which every tree has
  and whose policy is `suggest`.

So: assign tasks explicitly in any `serve --once` scenario, and use `run <id>`
when the *point* is to reach review — `run` returns when that single run
finishes and leaves the task where it landed.

This was invisible until the oracle patch (see `parity/patches/README.md`) made
`serve --once` actually dispatch; before that it was a no-op that returned
instantly, and three O7 scenarios were quietly wrong. `lint-scenarios` catches
it as a per-step timeout, which is why its budget is a scenario-level setting
rather than something to raise when a step is slow.

## Contract findings from writing this corpus

Two things measured against the oracle that **contradict `docs/PARITY.md`**.
The scenarios encode the measured behaviour; the doc is the architect's to fix.

1. **§3: a bare command group prints its help to stdout.** It prints to
   **stderr**. `orch task` with no subcommand: 0 bytes on stdout, 735 on
   stderr, exit 1 — Commander's `help({ error: true })` path. Measured for
   `task`, `agent` and `config`.
2. **`config global get` cannot read a nested key.** `config global get
   tui.palette` reports no value at all, while `config global show` reports
   `palette: "amber"` in the same tree. Replicated as-is in `cmd-config`,
   because the CLI contract is byte parity, not sanity.

Separately, `task edit` and `agent edit` take **no options**; §2.2's listing of
them alongside flag-taking verbs invites exactly the mistake four of these
scenarios originally made.

And one that needs an architect decision rather than a scenario: **a single
`msg send` can write a file `serde_json` cannot parse.** Bodies are truncated
by JS `.slice()`, which counts UTF-16 code units, so a 4001-code-unit body
ending in a non-BMP emoji is cut between the surrogates and stored as a lone
high surrogate — written as the escape `\ud83d`, which serde_json rejects
outright. `cmd-msg-truncation.toml` is the test for whichever way it is
resolved; the detail and the three options are in docs/TRAITS-FROZEN.md §11.

## Known coverage gaps

Honest limits of the current scenario set:

- **`tui`** is interactive and has no scenario. It cannot have one in this
  harness; it needs a separate pty-driven test.
- **`update`** is deliberately excluded: `update --check` queries the npm
  registry, so a scenario for it would not be hermetic. The pre-seeded cache
  covers the *implicit* check every other command performs. (r-orch's own
  `update` makes no query at all — PARITY.md §11.5 — so the two binaries'
  output differs by design and cannot be compared.)
- ~~**Index tie order under a re-save**~~ — **closed** by `index-tie-order.toml`
  and its `rust_then_ts` pair, which re-save `agt_creator` through
  `agent disable`/`agent enable`: push-to-end and in-place update now produce
  different trees, in both directions. `cmd-task-lifecycle` and
  `cmd-interleaved` widen the same check to tasks once `task assign` is wired
  into them.
- **Stable-sort ties** in `task list` need two tasks sharing a status *and* an
  `updated_at`, which the clock makes unreliable to produce from the CLI.
- **Multi-entity index rebuild** is deliberately not exercised:
  `rebuildIndex` derives order from `fs.readdir`, so two tmpdirs can enumerate
  the same logical set differently (PARITY.md §14.4). Do not add a scenario
  that deletes `_index.json` with ≥2 entities present.
- **Timing-dependent O7 scenarios** drive their clocks through *config*
  (`stall_timeout_ms`, `retry_base_delay_ms`, `poll_interval_ms`) rather than
  an injected Rust-side clock. An injected clock only one binary has would
  make the comparison non-differential; a config key both honour keeps the
  arithmetic itself under test.
