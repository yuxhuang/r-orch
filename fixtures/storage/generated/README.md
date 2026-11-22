# Generated 1.0 import fixtures

Do not edit by hand. These directories are made by `cargo xtask fixture-gen` (spec R90, O10): a seeded
script drives the real 1.0 file backend in a private temporary directory, so the layout, the index
order and the event files are what 1.0 writes. Every value is invented, and the clock and the ids
come from the seed. Nothing was read from a real project.

`cargo xtask fixture-gen --check` regenerates the corpus privately and compares exact file sets
and bytes with this directory without changing it. `cargo xtask fixture-gen` rewrites it.

Each scenario holds a project directory: its `.orchestry/` is a complete 1.0 data directory with
nothing running (a running entry names a process; the import refuses a live one).

| Directory | Seed | Contents |
|---|---|---|
| `small/` | 1 | four agents, two goals, one team, twelve tasks with their runs and events, two deleted tasks, six messages, three context entries, a template and one attachment |

The hand-written conformance corpus is `fixtures/storage-v1/`. Large corpora for the importer's
bounds are made on demand by its tests and are not checked in.
