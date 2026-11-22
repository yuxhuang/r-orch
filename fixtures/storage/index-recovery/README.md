# Damaged-index corpus for the 1.0 import

Hand-written cases for spec R65, R69, R122 and R123: a task directory whose `_index.json` is stale, absent,
truncated, or valid JSON that is not an array. Every value is invented. Nothing was read from a real project.

Each case directory holds a `source/` project root (a 1.0 `.orchestry/`) and, beside it and outside it, an
`expected.json`. The importer reads `source/` and never `expected.json`. The `source` entry of an `expected.json` is
`source` or a relative reference that, once resolved, stays inside `fixtures/` and holds a `.orchestry/`; the test
refuses any other (an absolute path, a path out of `fixtures/`, a missing directory).

| Case | `_index.json` | Entity files |
|---|---|---|
| `index-absent` | none | `tsk_idx_a`, `tsk_idx_b`, `tsk_idx_c` |
| `index-absent-update-order` | none | same three, with creation order, update order and id order all different (below) |
| `index-malformed` | cut off in the middle of its second entry | same three |
| `index-object` | valid JSON, an object with a `tasks` array that lists only `tsk_idx_c` | same three |
| `index-null` | valid JSON, `null` | same three |
| `index-disagrees` | the stale-entry case of `fixtures/storage-v1/index-disagrees`, used by reference (`source` in its `expected.json`); it is not copied | |

In the first four cases the ids sort in the reverse of creation order, so an importer that follows creation time
fails where one that follows id order passes. Each file's title names its case, so a payload read from another
case or from an index's stale copy is visible.

Those four cases cannot tell id order from 1.0's own `list()` order, because their `updated_at` values also run
the opposite way round, newest update first, which is id order. `index-absent-update-order` separates all three:

| Order | `tsk_idx_a` | `tsk_idx_b` | `tsk_idx_c` |
|---|---|---|---|
| id (what R122 imports) | 1st | 2nd | 3rd |
| creation (`created_at` ascending) | 3rd | 1st | 2nd |
| 1.0 `list()` (`updated_at` descending) | 2nd | 3rd | 1st |

An importer that copies the order of 1.0's reader, or follows creation time, fails this case. The corpus makes no
claim about the order in which the files were written to disk or checked out.

`expected.json` keeps two facts apart:

- `legacy_1_0`: what 1.0's real file reader returns. `crates/orch-infra/tests/import_index_reference.rs`
  establishes it with `FileTaskStore` on a copy. `list()` is specified: it sorts by status, then by `updated_at`
  newest first, and a tie keeps the order of the index it read. In every rebuilt case the statuses are equal and
  the `updated_at` values differ, so `list_order` is recorded and asserted exactly. A different fact is the
  *published* index: 1.0 publishes the rebuilt index as a side effect of a read, in directory enumeration order,
  which nothing specifies, so only its membership is asserted. That side effect is also why no test opens this
  directory in place.
- `importer_1_1`: what the 1.1 importer must produce: `task_order`, each task's payload (the entity file's, never
  an index's), and the `report`. The test derives `task_order` and the whole `report` from the files in `source/`
  and compares them exactly: each finding's class and subject, how many of each, and nothing missing or extra.
  `task_order` is the array order of a usable `_index.json` (entries with no file skipped), followed by the files
  it does not list in id order; an unusable index lists nothing, so every file in id order (R122). A class the
  spec names (`index_differs`, `index_unreadable`) is marked `named_in_spec: true`; the other two are the
  spec's descriptions of a row ("an entity its index does not list", "an index entry with no file"), which has
  no identifier yet. Every class goes on, as the "Import problem classes" table says. The `index-disagrees`
  expectation puts the index-listed entity first and the unlisted one after it ("Order").

An entry is `index_differs` when its object is not equal to the entity file's object: R122 says only that the two
"disagree". The index does not decide anything else.

## R122 and R123

R122 and R123 meet here. R123 says "a document that the importer cannot read shall stop it", and the first row of
the "Import problem classes" table stops on a document that does not parse. R122 is the specific rule for an
`_index.json` that is missing, not an array or does not parse: that is an index that lists nothing, listed as
`index_unreadable`, and the import goes on. This corpus follows R122, since `_index.json` is not an entity
document. R123's stop applies to an *entity* file that does not parse, which this corpus does not hold; the test
treats one as an error in the corpus. The spec is not changed here.

Open for T21: "id order" is byte order of the id in these cases (all ids are lower case after `tsk_`); the
spec does not say whether a mixed-case id sorts by bytes or by locale, and no case pins it.

This corpus holds no importer assertion. T21 owns those and may read `expected.json` for them. The generated
corpus is `fixtures/storage/generated/`, made by another author's tool; do not edit it from here.
