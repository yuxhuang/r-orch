# Generated 1.0 import fixtures

Do not edit by hand. These directories are made by `cargo xtask fixture-gen` (spec R90, O10): a seeded
script drives the real 1.0 file backend in a private temporary directory, so the layout, the index
order and the event files are what 1.0 writes. Every value is invented, and the clock and the ids
come from the seed. Nothing was read from a real project.

`cargo xtask fixture-gen --check` regenerates the corpus privately and compares exact file sets
and bytes with this directory without changing it. `cargo xtask fixture-gen` rewrites it. A nonempty
destination is replaced only when `.orch-fixture-gen` contains exactly `orch fixture-gen corpus v1`
followed by a newline. README.md alone does not grant replacement permission.

Each scenario holds synthetic records written through the 1.0 file stores, with nothing running.
This is a small serialization corpus, not a complete live-project or importer acceptance fixture.
Where it differs from what a 1.0 daemon leaves behind, a test written against it must not assume the
corpus is typical:

* Run `workspace_path` values are invented relative paths (`workspaces/<task-id>`). 1.0 writes an
  absolute path or `remote:<alias>`. No `workspaces/` directory and no repository exists.
* No task sets `workspace`, `proof`, `review_results`, `feedback`, `scope` or `workspace_mode`.
* No task is `in_progress` and no run is `running`: `state.json` holds no running or claimed
  entry (it does hold one retry-queue entry). Finished runs carry invented pids, which are not
  evidence of a process and may coincide with a live one on the machine that reads them.
* Every `expires_at` and TTL falls in January 2025, so against the wall clock all of them have
  expired. Tests that read the corpus must fix the clock at the generator's start (`START_MS`).
* Runtime statistics are a fixed 60 000 ms per run, which does not match the runs' own timestamps.
* No agent has an explicit `approval_policy: auto`. Only the config default is `auto`, so the
  per-project pre-approval rule (decision 14) is reached through the effective policy alone.

Live process identity, workspace ownership, hostile documents, import bounds, dry run and resume
are not exercised here. Those checks remain in T21.

| Directory | Seed | Contents |
|---|---|---|
| `small/` | 1 | four agents, two goals, one team, twelve tasks with their runs and events, two deleted tasks, six messages, three context entries, a template and one attachment |

The hand-written conformance corpus is `fixtures/storage-v1/`. Large corpora for the importer's
bounds remain work for T21 and are not checked in.
