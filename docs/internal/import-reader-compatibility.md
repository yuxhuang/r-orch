# Import reader compatibility

This is the record of what the read-only import preflight does with `config.yml` and
the five `_index.json` files, and why. It is evidence for
[issue 146](https://github.com/yuxhuang/r-orch/issues/146), not a change to the import
contract. The governing documents are the [spec](../specs/2026-11-20-sqlite-storage.md)
R65, R69, R70, R109–R112, R122–R123 and AP15 (including the vectors C-V1 to C-V14 and
X-V1 to X-V14), the [plan](../plans/2026-11-20-sqlite-storage.md) T21, and
[ADR 0017](../decisions/0017-sqlite-storage.md) decision 21.

## What is wired

`orch_infra::import::preflight` is the whole-source check that comes before every write
(R112). It now classifies the config file and all five indexed directories, on the same
bounded bytes it measured:

| Input | Classified by the preflight |
|---|---|
| `config.yml` | Missing, a stream of no document, one null or non-mapping document, two documents or more, unknown keys, the effective `run_cap`, and whether the owner must confirm preapproval |
| `tasks/_index.json`, `agents/_index.json`, `goals/_index.json`, `messages/_index.json`, `context/_index.json` | Missing or unreadable, a bad entry, a repeated identity, an entry with no file, a file the index does not list, an entry that differs from its file, and a preview of the order |

`teams/` and `runs/` have no index. Nothing in the product calls the preflight: there is
no `orch import` command, no writer and no resume, and the tests are its only callers.
The preflight creates, changes and removes nothing, and opens no path built from a value
inside a document or an index.

## Config

The contract is on `main` (PR 158), and the owner ratified its drafted rows by merging it:
T21-C1, C2, X1 and X2 as drafted (the stops stay) and T21-O1 as proposed (A: the value 1.0
ran with is carried). The effective 1.0 reader is `FileConfigStore::read_merged`; the test
`effective_config_defaults_and_r109_document_count_are_distinct` compares it with the
bounded reader on the same synthetic bytes.

| Source config | Effective legacy reader | Preflight |
|---|---|---|
| Missing | Defaults | Baseline `{}`. No problem; one `config_default` note for `run_cap` |
| Empty; spaces and newlines only; comment only | Defaults | Refuses under `documents` (observed 0), saying `expected one document, found 0`, that 1.0 read the file as its defaults, and that after taking a copy the owner can write `{}` to keep those defaults |
| Explicit `null`, `~`, a bare `---` | Defaults | Refuses as `unreadable` (a document that is not a mapping), with the same 1.0 statement |
| `42`, `[1]`, `"x"` | Defaults | As the row above |
| `{}` | Defaults | Baseline. No problem; one `config_default` note for `run_cap` |
| A tab at the start of a line | Rejects it as scanner syntax | Refuses as `unreadable`; says nothing about 1.0. A file that does not scan is never counted as a stream of no document |
| Two mappings separated by `---` | Refuses | Refuses under `documents`; says nothing about 1.0 |
| A mapping | The mapping over the defaults | The mapping over the frozen 1.0 defaults, below |
| More than 1,048,576 bytes | Not compared | Refuses under `bytes`, before documents are counted |

The file kind (R103), the byte bound, an input or output error, and the depth and node
budgets are terminal and are never read as a missing or an empty config.

**Frozen defaults.** The preflight holds its own copy of 1.0's configuration defaults
(`legacy_defaults` in `preflight/compatibility.rs`), independent of any later product
default. The file's mapping is merged over it and must then decode as the 1.0
configuration type; a mapping that does not refuses as `unreadable`.

**`run_cap`.** The report's `run_cap` is the merged `scheduling.max_concurrent_agents`,
6 unless the file sets it. When the file does not set the key, including a missing or `{}`
config, a note `config_default` (observed `6`) says that 6 is 1.0's default and not a
value the source set. It is a note and stops nothing. `run_cap` is absent from the report
if the config check did not finish.

**Unknown keys.** A key that is not in the frozen defaults is unknown, at any depth: an unknown key inside
a known section is unknown too, and `prompt` has the known children `template`,
`system_template` and `user_template`. Each unknown path is listed as a note
`unknown_config` with a dotted path, each component cut to 256 characters and escaped.
Values are never reported. `legacy_config_bytes` in the report is the size of the compact
JSON of the unknown keys. Above 65,536 bytes the check refuses under `legacy_config_bytes`
(limit 65,536), which is the `legacy.config` bound of R70.

**Preapproval.** `preapproval_requires_confirmation` is true when an agent file sets
`config.approval_policy: auto`, or omits it while the project's effective
`defaults.agent.approval_policy` is `auto` (1.0's default is `auto`). It adds one note,
`preapproval_confirmation`. This is a preview and nothing more: it sets no `preapproval`
value and the report has no such field. R93 and R133 still decide the trust: only an owner
session turns the policy on, and a command that would do it must name the policy in what
the owner confirms, because a source's agent files are input and input alone never grants
trust. That command does not exist yet.

## Indexes

Each directory with an index is reconciled one index and one entity document at a time.
An entity file's content is taken from its file, never from its index.

| Index | Result |
|---|---|
| Missing; not JSON within its bounds; a JSON object; `null` | Note `index_unreadable`; entity files are ordered by id; the check goes on |
| `[]` | A usable index that lists nothing: not `index_unreadable`; each file is noted `index_unlisted` and ordered by id |
| An array with any entry that is not an object or has no string identity (`id`, and `key` under `context/`) | Refuses `index_entry`, saying that 1.0 rebuilt such an index |
| An array of string identities with one identity more than once, whether the entries are bare identities or whole entities, and whether or not a file exists | Refuses `index_duplicate`, naming the identity (escaped, cut), saying that 1.0 used such an index as it stood only if every entry was a complete entity of the directory's type and otherwise rebuilt it |
| An entry whose file is missing or was not accepted | Note `index_missing_file`; its identity is never opened as a path |
| A file the index does not list | Note `index_unlisted`; ordered after the listed entities, in id order |
| An entry that differs from its file | Note `index_differs` |
| Over 16,777,216 bytes, nested more than 64 deep, over 1,000,000 nodes; a symbolic link, a FIFO or a file with two hard links; an input or output error | Terminal stop (`bytes`, `depth`, `nodes`, `file_kind`, `io`); never `index_unreadable` |

The array is examined whole before a class is chosen (R122): a bad entry anywhere decides
`index_entry`, and an earlier repeated identity cannot hide it. Two identities that differ
only in case are two (`A` and `a`). The preflight judges an entry by its identity alone and
decodes no entry as an entity type.

**Order preview.** `order` lists `store`, `id` and `position` for the entities the import
would order: indexed ones at their index position, the others after them. Ids are escaped
and cut like a finding. The list holds at most 1,000 entries for all stores together, and
`more` counts the rest; counts are not affected. It is a preview, not a load manifest: a
durable caller must repeat the checked reads under its own lock.

Report paths are relative to the source `.orchestry/`, and `notes` and `refusals` keep
their cap of 1,000 each.

## Named tests

`crates/orch-infra/tests/import_preflight_compat.rs` runs the real preflight on
synthetic sources in disposable directories:

| Vectors | Test |
|---|---|
| C-V2 to C-V6, C-V9, C-V12 and the C-V13 wording | `config_empty_null_and_non_mapping_diagnostics_match_the_contract` |
| C-V1, C-V7, C-V8, C-V14 | `omitted_config_run_cap_uses_frozen_legacy_default_and_a_note` |
| C-V10, X-V12, X-V14 | `budgets_and_file_kind_stops_never_become_index_unreadable` |
| C-V11 | `empty_entity_streams_keep_their_document_refusal_without_config_wording` |
| X-V4, X-V6 | `whole_array_bad_entry_precedes_duplicates_in_all_five_directories` |
| X-V5, X-V7 | `both_duplicate_shapes_and_missing_files_stop_in_all_five_directories`, which also runs 1.0's `IndexManager::read_index` on an independent copy of each case: it keeps a complete-entity duplicate and rebuilds an array of identities |
| X-V1 to X-V3, X-V8 to X-V11 | `index_order_and_named_problems_use_files_and_never_open_index_identities` |
| Unknown keys, the 64 KiB bound | `unknown_config_is_inert_bounded_and_values_are_not_reported` |
| Preapproval | `auto_agents_require_owner_confirmation_without_granting_policy` |
| Escaping and cutting | `hostile_duplicate_identity_is_escaped_and_cut_without_changing_class` |
| Entity documents decode as before | `typed_json_documents_still_refuse_duplicate_identity_members`, `typed_yaml_entities_keep_ignored_yaml_only_fields` |
| Report caps | `order_and_notes_are_capped_without_changing_counts` |

X-V13 (an injected read error and an interrupted read at the preflight) and the run of
1.0's own readers beside the import for C-V11 stay with the reader-level tests below and
the integrated acceptance. C-V6's bare `---` is now pinned by the test of C-V2 to C-V6:
the preflight's pass counts it as one null document.

## Compatibility conditions

These reader-level checks pre-date the wiring and still stand.

| Item from issue 146 | Evidence or remaining condition |
|---|---|
| I1 | The config table above pins the discrepancy; the contract on `main` disposes of it. The named-path report is the preflight's. Integrated dry-run output, including the `.orchestry/` prefix on a printed path, is the command's. |
| N1 | `physical_blank_lines_form_gaps_while_legacy_skips_unicode_whitespace` compares real `FileRunStore` reads. Empty, ASCII, U+2003 and U+00A0 lines with CRLF join adjacent malformed lines into physical gap 2–4; valid events retain lines 1 and 5. The final newline creates no extra line. The legacy reader returns two events. R111 is unchanged. |
| N2 | `index_damage_distinguishes_unreadable_streams_from_bad_array_entries` runs the bounded reader on missing, malformed JSON, object and null indexes, and on non-object entries, absent or non-string identities and duplicates, and real `IndexManager::read_index` rebuilds the first seven cases on disposable copies. That test decodes entries into a type of one field, so its "decoded duplicates survive" result does not hold for a real directory: an array of identities alone does not decode as the entity type, and 1.0 rebuilds it (spec F19). That reader-level `reconcile` stops at the first problem it meets and names a repeated
identity `index_entry`, so its class depends on the order of the entries; the preflight does
not call it and follows R122 and R123 instead (`index_entry` first, then `index_duplicate`),
as the preflight tests above show. Existing corpus tests retain stale-file content and distinct legacy/import order checks. |
| N3 | Named `JSON_DEPTH_CLAMP = 64`, pinned at 64/65 even when the requested limit is 100. No limit was raised. |
| N4 | `interrupted_buffered_reads_retry_and_terminal_errors_end_the_stream` injects one Interrupted after a partial buffered line and verifies both events survive. A PermissionDenied at the same position yields an I/O refusal and terminates, rather than a gap. Document `read_to_end` retries Interrupted and propagates terminal failure too. |
| N5 | Existing `depth_bombs_are_refused_on_a_small_stack` pins the YAML parser's depth classification. Its scanner's "recursion limit exceeded" wording is still a dependency; parser updates must preserve that test or deliberately review the classification. |
| N6 | The preflight acquires and budgets every document on one blocking thread, never on an async executor thread, and a flag stops it between reads. It does not prove cancellation of a blocked kernel read, or that a caller holds the 1.0 lock for as long as it relies on the answer; both wait for the first importer caller (issue 192). |
| N7 | Existing generated corpus test parses 50 documents and 76 event lines and verifies 73 files unchanged. Existing typed Task, Agent, state and RunEvent examples remain. Production-limit child outcomes and RSS are in `source_budget`. Integrated dry run, resume, writer and no-write acceptance remain T21 acceptance work (issue 146). No live project was read. |

## Still open

- The importer, the `orch import` command, the writer, dry run and resume. Integrated
  dry-run/import parity and zero writes on refusal are acceptance of issue 146, not of
  this check.
- The lock held across the check and cancellation of a blocked read (issue 192).
- The 1.0 readers run beside the preflight for the entity files of C-V11, and the
  injected read errors (X-V13).
- The reader-level `import::index::reconcile` and its test are an older, unwired
  classifier that disagrees with the contract on a repeated identity. Until it is removed
  or routed through the preflight's rules, two classifiers exist.

Rollback is reverting the wiring. There is no database migration, backfill, source
transformation or format cutover, and no new command.
