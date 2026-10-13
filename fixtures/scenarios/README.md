# Parity scenarios

Each `*.toml` here is one differential scenario. `cargo xtask parity` runs them
all against the vendored TypeScript oracle.

```sh
# Prerequisites: `cargo build -p orch-cli`, and parity/orch-ts/dist built by
# parity/setup.sh.
cargo xtask parity                       # all scenarios
cargo xtask parity --filter shell        # name substring
cargo xtask parity --verbose             # also print the id mapping
```

A failing scenario prints a unified diff — `---` is the oracle, `+++` is
r-orch — plus the id mapping, which is the first thing to read when a diff
looks nonsensical: a wrong mapping means two entities were matched up wrongly
rather than that a value diverged.

## Format

```toml
name = "walking-skeleton"
kind = "differential"       # differential | rust_then_ts | ts_then_rust | interleaved
split_at = 4                # required for rust_then_ts / ts_then_rust
description = "..."         # printed on failure and under --verbose
steps = [
  "init --name proj --adapter claude",
  "task add 'A title with spaces'",
]
verify = ["task list --json"]   # required for the cross-compat kinds
```

Steps are argv lines, shell-quoted. They are run with the project directory as
the working directory.

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
allowlist (`task list`, `agent list` today) rather than trusting the author — a
mutating step would fail its own second run on a duplicate and report a
divergence it caused itself.

## What is compared

Per step: stdout, stderr and the exit code. At the end: a recursive walk of
`.orchestry/`, every file parsed by extension (YAML, JSON, JSONL, else raw
text) and deep-compared.

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

## Proving the gate can fail

`xtask/probe-harness.sh` introduces a known divergence, asserts the harness
catches it, and reverts — one probe per numbered PARITY requirement. It found
two real blind spots when it was first written, so run it after changing the
harness.

## Known coverage gaps

Honest limits of the current scenario set, to be closed as WP-5 lands more
commands:

- ~~**Index tie order under a re-save**~~ — **closed** by
  `index-tie-order.toml` and its `rust_then_ts` pair: `agent disable
  agt_creator` re-saves an entity with a deterministic id, so push-to-end and
  in-place update now produce different trees.
- **A step cannot name a generated id.** Steps are literal argv lines, so any
  command whose target is a `tsk_`/`agt_`/`run_` id can only be exercised
  against one the project creates deterministically — in practice
  `agt_creator`, written by `init`. That is why `task assign`, which re-saves
  a task exactly as `agent disable` re-saves an agent, has unit coverage but no
  scenario. Capturing ids from a step's output into later steps would close
  this for every id-taking command at once.
- **Stable-sort ties** in `task list` need two tasks sharing a status *and* an
  `updated_at`, which the clock makes unreliable to produce from the CLI.
- **Multi-entity index rebuild** is deliberately not exercised:
  `rebuildIndex` derives order from `fs.readdir`, so two tmpdirs can enumerate
  the same logical set differently (PARITY.md §14.4). Do not add a scenario
  that deletes `_index.json` with ≥2 entities present.
