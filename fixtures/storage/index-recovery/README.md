# Damaged-index corpus for the 1.0 import

Hand-written cases for spec R65, R69, R122 and R123: a task directory whose `_index.json` is stale, absent,
truncated, or valid JSON that is not an array. Every value is invented. Nothing was read from a real project.

Each case directory holds a `source/` project root (a 1.0 `.orchestry/`) and, beside it and outside it, an
`expected.json`. The importer reads `source/` and never `expected.json`.

| Case | `_index.json` | Entity files |
|---|---|---|
| `index-absent` | none | `tsk_idx_a`, `tsk_idx_b`, `tsk_idx_c` |
| `index-malformed` | cut off in the middle of its second entry | same three |
| `index-object` | valid JSON, an object with a `tasks` array that lists only `tsk_idx_c` | same three |
| `index-null` | valid JSON, `null` | same three |
| `index-disagrees` | the stale-entry case of `fixtures/storage-v1/index-disagrees`, used by reference (`source` in its `expected.json`); it is not copied | |

The ids sort in the reverse of creation order, so an importer that follows creation time or file creation
order fails where one that follows id order passes. Each file's title names its case, so a payload read
from another case or from an index's stale copy is visible.

`expected.json` keeps two facts apart:

- `legacy_1_0`: what 1.0's real file reader returns. `crates/orch-infra/tests/import_index_reference.rs`
  establishes it with `FileTaskStore` on a copy. A rebuilt list has **no specified order**: 1.0 rebuilds in
  directory enumeration order, so only the sorted membership is recorded. 1.0 also *publishes* the rebuilt
  index as a side effect of a read, which is why no test opens this directory in place.
- `importer_1_1`: what the 1.1 importer must produce: `task_order` (R122: an unusable index lists nothing, so
  id order), each task's payload (the entity file's, never an index's), and the `report` classes. A class
  the spec names (`index_differs`, `index_unreadable`) is marked `named_in_spec: true`; the other two are the
  spec's descriptions of a row ("an entity its index does not list", "an index entry with no file"), which has
  no identifier yet. Every class goes on, as the "Import problem classes" table says. The `index-disagrees`
  expectation puts the index-listed entity first and the unlisted one after it ("Order").

Open for T21: "id order" is byte order of the id in these cases (all ids are lower case after `tsk_`); the
spec does not say whether a mixed-case id sorts by bytes or by locale, and no case pins it.

This corpus holds no importer assertion. T21 owns those and may read `expected.json` for them. The generated
corpus is `fixtures/storage/generated/`, made by another author's tool; do not edit it from here.
